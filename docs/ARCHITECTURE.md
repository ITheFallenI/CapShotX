# CapShotX architecture decisions

Engineering decisions taken at phase gates (spec §14.1). Each entry records what
was chosen, why, and what evidence backs it.

## ADR-001 — Native shell: eframe/egui with glow + zero-GPU tray mode

**Decision:** `eframe` 0.37 + `egui` with the `glow` renderer for the UI window,
`tray-icon` for the notification-area icon, and on-demand window/renderer
teardown (`run_and_return: true`) whenever hidden to the tray.

**Tray & background loop:** `CoreState` (`Tray`, `HotkeyService`,
`SingleInstance`, `ScreenRecorder`, `Settings`, `GalleryCache` and saved window
geometry) lives outside `eframe` on a sleeping Win32 `GetMessageW` pump
(`platform_windows::shell::pump_messages_once`), woken via `PostThreadMessageW`
(`Waker`). `shell::init_process()` enables Per-Monitor V2 DPI and apartment COM
before any hidden window is created, so `start_hidden` launches straight to the
tray without ever creating an `eframe` event loop or GPU context. Under
**Low memory mode** (`settings.low_memory_mode`, **enabled by default**),
closing the window destroys the `eframe` window, GL context and GPU textures,
preserves page/geometry/gallery index in `CoreState`, and calls
`shell::trim_working_set` (`HeapCompact` + `K32EmptyWorkingSet`) whenever no
recording is active; disabling Low memory mode keeps the window hidden via
`ViewportCommand::Visible(false)` and switches the tray right-click menu to the
custom Direct2D popup.

**Single instance, Windows sign-in & elevated windows:**
- `platform-windows::single_instance` creates `Local\CapShotX.SingleInstance` and
  `Local\CapShotX.Activate` with a Medium-Integrity SDDL (`D:(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;IU)S:(ML;;NW;;;ME)`)
  so a normal Explorer launch can wake an elevated primary instance (~48 ms)
  without triggering another UAC prompt.
- **Capture admin windows (`settings.run_as_admin`, default `false` — opt-in):**
  Windows UIPI blocks `RegisterHotKey` and `SetForegroundWindow` when High-IL
  windows such as Task Manager are focused. Only when the user explicitly turns
  this setting on does CapShotX prompt UAC (`ShellExecuteExW("runas")`,
  snapping back to `false` if UAC is cancelled).
- **Launch at Windows startup (`settings.launch_at_startup`, default `true`):**
  `platform-windows::startup::sync` registers either a per-user Task Scheduler
  `<LogonTrigger>` with `<RunLevel>HighestAvailable` (battery-enabled, unlimited
  duration, normal priority, `--autostart` straight to tray — zero UAC prompt on
  boot) when elevated, or `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`
  when non-elevated.

**Measured memory (release, Windows 11, hybrid Intel + NVIDIA GPUs, 1040×680):**

| Configuration | Working set | Private commit | Threads / DLLs (tray) | Reopen latency |
|---|---|---|---|---|
| Previous `wgpu` (all backends, hidden window kept alive) | ~150–263 MB | ~465 MB | 46–60 | — |
| `wgpu` (DX12-only) + tray teardown | ~2.1 MB tray / ~196–221 MB open | ~29 MB tray / ~255 MB open | — | ~415 ms |
| `glow` + Low memory OFF (warm hidden window + Direct2D tray menu) | ~39–100 MB (hidden or open) | ~92–114 MB | ~27–38 / ~83+ | <5 ms |
| `glow` + tray teardown + custom Direct2D tray menu | ~1–2 MB idle / ~52 MB menu open / ~31 MB after menu | ~22 MB after menu (~57 MB while open) | 27 / 83 | ~48 ms |
| **`glow` + Low memory ON — default Windows `HMENU` tray (default)** | **~0.4–2 MB tray (before & after menu) / ~24.5 MB menu open / ~39–100 MB UI open** | **~3.9 MB `start_hidden` (~4.8 MB menu open) / ~89–92 MB UI** | **12–15 / 59** | **~48 ms** |
| WinShotX reference (WebView2 + process) | ~246 MB | — | — | — |

CPU while in tray: 0%. Well within the spec's <75 MB idle tray target.

## ADR-002 — Capture engine: direct `windows` bindings behind a safe crate

**Decision:** call Windows Graphics Capture, DXGI, Win32 and WASAPI through the
`windows` crate directly, wrapped in safe types in `platform-windows` /
`capture`. Do not build on third-party capture wrappers such as
`windows-capture`.

**Why:** frozen-overlay timing, monitor/DPI transforms, cursor and border
exclusion, frame-pool sizing and (later) the single media clock for recording
all need control that wrappers hide or fix in their own threading model. The
spec's worst historic bugs (static-screen truncation, timing drift) live
exactly in that layer. Rule: all `unsafe` Win32 code lives in the platform
crates; the rest of the workspace sees only safe, `Send` types.

## ADR-003 — Tray menu: native Win32 `HMENU` (Low memory mode) & Direct2D popup

**Decision:** `platform-windows::popup_menu` provides two tray right-click menu
paths controlled by **Settings › Window & tray → Low memory mode** (default
`true`):
1. **Standard native Win32 menu (`show_native_popup_menu`, default):** builds a
   `CreatePopupMenu` + `TrackPopupMenuEx` menu inside `user32.dll` and applies
   Windows 10/11 dark/light styling via `uxtheme.dll` (`SetPreferredAppMode` +
   `FlushMenuThemes`). Initialises zero Direct2D/DirectWrite factories, font
   caches or GPU surfaces.
2. **Custom rendered Direct2D menu (`show_popup_menu`, when Low memory mode is
   off):** a `WS_POPUP` window drawn with Direct2D/DirectWrite featuring the
   logo header, rounded hover rows and DWM shadow/corners. Both follow the app's
   light/dark/system theme, scale per-monitor DPI, support keyboard navigation
   and dismiss cleanly on outside click.

Visual test: `cargo run -p platform-windows --example popup_menu [dark] [native]`.

| Tray context menu | Working set (menu open) | Working set (after close) | Private commit (open → closed) | Threads | Loaded DLLs |
|---|---|---|---|---|---|
| **Default Windows `HMENU` (`show_native_popup_menu`)** | **24.5 MB** | **~0.1–2.0 MB** (trimmed) | **4.8 MB → 3.9 MB** | **15** | **59** |
| Custom Direct2D popup (`show_popup_menu`) | 52.2 MB | 30.9 MB | 56.9 MB → 22.3 MB | 27 | 83 |

## ADR-004 — Global shortcuts: `RegisterHotKey` on a dedicated thread

**Decision:** `platform-windows::hotkeys::HotkeyService` owns a thread with its
own message loop; hotkeys are registered thread-wide (no window) with
`MOD_NOREPEAT`. No low-level keyboard hooks (spec §3.3).

**Behaviour:** registration failures are reported per shortcut in Settings with
a plain-language reason (e.g. "Already used by Windows or another app") and
logged. Shortcuts are bound to typed `Action`s (`app.open`, …) — only actions
whose feature exists are listed. While a shortcut is being re-recorded, its
current registration is released so the keys reach the window.

## ADR-005 — Screenshot engine (M1): GDI freeze + per-monitor Direct2D overlay

**Freeze:** one GDI `BitBlt` (`SRCCOPY | CAPTUREBLT`) of the whole virtual
desktop, taken *before* any overlay exists, so overlay chrome can never appear
in output (CAP-02). Measured: 5120×1440 across two displays in 47–87 ms.
Content marked `WDA_EXCLUDEFROMCAPTURE` comes back black, as Windows intends.
WGC is kept for Phase 2 recording, where continuous frames matter; HDR-correct
stills (DXGI duplication) are a later refinement.

**Overlay:** one borderless topmost window per monitor, Direct2D render target
at 96 DPI (1 unit = 1 physical pixel) so each monitor is pixel-exact at its own
scale; UI chrome is scaled by that monitor's DPI. Selection state is shared in
virtual-desktop coordinates, so drags cross monitors and negative coordinates
work. Window targeting uses a Z-order snapshot (`EnumWindows`, DWM extended
frame bounds) excluding cloaked, click-through and own windows; the desktop
falls back to the whole monitor. Runs on its own thread; repeated hotkeys while
a session is open are ignored (CAP-05).

**Escape hatches (CAP-05):** Esc, right-click, losing activation (Alt-Tab) and a
3 s render watchdog all close the overlay.

**Output:** crop → PNG (RGB, fast compression) → clipboard as `CF_DIB` + `PNG`
→ optional atomic save (`.partial` then rename, never overwrites) → toast.
Measured end-to-end: ~360 ms from release to clipboard + file for 2148×1013.

## ADR-006 — Gallery (M2): folder index, LIFO thumbnail workers, virtual grid

**Index:** the capture folder itself is the library (GAL-02: originals, no
import database). Outputs are organised under `<date>/Images/*.png` and
`<date>/Video/*.mp4` (`capture::render_output_path`). `gallery::scan` walks up
to depth 6 on a background thread at startup, after each saved capture,
recording, rename or delete, and every 10 s while the window is visible.
Measured: 1,500 files in ~1 ms.

**Thumbnails:** up to two on-demand worker threads (exiting after 2 s idle),
most-recent-request-first so whatever is on screen wins over rows already
scrolled past; queued work for tiles that left the viewport is dropped each
frame. 320 px PNGs (including MP4 first-frame posters pre-warmed on save via
`gallery::warm_cache`) are cached on disk under
`%LOCALAPPDATA%\CapShotX\data\thumbs`, keyed by path + size + mtime. MP4
container duration (`moov`/`mvhd`/`mdhd` and fragmented `moof`/`traf`/`trun`)
is parsed in pure Rust (`gallery::mp4_dur`, falling back to `media::probe` only
if needed) so cached video tiles never load Media Foundation or DXVA drivers.
At most 48 thumbnail textures stay in GPU memory while the window is open
(least-recently-drawn evicted), and full-resolution preview textures (≤ 3840 px)
are released on leaving Gallery or closing to tray. Measured: one screenful (30)
in 116 ms cold, 7 ms from cache.

**UI:** a Gallery page (day headers, All/Images/Videos filter, search, sort,
right-click actions) whose grid lays out all rows up front but draws only those
in the viewport; Home shows the newest two rows. Preview loads the full image or
video poster off-thread (≤ 3840 px) and launches video playback on click.
Delete goes to the Recycle Bin via `SHFileOperationW` with `FOF_ALLOWUNDO`.

## ADR-007 — Screen recording & live bounds staging (M3 / Phase 2)

**Pre-recording workflow:** two explicit modes share the same engine:
1. **Camtasia-style bounds staging (default):** after initial selection in the
   frozen picker, `platform_windows::rec_staging` opens two `WDA_EXCLUDEFROMCAPTURE`
   topmost windows on the live desktop:
   - A per-pixel-alpha `WS_EX_LAYERED` frame (`UpdateLayeredWindow` with `ULW_ALPHA`
     rendered via `ID2D1DCRenderTarget` into a 32-bpp DIB). Its interior has
     `alpha == 0`, so clicks pass straight through to live desktop windows while
     the border, 8 resize handles and dimension/window pill remain interactive.
     Window selections track live `DWMWA_EXTENDED_FRAME_BOUNDS` until manually
     resized or moved.
   - A Direct2D control bar (`Reselect`, `Full`, `1080p`, `720p`, live `Sys` and
     `Mic` toggles with WASAPI level meters, `Start` and `Cancel`). Temporary
     level monitors are dropped before `Recorder::start` acquires endpoints.
2. **Fast record:** skips staging and proceeds straight from selection to the
   countdown/recording toolbar.

**Live recording outline (REC-04):** `platform_windows::rec_outline::RecordingOutline`
renders a topmost, click-through (`WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE`)
border around the active bounds (following window moves via `DWMWA_EXTENDED_FRAME_BOUNDS`,
colour-coded for countdown, live recording and paused states, and zero-cost when
bounds/state are unchanged). Two independent layers keep it out of the recorded
MP4: `SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE)` omits the window
from DWM capture surfaces, and whenever display space exists outside an edge the
stroke and 1 px contrast halo are drawn strictly *outside* `[rect.left, rect.right) ×
[rect.top, rect.bottom)`. Users can toggle it on/off at any time in
**Settings › Recording → Recording outline** (`record.show_outline`).

**Single media clock (REC-03):** `platform_windows::clock` reads `QueryPerformanceCounter`
in 100 ns Media Foundation ticks. `SessionClock` maps QPC instants to active
recording time, discarding video/audio inside paused intervals and freezing
`elapsed` cleanly if stopped while paused.

**Video path (REC-01/05):** `Direct3D11CaptureFramePool::CreateFreeThreaded`
(depth 2) captures either a `GraphicsCaptureItem` window (recreating the pool if
resized, warning and holding the last image if closed) or a monitor region
cropped via `CopySubresourceRegion` to even dimensions. The encoder thread
drives fixed `1/fps` slots on `SessionClock`, repeating the latest frame across
still desktop stretches so variable WGC delivery never shortens the file, and
enforcing at least one full frame slot on sub-frame stops.

**Audio path & mixer (REC-02/05):** independent WASAPI event-driven threads for
render loopback (`AUDCLNT_STREAMFLAGS_LOOPBACK`) and microphone input convert
device buffers (float32 / PCM16 / PCM24 / extensible) and stamp each packet with
its QPC capture position. `Mixer` resamples non-48 kHz devices, fills silent
loopback gaps with explicit zeros (capped at 10 s per track to bound memory),
applies per-source gain (0–200%) plus live toolbar mute, and soft-clips above
0.8 before handing 48 kHz stereo PCM to AAC encoding.

**Container & crash recovery (REC-03):** `media::Mp4Writer` writes H.264 + AAC
into `MFTranscodeContainerType_FMPEG4` (fragmented MP4) at `<name>.mp4.partial`
with a forced 2 s keyframe interval (`MF_MT_MAX_KEYFRAME_SPACING = fps * 2`) so
self-contained `moof`+`mdat` fragments flush every ~2 s, then renames to a
unique `.mp4` after `Finalize()`. On startup, `recorder::recover_partials` scans
for leftover `.mp4.partial` files, verifies both container streams and first-frame
decodability via Media Foundation, renames valid files to `<stem> (recovered).mp4`,
and leaves unreadable partials untouched on disk.
