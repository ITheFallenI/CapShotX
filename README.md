<p align="center">
  <img src="Icon/capshotx-256.png" width="128" alt="CapShotX icon">
</p>

<h1 align="center">CapShotX</h1>

<p align="center">
  A fast, native screenshot and screen-recording tool for Windows, written in Rust.<br>
  Frozen-screen region capture, a built-in capture gallery, and MP4 recording with
  in-sync desktop and microphone audio. Everything stays on your PC.
</p>

> **Preview build.** Screenshots, the capture gallery, screen recording with audio,
> and the native Windows installer are complete. The image editor and video studio come
> next (see the [roadmap](#roadmap)).

## Download & Installation

Grab the latest build from [**GitHub Releases**](https://github.com/ITheFallenI/CapShotX/releases/latest):

- **[`CapShotX-Setup.exe`](https://github.com/ITheFallenI/CapShotX/releases/latest/download/CapShotX-Setup.exe) (Recommended — Windows Installer)**  
  Native per-user installer (`%LOCALAPPDATA%\Programs\CapShotX`, no UAC prompt required). Adds a Start Menu shortcut, configures optional launch at Windows startup, and registers a clean uninstaller in **Windows Settings › Apps › Installed apps** (`Uninstall CapShotX.exe`).
- **[`CapShotX.exe`](https://github.com/ITheFallenI/CapShotX/releases/latest/download/CapShotX.exe) (Portable single-file executable)**  
  Zero dependencies — drop it anywhere and run.

## Features

### Screenshots
- **Region, window or display** — drag an area, click a window, or click a monitor.
  The screen is frozen first, so menus and tooltips stay put and the overlay never
  appears in the shot.
- Pixel-exact on mixed-DPI multi-monitor setups, with a magnifier, colour readout
  and keyboard nudging.
- Copy to the clipboard, save automatically, or choose each time from a small toolbar.
- Toggleable bottom-right **capture preview notification** and optional **shutter sound**.
- Configurable save folder and file-name template — by default outputs are organised
  by day into `<YYYY-MM-DD>/Images/*.png` and `<YYYY-MM-DD>/Video/*.mp4`.

### Gallery
- Every screenshot and recording in one place, grouped by day, with **All / Images /
  Videos** filtering, file-name search and sort.
- Fast thumbnails and video poster frames (cached on disk) with duration badges,
  staying responsive with thousands of files.
- Preview (click a video to play), rename, copy, show in Explorer, or send to the
  Recycle Bin.

### Screen recording
- **Camtasia-style bounds staging (default)** — pick initial bounds (drag a region,
  click a window, or press `F` for full screen), then fine-tune the live selection
  with 8 resize handles, move badge, `Full` / `1080p` / `720p` / `Reselect` presets,
  and live **Sys** / **Mic** toggles with level meters while interacting normally
  with windows inside the frame before clicking **Start** (`Enter`).
- **Fast record** — available from Home, the tray menu, Settings, or its own global
  shortcut when you want to pick bounds and start recording immediately.
- Record a **region**, a **window** (follows the window when moved) or a **full screen**.
- **Desktop audio and microphone**, each with its own device picker, 0–200% gain
  control, live level meter, a **Test sound** button on Desktop audio (plays a stereo
  WASAPI chime on the selected output device), and a built-in **4 s audio check** in
  Settings.
- H.264 + AAC **MP4**, **High / Balanced / Compact** quality presets, 30 or 60 FPS,
  optional countdown and cursor capture.
- Pause and resume without gaps or drift: video and both audio streams share one QPC
  clock, and still screens or quiet stretches never shorten the file.
- Floating capture-excluded control bar (elapsed time, clickable audio meter / live
  **Mute**, pause/resume, stop), a click-through **recording outline** outside the
  recorded bounds (toggleable in Settings, never visible in the video), plus a red
  recording indicator on the tray icon.
- Crash-tolerant fragmented MP4 written to `.mp4.partial` (2 s keyframe fragments);
  interrupted recordings are verified and recovered automatically on next launch.
- Clear errors instead of silent failures, e.g. a minimised window, a blocked
  microphone, or a disconnected headset.

### System tray, Low memory mode & Settings
- **Built-in GitHub auto-updater** — checks GitHub Releases in the background on
  startup (toggleable in Settings) and lets you download, verify (SHA-256), and install
  updates in place with one click (**Update & Restart**), with no UAC prompt required.
- **Low memory mode (on by default)** — destroys the main window and GPU swapchain
  while minimised to the system tray (`--minimized`) and trims the working set to
  ~0.1–2 MB until reopened.
- **Native tray context menu** — right-click the tray icon to **Capture region /
  window / display**, **Record** (**Stage bounds** or **Fast record**), open the
  **Gallery**, **Settings**, or the **Captures folder**, or **Quit**.
- **Configurable tray left-click** — choose whether left-clicking the tray icon opens
  CapShotX, opens the latest capture folder, starts a capture/recording, or opens the
  Gallery/Settings.
- **Tabbed Settings with instant search** — organised into **General**, **Capture**,
  **Recording**, **Hotkeys**, and **Displays & About**, with automatic type-to-search
  filtering across every setting.
- **Opt-in administrator capture** — runs as a standard user by default; enable
  **Capture admin windows** in **Settings › General** only if you need hotkeys and
  cursor hooks to work inside elevated windows such as Task Manager.

## Shortcuts

| Action | Default shortcut |
|---|---|
| Capture region | `Ctrl+Shift+2` |
| Start / stop recording | `Ctrl+Shift+5` |
| Fast record | *(configurable in Settings)* |
| Pause / resume recording | *(configurable in Settings)* |
| Open CapShotX | `Ctrl+Shift+G` |

Every shortcut in **Settings › Hotkeys** supports **any key or combination** —
including `Ctrl+X`, `Ctrl+C`, `Ctrl+V`, `Win+…`, `Alt+…`, function keys, Numpad keys,
navigation keys, or single keys.

In the capture overlay: `Esc` cancels, `F` picks the full screen, `Z` toggles the
magnifier and `C` copies the colour under the cursor. In the pre-record bounds
staging view: drag handles or arrow keys adjust bounds (`Shift` ×5, `Ctrl` resize),
`F` snaps to full screen, `Enter` starts recording and `Esc` cancels.

## Building

Requires Windows 10 (2004) or later and a recent stable Rust toolchain.

```powershell
# Build the main application
cargo build --release -p capshotx
target\release\CapShotX.exe

# Build the standalone installer + portable bundle into dist\
.\packaging\build-installer.ps1

# Build and publish/update the GitHub Release in one step
.\packaging\build-installer.ps1 -PublishRelease
```

Run the tests with `cargo test --workspace`. `cargo run -p recorder --example record_check`
makes a short real recording and checks its duration, pause handling and audio sync.

## Project layout

| Crate | What it does |
|---|---|
| `app` | The CapShotX app (egui): home, gallery, tabbed settings, tray and recording control |
| `installer` | Native Windows GUI/silent installer (`CapShotX-Setup.exe`) and uninstaller |
| `capture` | Screenshot engine: screen freeze, selection overlay, PNG output |
| `recorder` | Screen recording: Windows Graphics Capture video, WASAPI audio, mixing, timing |
| `media` | Media Foundation MP4 writing and reading |
| `gallery` | Capture-folder index, thumbnails and file actions |
| `platform-windows` | Safe wrappers for the Win32 APIs used everywhere else |

Design decisions are recorded in [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md), the
audio/video test matrix is in [`docs/RECORDING_TEST_MATRIX.md`](docs/RECORDING_TEST_MATRIX.md),
and the full product specification is in [`Documentation/`](Documentation/).

## Roadmap

1. ~~Screenshot capture + gallery~~ ✅
2. ~~Screen recording with audio~~ ✅
3. ~~Native Windows installer, uninstaller & GitHub auto-updater~~ ✅
4. **Image editor**: layers, annotations, redaction, adjustments ← next
5. Video studio: timeline editing, smooth zoom, cursor effects, callouts

## Known limitations

- Region recordings capture within a single display (Windows Graphics Capture
  monitors one display per session); cross-monitor screenshots are supported.
- Protected DRM surfaces and windows using `WDA_EXCLUDEFROMCAPTURE` appear black
  in captures and recordings by Windows design.
- The layered image editor (Phase 3), video timeline studio (Phase 4) and rolling
  replay buffer are not yet implemented.

## Privacy

Captures, recordings, settings and thumbnails all stay on your machine. No telemetry or
analytics are collected; the only optional network request is checking GitHub Releases for
updates (which can be disabled in **Settings**).

## License

[MIT](LICENSE)
