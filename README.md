# ⚡ SpeedDisplay

**A very fast image & video viewer for Windows**, in the spirit of XnView MP / IrfanView — Rust backend (Tauri 2), TypeScript frontend, hardware video decoding (WebView2).

## Features

- **Virtualized thumbnail grid** — stays smooth with thousands of files; alternative list view
- **Parallel thumbnail generation** (rayon, all CPU cores) with a persistent SQLite + JPEG/PNG cache
- **Images**: JPEG, PNG, GIF, BMP, TIFF, WebP, ICO, TGA, PNM — plus AVIF, HEIC, JXL, JP2, PSD, SVG through FFmpeg (depending on your FFmpeg build)
- **RAW**: near-instant display via the embedded JPEG preview (CR2, CR3, NEF, ARW, RW2, RAF, ORF, DNG…)
- **Videos**: thumbnails, animated hover previews (live or pre-generated), direct playback, lossless MKV→MP4 remux, NVENC transcode as a last resort
- **Fullscreen viewer**: wheel zoom, keyboard navigation, neighbor preloading
- Rename, **Recycle Bin** delete (never hard-deletes), reveal in Explorer
- Duplicate detection, search, EXIF rotation, XMP metadata
- Dark/light themes, 6 languages, persistent settings

## Setup (one time)

Open **PowerShell as administrator**:

```powershell
Set-ExecutionPolicy -Scope Process Bypass -Force
.\setup-windows.ps1
```

The script installs via winget: Visual Studio Build Tools (C++), Rustup, Node.js LTS, FFmpeg (Gyan build). **Close and reopen your terminal** afterwards (PATH refresh).

## Development

```cmd
npm install
run-dev.cmd
```

The first Rust build takes 5–10 minutes (Tauri and dependencies); later builds are incremental.

## Building the installer

```cmd
build-release.cmd
```

The NSIS installer is produced in `src-tauri\target\release\bundle\nsis\` (NSIS is fetched automatically by Tauri on first build).

## Architecture

```
├── core/            # Pure Rust crate: scan, thumbnails, SQLite cache, RAW, FFmpeg
│                    #   tested independently: cargo test --release
├── src-tauri/       # Tauri 2 app: commands, events, state, icons
├── src/             # TypeScript frontend (Vite): virtualized grid, viewer, player
├── index.html
├── setup-windows.ps1
├── run-dev.cmd
└── build-release.cmd
```

Design notes:

- Files are served to the frontend through Tauri's **asset protocol** (`convertFileSrc`) — no local HTTP server. The asset scope is intentionally broad (`**`) so any folder can be browsed; the app runs fully offline.
- The thumbnail cache is invalidated by (path, mtime, file size, thumbnail size).
- Videos play as-is when the container/codec is WebView2-compatible, otherwise stream-copy remux (fast, lossless), otherwise `h264_nvenc` transcode.
- An atomic `generation` counter prevents stale thumbnails from appearing during rapid folder switches.

## Known limitations

- HEIC/AVIF/JXL support depends on the codecs included in the winget FFmpeg build.
- `core` and `src-tauri` are intentionally **outside a Cargo workspace** (version constraints).

## Support

If SpeedDisplay makes your photo sorting faster:

[![ko-fi](https://ko-fi.com/img/githubbutton_sm.svg)](https://ko-fi.com/hypedigger)

## License

[MIT](LICENSE)
