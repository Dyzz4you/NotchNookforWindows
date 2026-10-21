# NotchNook untuk Windows

Panel bergaya "notch" untuk Windows 10/11, terinspirasi dari aplikasi NotchNook di macOS.
Notch kecil di tengah atas layar akan melebar saat kursor mendekat, berisi kontrol musik,
jam, Wi-Fi, Bluetooth, shelf file, dan alat bantu lainnya. Dibuat dengan **Tauri v2**
(Rust + WebView2), ukuran aplikasinya sekitar 1 MB.

> Proyek pribadi, tidak berafiliasi dengan NotchNook di macOS maupun pengembangnya.

## Fitur

**Beranda**
- Kontrol media Windows (Spotify, YouTube di browser, dan pemutar lain): judul, artis, sampul,
  putar/jeda, berikutnya, sebelumnya, bar progres yang bisa diklik untuk melompat.
- Warna aksen otomatis dari sampul album.
- Jam, tanggal, strip minggu ini, dan indikator baterai laptop berbentuk donat.

**Shelf**
- Seret file ke notch untuk menyimpannya sementara, seret keluar ke aplikasi lain.
- Klik file untuk membukanya. Daftar tersimpan walau aplikasi ditutup.

**Koneksi**
- Wi-Fi: saklar, pindai jaringan sekitar, sambung dengan satu klik, kolom sandi untuk jaringan baru.
- Bluetooth: saklar, daftar perangkat yang dipasangkan, sambung/putus, dan baterai perangkat
  (jika perangkat melaporkannya ke Windows).

**Sistem, Alat, Catatan**
- Monitor CPU, RAM, dan kecepatan jaringan.
- Timer fokus, peluncur cepat (Kalkulator, Notepad, File Explorer, Terminal, Tangkap Layar, Pengaturan).
- Catatan cepat yang tersimpan otomatis.

**Notch tertutup**
- Menampilkan jam, ikon Wi-Fi/Bluetooth, dan baterai laptop. Saat musik diputar: sampul, judul,
  dan equalizer. Saat timer berjalan: hitung mundur.
- Pop-up singkat saat perangkat Bluetooth tersambung/terputus, Wi-Fi berganti, charger
  dipasang/dilepas, timer selesai, atau lagu berganti.
- Sembunyi otomatis saat jendela (mis. Chrome yang di-maximize) menutupi area notch.

**Pengaturan** (ikon roda gigi di pojok kanan atas)
- Tampilan notch: Pintar / Selalu tampil / Sembunyi.
- Mulai otomatis saat Windows menyala.
- Keluar dari aplikasi.

## Kebutuhan

- Windows 10/11 (dikembangkan dan diuji di Windows 11)
- [Rust](https://rustup.rs) dengan target MSVC
- Visual Studio Build Tools, workload **Desktop development with C++**
- WebView2 Runtime (sudah ada di Windows 11)
- Tauri CLI: `cargo install tauri-cli --version "^2"`

## Menjalankan dan membangun

```
cargo tauri dev      # mode pengembangan
cargo tauri build    # versi rilis (ringan)
```

Hasil build:
- `src-tauri/target/release/notch-nook.exe`
- `src-tauri/target/release/bundle/nsis/` (installer)

Gerakkan kursor ke tepi paling atas layar, tepat di tengah, untuk membuka notch.
Mulai otomatis dan penyimpanan pengaturan hanya bekerja pada versi rilis.

## Batasan yang diketahui

- Pindai Wi-Fi membutuhkan **Layanan Lokasi** Windows aktif. Jaringan kantor/kampus (login akun)
  tidak didukung.
- Sambung/putus Bluetooth bersifat upaya terbaik dan tergantung perangkat. Memasangkan perangkat
  baru masih lewat Pengaturan Windows.
- Baterai Bluetooth hanya tampil untuk perangkat yang melaporkannya ke Windows (umumnya headset/TWS).
- Shelf menyimpan lokasi file, bukan salinannya.
- Deteksi "tertutup jendela" hanya memeriksa jendela yang sedang aktif.

## Struktur proyek

```
ui/index.html          # seluruh antarmuka (HTML/CSS/JS)
src-tauri/src/main.rs  # logika Windows: kursor, media, jaringan, Bluetooth, sistem
src-tauri/tauri.conf.json
```

## Privasi

Aplikasi tidak mengirim data ke internet. Catatan, daftar Shelf, dan pengaturan disimpan
secara lokal di profil WebView2 pada komputermu. Kata sandi Wi-Fi hanya diteruskan ke Windows
dan tidak disimpan.

## Terima kasih

[Tauri](https://tauri.app), [windows-rs](https://github.com/microsoft/windows-rs), dan
[drag](https://crates.io/crates/drag).

## Lisensi

[MIT](LICENSE)