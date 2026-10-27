<p align="center">
  <img src="Icon/capshotx-256.png" width="128" alt="CapShotX icon">
</p>

<h1 align="center">CapShotX</h1>

<p align="center">
  A fast, native screenshot and screen-recording tool for Windows, written in Rust.<br>
  Frozen-screen region capture, a built-in capture gallery, and MP4 recording with
  in-sync desktop and microphone audio. Everything stays on your PC.
</p>

> **Preview build.** Screenshots and the gallery are complete; screen recording is
> working and being hardened. The image editor and video studio come later
> (see the [roadmap](#roadmap)).

## Features

### Screenshots
- **Region, window or display** — drag an area, click a window, or click a monitor.
  The screen is frozen first, so menus and tooltips stay put and the overlay never
  appears in the shot.
- Pixel-exact on mixed-DPI multi-monitor setups, with a magnifier, colour readout
  and keyboard nudging.
- Copy to the clipboard, save automatically, or choose each time from a small toolbar.
- Configurable save folder and file-name template (dated folders by default).

### Gallery
- Every capture and recording in one place, grouped by day, with search and sort.
- Fast thumbnails (cached on disk) that stay responsive with thousands of files.
- Preview, rename, copy, show in Explorer, or send to the Recycle Bin.

### Screen recording
- Record a **region**, a **window** (it follows the window when moved) or a **full screen**.
- **Desktop audio and microphone**, each with its own device picker and live level meter.
- H.264 + AAC **MP4**, 30 or 60 FPS, optional countdown and cursor capture.
- Pause and resume without gaps or drift: video and both audio streams share one clock,
  and still screens or quiet stretches never shorten the file.
- A small floating control bar (time, level meter, pause, stop) that is hidden from the
  recording itself. A red dot on the tray icon shows when a recording is running.
- Recordings are written to a `.partial` file and renamed only once complete.
- Clear errors instead of silent failures, e.g. a blocked microphone or a
  disconnected headset.

## Default shortcuts

| Action | Shortcut |
|---|---|
| Capture region | `Ctrl+Shift+2` |
| Start / stop recording | `Ctrl+Shift+5` |
| Open CapShotX | `Ctrl+Shift+G` |

In the capture overlay: `Esc` cancels, `F` picks the full screen, `Z` toggles the
magnifier and `C` copies the colour under the cursor. All shortcuts can be changed in
Settings.

## Building

Requires Windows 10 (2004) or later and a recent stable Rust toolchain.

```sh
cargo build --release -p capshotx
target\release\CapShotX.exe
```

Run the tests with `cargo test --workspace`. `cargo run -p recorder --example record_check`
makes a short real recording and checks its duration, pause handling and audio sync.

## Project layout

| Crate | What it does |
|---|---|
| `app` | The CapShotX app (egui): home, gallery, settings, tray and recording control |
| `capture` | Screenshot engine: screen freeze, selection overlay, PNG output |
| `recorder` | Screen recording: Windows Graphics Capture video, WASAPI audio, mixing, timing |
| `media` | Media Foundation MP4 writing and reading |
| `gallery` | Capture-folder index, thumbnails and file actions |
| `platform-windows` | Safe wrappers for the Win32 APIs used everywhere else |

Design decisions are recorded in [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md), and the
full product specification is in [`Documentation/`](Documentation/).

## Roadmap

1. ~~Screenshot capture + gallery~~ ✅
2. **Screen recording with audio** ← in progress
3. Image editor: layers, annotations, redaction, adjustments
4. Video studio: timeline editing, smooth zoom, cursor effects, callouts
5. 1.0 hardening: installer, updates, accessibility, performance checks

## Privacy

CapShotX makes no network connections. Captures, recordings, settings and thumbnails all
stay on your machine.

## License

[MIT](LICENSE)
