#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use base64::{engine::general_purpose::STANDARD, Engine};
use serde::Serialize;
use std::{
    collections::HashMap,
    os::windows::process::CommandExt,
    path::Path,
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering::Relaxed},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tauri::{Emitter, Manager, PhysicalPosition, WebviewWindow};
use windows::{
    core::HSTRING,
    Devices::{
        Radios::RadioAccessStatus,
        WiFi::{WiFiAccessStatus, WiFiAdapter, WiFiConnectionStatus, WiFiReconnectionKind},
    },
    Networking::Connectivity::NetworkAuthenticationType,
    Security::Credentials::PasswordCredential,
};
use windows::{
    Win32::{
        Foundation::RECT,
        UI::WindowsAndMessaging::{
            GetClassNameW, GetForegroundWindow, GetWindowRect, IsIconic, IsWindowVisible,
        },
    },
    Devices::{
        Bluetooth::{BluetoothConnectionStatus, BluetoothDevice, BluetoothLEDevice},
        Enumeration::DeviceInformation,
        Radios::{Radio, RadioKind, RadioState},
    },
    Networking::Connectivity::{NetworkConnectivityLevel, NetworkInformation},
    Media::Control::{
        GlobalSystemMediaTransportControlsSession as Session,
        GlobalSystemMediaTransportControlsSessionManager as SessionManager,
        GlobalSystemMediaTransportControlsSessionMediaProperties as Props,
        GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
    },
    Storage::Streams::DataReader,
    Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED},
};

// Ukuran (px logis) -- samakan dengan CSS di ui/index.html
const HOT_HALF_WIDTH: f64 = 155.0; // zona pemicu kiri/kanan dari tengah
const HOT_HEIGHT: f64 = 6.0; // zona pemicu dari tepi atas layar
const PANEL_WIDTH: f64 = 530.0; // lebar panel terbuka (+ telinga)
const PANEL_HEIGHT: f64 = 215.0;
const CLOSE_DELAY: Duration = Duration::from_millis(300);
const POLL: Duration = Duration::from_millis(60);
// Kursor harus diam di tepi atas sebentar sebelum notch terbuka, supaya klik tab
// browser di tengah atas layar tidak ikut memicunya. Lebih lama saat notch tersembunyi.
const DWELL: Duration = Duration::from_millis(150);
const DWELL_GHOST: Duration = Duration::from_millis(500);

static OPEN: AtomicBool = AtomicBool::new(false);
static GHOST: AtomicBool = AtomicBool::new(false); // notch sedang disembunyikan
static SYS_ON: AtomicBool = AtomicBool::new(false); // tab Sistem sedang dilihat
static PINNED: AtomicBool = AtomicBool::new(false); // true saat mengetik catatan

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![media_cmd, launch, open_path, set_pin, set_ghost, media_seek, set_sys,
            wifi_list, wifi_connect, wifi_disconnect, radio_set, bt_list, bt_toggle, quit,
            autostart_get, autostart_set, drag_out])
        .setup(|app| {
            let win = app.get_webview_window("main").expect("window main");
            place_top_center(&win);
            let _ = win.set_ignore_cursor_events(true);
            let _ = win.show();

            let w = win.clone();
            thread::spawn(move || watch_cursor(w));
            let w = win.clone();
            thread::spawn(move || media_thread(w));
            let w = win.clone();
            thread::spawn(move || conn_thread(w));
            let w = win.clone();
            thread::spawn(move || sys_thread(w));
            // Jika mulai otomatis aktif, perbarui lokasi exe di registry (mis. setelah file dipindah)
            thread::spawn(|| {
                if !cfg!(debug_assertions) && autostart_enabled() {
                    autostart_write();
                }
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("gagal menjalankan NotchNook");
}

fn place_top_center(win: &WebviewWindow) {
    if let (Ok(Some(m)), Ok(size)) = (win.primary_monitor(), win.outer_size()) {
        let x = m.position().x + (m.size().width as i32 - size.width as i32) / 2;
        let _ = win.set_position(PhysicalPosition::new(x, m.position().y));
    }
}

fn set_open(win: &WebviewWindow, open: bool) {
    OPEN.store(open, Relaxed);
    let _ = win.set_ignore_cursor_events(!open);
    let _ = win.emit("notch-open", open);
}

fn watch_cursor(win: WebviewWindow) {
    let mut open = false;
    let mut left_at: Option<Instant> = None;
    let mut hot_since: Option<Instant> = None;
    let mut tick = 0u32;
    let mut covered_last: Option<bool> = None;

    loop {
        thread::sleep(POLL);

        // Tiap ~0,5 dtk: cek apakah jendela aktif menutupi area notch
        tick = tick.wrapping_add(1);
        if tick % 8 == 0 {
            if let Some(c) = notch_covered(&win) {
                if covered_last != Some(c) || tick < 100 {
                    covered_last = Some(c);
                    let _ = win.emit("covered", c);
                }
            }
        }

        let (Ok(c), Ok(pos), Ok(size)) =
            (win.cursor_position(), win.outer_position(), win.outer_size())
        else {
            continue;
        };
        let s = win.scale_factor().unwrap_or(1.0);
        let (px, py) = (pos.x as f64, pos.y as f64);
        let cx = px + size.width as f64 / 2.0;

        let in_hot = (c.x - cx).abs() <= HOT_HALF_WIDTH * s
            && c.y >= py - 1.0
            && c.y <= py + HOT_HEIGHT * s;
        let in_panel = (c.x - cx).abs() <= PANEL_WIDTH / 2.0 * s
            && c.y >= py
            && c.y <= py + PANEL_HEIGHT * s;

        if !open {
            if in_hot {
                let t = *hot_since.get_or_insert_with(Instant::now);
                let dwell = if GHOST.load(Relaxed) { DWELL_GHOST } else { DWELL };
                if t.elapsed() >= dwell {
                    open = true;
                    left_at = None;
                    hot_since = None;
                    set_open(&win, true);
                }
            } else {
                hot_since = None;
            }
        } else if in_panel || PINNED.load(Relaxed) {
            left_at = None;
        } else {
            let t = *left_at.get_or_insert_with(Instant::now);
            if t.elapsed() >= CLOSE_DELAY {
                open = false;
                left_at = None;
                set_open(&win, false);
            }
        }
    }
}

/// true bila jendela aktif (bukan desktop/taskbar) menimpa area notch.
/// None = tidak bisa ditentukan (mis. jendela notch sendiri yang aktif) -> pakai status sebelumnya.
fn notch_covered(win: &WebviewWindow) -> Option<bool> {
    let (pos, size) = (win.outer_position().ok()?, win.outer_size().ok()?);
    let s = win.scale_factor().unwrap_or(1.0);
    let cx = pos.x as f64 + size.width as f64 / 2.0;
    let (l, r) = ((cx - 155.0 * s) as i32, (cx + 155.0 * s) as i32);
    let (t, b) = (pos.y, pos.y + (34.0 * s) as i32);
    unsafe {
        let fg = GetForegroundWindow();
        if fg.0.is_null() {
            return Some(false);
        }
        if let Ok(own) = win.hwnd() {
            if own.0 == fg.0 {
                return None;
            }
        }
        if IsIconic(fg).as_bool() || !IsWindowVisible(fg).as_bool() {
            return Some(false);
        }
        let mut buf = [0u16; 64];
        let n = GetClassNameW(fg, &mut buf).max(0) as usize;
        let class = String::from_utf16_lossy(&buf[..n]);
        if matches!(class.as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd") {
            return Some(false);
        }
        let mut rc = RECT::default();
        GetWindowRect(fg, &mut rc).ok()?;
        Some(rc.left < r && rc.right > l && rc.top < b && rc.bottom > t)
    }
}

// ---------- Media (Windows SMTC) ----------

#[derive(Clone, PartialEq, Serialize)]
struct MediaInfo {
    title: String,
    artist: String,
    playing: bool,
    thumb: String,
}

fn session() -> windows::core::Result<Session> {
    SessionManager::RequestAsync()?.get()?.GetCurrentSession()
}

fn read_thumb(props: &Props) -> windows::core::Result<String> {
    let stream = props.Thumbnail()?.OpenReadAsync()?.get()?;
    let size = stream.Size()? as u32;
    if size == 0 || size > 4_000_000 {
        return Ok(String::new());
    }
    let reader = DataReader::CreateDataReader(&stream)?;
    reader.LoadAsync(size)?.get()?;
    let mut buf = vec![0u8; size as usize];
    reader.ReadBytes(&mut buf)?;
    Ok(format!("data:image/png;base64,{}", STANDARD.encode(buf)))
}

#[derive(Clone, PartialEq, Serialize)]
struct MediaTime {
    pos: u64, // ms
    dur: u64, // ms
}

fn read_time(s: &Session, playing: bool) -> Option<MediaTime> {
    let t = s.GetTimelineProperties().ok()?;
    let start = t.StartTime().ok()?.Duration;
    let end = t.EndTime().ok()?.Duration;
    let mut pos = t.Position().ok()?.Duration;
    if end <= start {
        return None; // siaran langsung / tanpa durasi
    }
    if playing {
        // Posisi dari aplikasi adalah cuplikan pada LastUpdatedTime, jadi tambahkan selisihnya
        if let Ok(lu) = t.LastUpdatedTime() {
            let now = (SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_nanos() / 100) as i64
                + 116_444_736_000_000_000; // 1601 -> 1970 dalam satuan 100 ns
            let dt = now - lu.UniversalTime;
            if dt > 0 && dt < 36_000_000_000 {
                pos += dt;
            }
        }
    }
    let pos = pos.clamp(start, end);
    Some(MediaTime { pos: ((pos - start) / 10_000) as u64, dur: ((end - start) / 10_000) as u64 })
}

fn read_info(key: &mut (String, String), thumb: &mut String) -> Option<(MediaInfo, Option<MediaTime>)> {
    let s = session().ok()?;
    let props = s.TryGetMediaPropertiesAsync().ok()?.get().ok()?;
    let title = props.Title().ok()?.to_string();
    let artist = props.Artist().map(|a| a.to_string()).unwrap_or_default();
    let playing = s.GetPlaybackInfo().ok()?.PlaybackStatus().ok()? == Status::Playing;

    let k = (title.clone(), artist.clone());
    if k != *key {
        *thumb = read_thumb(&props).unwrap_or_default();
        *key = k;
    }
    let t = if OPEN.load(Relaxed) { read_time(&s, playing) } else { None };
    Some((MediaInfo { title, artist, playing, thumb: thumb.clone() }, t))
}

fn media_thread(win: WebviewWindow) {
    unsafe {
        let _ = RoInitialize(RO_INIT_MULTITHREADED);
    }
    let mut key = (String::new(), String::new());
    let mut thumb = String::new();
    let mut last: Option<MediaInfo> = None;
    let mut last_t: Option<Option<MediaTime>> = None;
    let (mut waited, mut was_open) = (99u32, false);
    loop {
        thread::sleep(Duration::from_millis(200));
        waited += 1;
        let open = OPEN.load(Relaxed);
        let opened_now = open && !was_open;
        was_open = open;
        // Terbuka: ~0,8 dtk (untuk bar progres). Tertutup: ~2,4 dtk (hemat CPU)
        if !(opened_now || (open && waited >= 4) || (!open && waited >= 12)) {
            continue;
        }
        waited = 0;
        let (cur, t) = match read_info(&mut key, &mut thumb) {
            Some((i, t)) => (Some(i), t),
            None => (None, None),
        };
        if cur != last {
            let _ = win.emit("media", cur.clone());
            last = cur;
        }
        if open && (opened_now || last_t.as_ref() != Some(&t)) {
            let _ = win.emit("mtime", t.clone());
            last_t = Some(t);
        }
    }
}

#[tauri::command]
async fn media_cmd(action: String) {
    unsafe {
        let _ = RoInitialize(RO_INIT_MULTITHREADED);
    }
    if let Ok(s) = session() {
        match action.as_str() {
            "toggle" => drop(s.TryTogglePlayPauseAsync()),
            "next" => drop(s.TrySkipNextAsync()),
            "prev" => drop(s.TrySkipPreviousAsync()),
            _ => {}
        }
    }
}

// ---------- Wi-Fi & Bluetooth ----------

#[derive(Clone, PartialEq, Serialize)]
struct NetInfo {
    kind: String, // "wifi" | "wired" | "none"
    name: String,
    bars: u8, // 0-4
    internet: bool,
}

#[derive(Clone, PartialEq, Serialize)]
struct BtDev {
    name: String,
    battery: Option<u8>, // persen; None bila perangkat tidak melaporkan baterai
}

#[derive(Clone, PartialEq, Serialize)]
struct BtInfo {
    available: bool,
    on: bool,
    devices: Vec<BtDev>,
}

#[derive(Clone, PartialEq, Serialize)]
struct Conn {
    net: NetInfo,
    bt: BtInfo,
}

fn read_net() -> NetInfo {
    let mut info = NetInfo { kind: "none".into(), name: String::new(), bars: 0, internet: false };
    let Ok(list) = NetworkInformation::GetConnectionProfiles() else { return info };
    for p in &list {
        let Ok(level) = p.GetNetworkConnectivityLevel() else { continue };
        if level == NetworkConnectivityLevel::None {
            continue;
        }
        let wlan = p.IsWlanConnectionProfile().unwrap_or(false);
        if !wlan && info.kind != "none" {
            continue;
        }
        info.kind = String::from(if wlan { "wifi" } else { "wired" });
        info.internet = level == NetworkConnectivityLevel::InternetAccess;
        info.name = p.ProfileName().map(|n| n.to_string()).unwrap_or_default();
        if wlan {
            if let Ok(d) = p.WlanConnectionProfileDetails() {
                if let Ok(ssid) = d.GetConnectedSsid() {
                    if !ssid.is_empty() {
                        info.name = ssid.to_string();
                    }
                }
            }
            info.bars = p.GetSignalBars().ok().and_then(|r| r.Value().ok()).unwrap_or(0);
            break; // Wi-Fi diprioritaskan
        }
    }
    info
}

// Level baterai TWS/headset (HFP). Windows menyimpannya pada node layanan "<nama> Hands-Free AG"
// (enumerator BTHENUM), bukan pada perangkat induknya, sebagai properti BYTE
// {104EA319-6EE2-4701-BD47-8DDBF425BBE5} #2. Dibaca lewat cfgmgr32 (tanpa library tambahan).
#[repr(C)]
struct Guid {
    d1: u32,
    d2: u16,
    d3: u16,
    d4: [u8; 8],
}
#[repr(C)]
struct DevPropKey {
    fmtid: Guid,
    pid: u32,
}
#[link(name = "cfgmgr32")]
extern "system" {
    fn CM_Get_Device_ID_List_SizeW(len: *mut u32, filter: *const u16, flags: u32) -> u32;
    fn CM_Get_Device_ID_ListW(filter: *const u16, buffer: *mut u16, len: u32, flags: u32) -> u32;
    fn CM_Locate_DevNodeW(devinst: *mut u32, id: *const u16, flags: u32) -> u32;
    fn CM_Get_DevNode_PropertyW(devinst: u32, key: *const DevPropKey, ty: *mut u32, buf: *mut u8, size: *mut u32, flags: u32) -> u32;
}

/// Kunci: nama perangkat Bluetooth (mis. "TWS"), nilai: persen baterai.
fn bt_batteries() -> HashMap<String, u8> {
    let mut map = HashMap::new();
    let filter: Vec<u16> = "BTHENUM\0".encode_utf16().collect();
    let bat_key = DevPropKey {
        fmtid: Guid { d1: 0x104E_A319, d2: 0x6EE2, d3: 0x4701, d4: [0xBD, 0x47, 0x8D, 0xDB, 0xF4, 0x25, 0xBB, 0xE5] },
        pid: 2,
    };
    let name_key = DevPropKey {
        // DEVPKEY_Device_FriendlyName
        fmtid: Guid { d1: 0xA45C_254E, d2: 0xDF1C, d3: 0x4EFD, d4: [0x80, 0x20, 0x67, 0xD1, 0x46, 0xA8, 0x50, 0xE0] },
        pid: 14,
    };
    unsafe {
        let flags = 0x101; // FILTER_ENUMERATOR | FILTER_PRESENT
        let mut len = 0u32;
        if CM_Get_Device_ID_List_SizeW(&mut len, filter.as_ptr(), flags) != 0 || len < 2 {
            return map;
        }
        let mut buf = vec![0u16; len as usize];
        if CM_Get_Device_ID_ListW(filter.as_ptr(), buf.as_mut_ptr(), len, flags) != 0 {
            return map;
        }
        for id in buf.split(|&c| c == 0).filter(|s| !s.is_empty()) {
            let mut wid: Vec<u16> = id.to_vec();
            wid.push(0);
            let mut dev = 0u32;
            if CM_Locate_DevNodeW(&mut dev, wid.as_ptr(), 0) != 0 {
                continue;
            }
            // Baterai: tipe DEVPROP_TYPE_BYTE (3)
            let (mut ty, mut size, mut level) = (0u32, 4u32, [0u8; 4]);
            if CM_Get_DevNode_PropertyW(dev, &bat_key, &mut ty, level.as_mut_ptr(), &mut size, 0) != 0
                || ty != 3
                || level[0] > 100
            {
                continue;
            }
            // Nama: tipe DEVPROP_TYPE_STRING (0x12), UTF-16
            let (mut nty, mut nsize, mut nb) = (0u32, 512u32, [0u8; 512]);
            if CM_Get_DevNode_PropertyW(dev, &name_key, &mut nty, nb.as_mut_ptr(), &mut nsize, 0) != 0 {
                continue;
            }
            let units: Vec<u16> = nb[..(nsize as usize).min(512)]
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .take_while(|&c| c != 0)
                .collect();
            let full = String::from_utf16_lossy(&units);
            let base = full
                .strip_suffix(" Hands-Free AG")
                .or_else(|| full.strip_suffix(" Hands-Free"))
                .unwrap_or(&full)
                .to_string();
            if !base.is_empty() {
                map.insert(base, level[0]);
            }
        }
    }
    map
}

fn read_bt() -> BtInfo {
    let mut bt = BtInfo { available: false, on: false, devices: vec![] };
    if let Ok(radios) = Radio::GetRadiosAsync().and_then(|a| a.get()) {
        for r in &radios {
            if r.Kind().ok() == Some(RadioKind::Bluetooth) {
                bt.available = true;
                bt.on = r.State().ok() == Some(RadioState::On);
            }
        }
    }
    if bt.on {
        // Perangkat klasik (headset, speaker) dan BLE (keyboard, mouse modern)
        let mut names: Vec<String> = Vec::new();
        let selectors = [
            BluetoothDevice::GetDeviceSelectorFromConnectionStatus(BluetoothConnectionStatus::Connected),
            BluetoothLEDevice::GetDeviceSelectorFromConnectionStatus(BluetoothConnectionStatus::Connected),
        ];
        for sel in selectors.into_iter().flatten() {
            if let Ok(found) = DeviceInformation::FindAllAsyncAqsFilter(&sel).and_then(|a| a.get()) {
                for d in &found {
                    if let Ok(n) = d.Name() {
                        let n = n.to_string();
                        if !n.is_empty() && !names.contains(&n) {
                            names.push(n);
                        }
                    }
                }
            }
        }
        // Baterai (mis. TWS) hanya dicari bila ada perangkat tersambung
        let bats = if names.is_empty() { HashMap::new() } else { bt_batteries() };
        bt.devices = names
            .into_iter()
            .take(6)
            .map(|name| {
                let battery = bats.get(&name).copied().or_else(|| {
                    bats.iter()
                        .find(|(k, _)| k.starts_with(name.as_str()) || name.starts_with(k.as_str()))
                        .map(|(_, v)| *v)
                });
                BtDev { name, battery }
            })
            .collect();
    }
    bt
}

fn conn_thread(win: WebviewWindow) {
    unsafe {
        let _ = RoInitialize(RO_INIT_MULTITHREADED);
    }
    let (mut waited, mut was_open) = (99u32, false);
    let mut last: Option<Conn> = None;
    loop {
        thread::sleep(Duration::from_secs(1));
        waited += 1;
        let open = OPEN.load(Relaxed);
        let opened_now = open && !was_open;
        was_open = open;
        // Terbuka: cek tiap 5 dtk. Tertutup: tiap 8 dtk (cukup untuk pop-up perangkat baru)
        if !(opened_now || (open && waited >= 5) || (!open && waited >= 8)) {
            continue;
        }
        waited = 0;
        let cur = Conn { net: read_net(), bt: read_bt() };
        if opened_now || last.as_ref() != Some(&cur) {
            let _ = win.emit("conn", cur.clone());
        }
        last = Some(cur);
    }
}

// ---------- Launcher, shelf, pin ----------

#[tauri::command]
fn launch(id: String) {
    // Daftar putih: frontend hanya boleh memilih id, bukan perintah bebas
    let target = match id.as_str() {
        "calc" => "calc",
        "notepad" => "notepad",
        "explorer" => "explorer",
        "terminal" => "wt",
        "settings" => "ms-settings:",
        "wifi" => "ms-settings:network-wifi",
        "bluetooth" => "ms-settings:bluetooth",
        "snip" => "ms-screenclip:",
        _ => return,
    };
    let _ = Command::new("cmd")
        .args(["/C", "start", "", target])
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .spawn();
}

#[tauri::command]
fn open_path(path: String) {
    if Path::new(&path).exists() {
        let _ = Command::new("explorer").arg(&path).spawn();
    }
}

#[tauri::command]
fn set_pin(pinned: bool) {
    PINNED.store(pinned, Relaxed);
}

#[tauri::command]
fn set_ghost(on: bool) {
    GHOST.store(on, Relaxed);
}

#[tauri::command]
async fn media_seek(ms: u64) {
    unsafe {
        let _ = RoInitialize(RO_INIT_MULTITHREADED);
    }
    if let Ok(s) = session() {
        let start = s
            .GetTimelineProperties()
            .ok()
            .and_then(|t| t.StartTime().ok())
            .map(|t| t.Duration)
            .unwrap_or(0);
        drop(s.TryChangePlaybackPositionAsync(start + ms as i64 * 10_000));
    }
}

#[tauri::command]
fn set_sys(on: bool) {
    SYS_ON.store(on, Relaxed);
}

// ---------- Monitor sistem (CPU, RAM, jaringan) ----------
// Memakai API Windows langsung lewat FFI supaya tanpa library tambahan.

#[repr(C)]
struct MemStatus {
    len: u32,
    load: u32,
    total: u64,
    avail: u64,
    total_pf: u64,
    avail_pf: u64,
    total_virt: u64,
    avail_virt: u64,
    avail_ext: u64,
}

#[link(name = "kernel32")]
extern "system" {
    fn GetSystemTimes(idle: *mut u64, kernel: *mut u64, user: *mut u64) -> i32;
    fn GlobalMemoryStatusEx(m: *mut MemStatus) -> i32;
}
#[link(name = "iphlpapi")]
extern "system" {
    fn GetIfTable(table: *mut u8, size: *mut u32, order: i32) -> u32;
}

#[derive(Clone, Serialize)]
struct Sys {
    cpu: u8,
    ram: u8,
    used_mb: u64,
    total_mb: u64,
    down: u64, // byte/detik
    up: u64,
}

/// Penghitung byte per adapter (MIB_IFROW = 860 byte; Type @516, OperStatus @544, InOctets @552, OutOctets @576)
fn net_counters() -> HashMap<u32, (u32, u32)> {
    let mut out = HashMap::new();
    unsafe {
        let mut size = 0u32;
        GetIfTable(std::ptr::null_mut(), &mut size, 0);
        if size < 4 {
            return out;
        }
        let mut buf = vec![0u8; size as usize];
        if GetIfTable(buf.as_mut_ptr(), &mut size, 0) != 0 {
            return out;
        }
        let rd = |o: usize| u32::from_le_bytes([buf[o], buf[o + 1], buf[o + 2], buf[o + 3]]);
        let n = rd(0) as usize;
        for i in 0..n {
            let o = 4 + i * 860;
            if o + 860 > buf.len() {
                break;
            }
            let (idx, typ, oper) = (rd(o + 512), rd(o + 516), rd(o + 544));
            if typ == 24 || oper < 4 {
                continue; // loopback atau adapter tidak aktif
            }
            out.insert(idx, (rd(o + 552), rd(o + 576)));
        }
    }
    out
}

fn sys_thread(win: WebviewWindow) {
    let mut prev_cpu: Option<(u64, u64, u64)> = None;
    let mut prev_net: Option<(Instant, HashMap<u32, (u32, u32)>)> = None;
    loop {
        thread::sleep(Duration::from_secs(1));
        // Hanya bekerja saat notch terbuka di tab Sistem
        if !(OPEN.load(Relaxed) && SYS_ON.load(Relaxed)) {
            prev_cpu = None;
            prev_net = None;
            continue;
        }
        let (mut i, mut k, mut u) = (0u64, 0u64, 0u64);
        unsafe {
            GetSystemTimes(&mut i, &mut k, &mut u);
        }
        let (now, now_net) = (Instant::now(), net_counters());
        let (Some(pc), Some((t0, old))) = (prev_cpu, prev_net.take()) else {
            prev_cpu = Some((i, k, u));
            prev_net = Some((now, now_net));
            continue; // sampel pertama hanya untuk dasar perbandingan
        };
        prev_cpu = Some((i, k, u));
        prev_net = Some((now, now_net.clone()));

        let (di, dk, du) = (i.wrapping_sub(pc.0), k.wrapping_sub(pc.1), u.wrapping_sub(pc.2));
        let total = dk + du; // waktu kernel sudah termasuk idle
        let cpu = if total == 0 { 0 } else { 100u64.saturating_sub(100 * di / total).min(100) as u8 };

        // Jaringan: ambil adapter tersibuk (menghindari hitungan ganda adapter virtual WSL/Docker)
        let secs = now.duration_since(t0).as_secs_f64().max(0.001);
        let mut best = (0u64, 0u64);
        for (idx, (ni, no)) in &now_net {
            if let Some((oi, oo)) = old.get(idx) {
                let (a, b) = (ni.wrapping_sub(*oi) as u64, no.wrapping_sub(*oo) as u64);
                if a + b > best.0 + best.1 {
                    best = (a, b);
                }
            }
        }

        let mut ms = MemStatus {
            len: std::mem::size_of::<MemStatus>() as u32,
            load: 0, total: 0, avail: 0, total_pf: 0, avail_pf: 0, total_virt: 0, avail_virt: 0, avail_ext: 0,
        };
        unsafe {
            GlobalMemoryStatusEx(&mut ms);
        }
        let _ = win.emit(
            "sys",
            Sys {
                cpu,
                ram: ms.load.min(100) as u8,
                used_mb: ms.total.saturating_sub(ms.avail) / 1_048_576,
                total_mb: ms.total / 1_048_576,
                down: (best.0 as f64 / secs) as u64,
                up: (best.1 as f64 / secs) as u64,
            },
        );
    }
}

// ---------- Menu cepat Wi-Fi & Bluetooth, keluar ----------

#[tauri::command]
fn quit(app: tauri::AppHandle) {
    app.exit(0);
}

fn ro_init() {
    unsafe {
        let _ = RoInitialize(RO_INIT_MULTITHREADED);
    }
}

fn radio_on(kind: RadioKind) -> Option<bool> {
    let radios = Radio::GetRadiosAsync().and_then(|a| a.get()).ok()?;
    for r in &radios {
        if r.Kind().ok() == Some(kind) {
            return Some(r.State().ok() == Some(RadioState::On));
        }
    }
    None
}

#[tauri::command]
async fn radio_set(kind: String, on: bool) -> String {
    ro_init();
    let kind = if kind == "wifi" { RadioKind::WiFi } else { RadioKind::Bluetooth };
    match Radio::RequestAccessAsync().and_then(|a| a.get()) {
        Ok(s) if s == RadioAccessStatus::Allowed => {}
        _ => return "access".into(),
    }
    let Ok(radios) = Radio::GetRadiosAsync().and_then(|a| a.get()) else { return "error".into() };
    for r in &radios {
        if r.Kind().ok() == Some(kind) {
            let st = if on { RadioState::On } else { RadioState::Off };
            return match r.SetStateAsync(st).and_then(|a| a.get()) {
                Ok(s) if s == RadioAccessStatus::Allowed => "ok".into(),
                _ => "failed".into(),
            };
        }
    }
    "none".into()
}

#[derive(Serialize)]
struct WifiNet {
    ssid: String,
    bars: u8,
    secured: bool,
}

#[derive(Serialize)]
struct WifiList {
    error: String, // "" bila berhasil; "off" | "access" | "noadapter" | "error"
    radio: Option<bool>,
    nets: Vec<WifiNet>,
}

fn wifi_adapter() -> Result<WiFiAdapter, String> {
    ro_init();
    match WiFiAdapter::RequestAccessAsync().and_then(|a| a.get()) {
        Ok(s) if s == WiFiAccessStatus::Allowed => {}
        _ => return Err("access".into()),
    }
    let list = WiFiAdapter::FindAllAdaptersAsync()
        .and_then(|a| a.get())
        .map_err(|_| "error".to_string())?;
    list.GetAt(0).map_err(|_| "noadapter".to_string())
}

#[tauri::command]
async fn wifi_list() -> WifiList {
    ro_init();
    let radio = radio_on(RadioKind::WiFi);
    if radio == Some(false) {
        return WifiList { error: "off".into(), radio, nets: vec![] };
    }
    let adapter = match wifi_adapter() {
        Ok(a) => a,
        Err(e) => return WifiList { error: e, radio, nets: vec![] },
    };
    let _ = adapter.ScanAsync().and_then(|a| a.get()); // memindai ulang (beberapa detik)
    let mut nets: Vec<WifiNet> = vec![];
    if let Ok(list) = adapter.NetworkReport().and_then(|r| r.AvailableNetworks()) {
        for n in &list {
            let ssid = n.Ssid().map(|s| s.to_string()).unwrap_or_default();
            if ssid.is_empty() {
                continue; // jaringan tersembunyi
            }
            let bars = n.SignalBars().unwrap_or(0);
            let secured = n
                .SecuritySettings()
                .and_then(|s| s.NetworkAuthenticationType())
                .map(|t| t != NetworkAuthenticationType::None && t != NetworkAuthenticationType::Open80211)
                .unwrap_or(true);
            if let Some(e) = nets.iter_mut().find(|e| e.ssid == ssid) {
                if bars > e.bars {
                    e.bars = bars;
                }
            } else {
                nets.push(WifiNet { ssid, bars, secured });
            }
        }
    }
    nets.sort_by(|a, b| b.bars.cmp(&a.bars));
    nets.truncate(12);
    WifiList { error: String::new(), radio, nets }
}

/// "ok" | "need_password" | "bad_password" | "nonet" | "failed" | "access" | "error"
#[tauri::command]
async fn wifi_connect(ssid: String, password: Option<String>) -> String {
    let adapter = match wifi_adapter() {
        Ok(a) => a,
        Err(e) => return e,
    };
    let Ok(list) = adapter.NetworkReport().and_then(|r| r.AvailableNetworks()) else { return "nonet".into() };
    let mut target = None;
    for n in &list {
        if n.Ssid().map(|s| s.to_string()).unwrap_or_default() == ssid {
            target = Some(n);
            break;
        }
    }
    let Some(net) = target else { return "nonet".into() };
    let pw = password.filter(|p| !p.is_empty());
    let started = match &pw {
        Some(p) => {
            let Ok(cred) = PasswordCredential::new() else { return "error".into() };
            let _ = cred.SetPassword(&HSTRING::from(p.as_str()));
            adapter.ConnectWithPasswordCredentialAsync(&net, WiFiReconnectionKind::Automatic, &cred)
        }
        None => adapter.ConnectAsync(&net, WiFiReconnectionKind::Automatic),
    };
    match started.and_then(|a| a.get()).and_then(|r| r.ConnectionStatus()) {
        Ok(s) if s == WiFiConnectionStatus::Success => "ok".into(),
        Ok(s)
            if s == WiFiConnectionStatus::InvalidCredential
                || s == WiFiConnectionStatus::UnspecifiedFailure
                || s == WiFiConnectionStatus::AccessRevoked =>
        {
            if pw.is_some() { "bad_password".into() } else { "need_password".into() }
        }
        Ok(_) => "failed".into(),
        Err(_) => "error".into(),
    }
}

#[tauri::command]
async fn wifi_disconnect() -> String {
    match wifi_adapter() {
        Ok(a) => {
            let _ = a.Disconnect();
            "ok".into()
        }
        Err(e) => e,
    }
}

#[derive(Serialize)]
struct BtPaired {
    name: String,
    addr: u64,
    connected: bool,
}

#[derive(Serialize)]
struct BtList {
    radio: Option<bool>,
    devices: Vec<BtPaired>,
}

#[tauri::command]
async fn bt_list() -> BtList {
    ro_init();
    let radio = radio_on(RadioKind::Bluetooth);
    let mut devices: Vec<BtPaired> = vec![];
    if radio == Some(true) {
        if let Ok(sel) = BluetoothDevice::GetDeviceSelectorFromPairingState(true) {
            if let Ok(found) = DeviceInformation::FindAllAsyncAqsFilter(&sel).and_then(|a| a.get()) {
                for d in &found {
                    let (Ok(id), Ok(name)) = (d.Id(), d.Name()) else { continue };
                    let Ok(dev) = BluetoothDevice::FromIdAsync(&id).and_then(|a| a.get()) else { continue };
                    let addr = dev.BluetoothAddress().unwrap_or(0);
                    let connected = dev
                        .ConnectionStatus()
                        .map(|s| s == BluetoothConnectionStatus::Connected)
                        .unwrap_or(false);
                    devices.push(BtPaired { name: name.to_string(), addr, connected });
                }
            }
        }
    }
    devices.sort_by(|a, b| b.connected.cmp(&a.connected).then(a.name.cmp(&b.name)));
    BtList { radio, devices }
}

// Sambung/putus perangkat audio yang sudah dipasangkan: nyalakan atau matikan layanannya
// (A2DP Sink, Hands-Free, Headset) lewat bthprops. Hasilnya tergantung perangkat.
#[repr(C)]
struct BtDevInfo {
    size: u32,
    _pad: u32,
    address: u64,
    class: u32,
    connected: i32,
    remembered: i32,
    authenticated: i32,
    last_seen: [u16; 8],
    last_used: [u16; 8],
    name: [u16; 248],
}
#[link(name = "bthprops")]
extern "system" {
    fn BluetoothGetDeviceInfo(radio: isize, info: *mut BtDevInfo) -> u32;
    fn BluetoothSetServiceState(radio: isize, info: *const BtDevInfo, guid: *const Guid, flags: u32) -> u32;
}
fn svc(short: u32) -> Guid {
    Guid { d1: short, d2: 0, d3: 0x1000, d4: [0x80, 0x00, 0x00, 0x80, 0x5F, 0x9B, 0x34, 0xFB] }
}

#[tauri::command]
async fn bt_toggle(addr: u64, connect: bool) -> String {
    let mut info: BtDevInfo = unsafe { std::mem::zeroed() };
    info.size = std::mem::size_of::<BtDevInfo>() as u32;
    info.address = addr;
    unsafe {
        let rc = BluetoothGetDeviceInfo(0, &mut info);
        if rc != 0 {
            return format!("noinfo:{rc}");
        }
        let mut ok = false;
        for g in [svc(0x110B), svc(0x111E), svc(0x1108)] {
            if connect {
                // matikan lalu nyalakan lagi untuk memicu penyambungan ulang
                BluetoothSetServiceState(0, &info, &g, 0);
                if BluetoothSetServiceState(0, &info, &g, 1) == 0 {
                    ok = true;
                }
            } else if BluetoothSetServiceState(0, &info, &g, 0) == 0 {
                ok = true;
            }
        }
        if ok { "ok".into() } else { "failed".into() }
    }
}

// ---------- Mulai otomatis saat Windows menyala (HKCU\...\Run, tanpa hak admin) ----------

const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

fn reg(args: &[&str]) -> bool {
    Command::new("reg")
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn autostart_enabled() -> bool {
    reg(&["query", RUN_KEY, "/v", "NotchNook"])
}

fn autostart_write() -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    reg(&["add", RUN_KEY, "/v", "NotchNook", "/t", "REG_SZ", "/d", &format!("\"{}\"", exe.display()), "/f"])
}

#[tauri::command]
async fn autostart_get() -> bool {
    autostart_enabled()
}

/// "ok" | "failed" | "dev" (versi debug tidak didaftarkan agar tidak menunjuk ke target\debug)
#[tauri::command]
async fn autostart_set(on: bool) -> String {
    if cfg!(debug_assertions) {
        return "dev".into();
    }
    let ok = if on {
        autostart_write()
    } else {
        reg(&["delete", RUN_KEY, "/v", "NotchNook", "/f"]);
        !autostart_enabled()
    };
    if ok { "ok".into() } else { "failed".into() }
}

// ---------- Seret file keluar dari Shelf (drag & drop OLE asli) ----------
// Perintah sinkron berjalan di thread utama; DoDragDrop bersifat modal sampai file dilepas.
#[tauri::command]
fn drag_out(window: tauri::Window, path: String) -> Result<(), String> {
    let file = std::path::PathBuf::from(&path);
    if !file.exists() {
        return Err("missing".into());
    }
    PINNED.store(true, Relaxed); // notch tidak menutup selama menyeret
    let res = drag::start_drag(
        &window,
        drag::DragItem::Files(vec![file]),
        drag::Image::Raw(include_bytes!("../icons/drag.png").to_vec()),
        |_, _| {},
        Default::default(),
    );
    PINNED.store(false, Relaxed);
    res.map_err(|e| e.to_string())
}
