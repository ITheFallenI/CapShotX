# CapShotX architecture decisions

Engineering decisions taken at phase gates (spec §14.1). Each entry records what
was chosen, why, and what evidence backs it.

## ADR-001 — Native shell: eframe/egui with wgpu (M0 proof of concept)

**Decision:** `eframe` 0.37 + `egui`, wgpu renderer, `tray-icon` for the
notification-area icon.

**Tray behaviour:** right-click opens the native context menu (Open, Settings,
Quit). Left-click is forwarded to the app, which opens the window or does
nothing per user setting. Closing the window hides to tray by default.

**Hidden-window handling:** eframe 0.37 calls `App::logic` while the window is
hidden whenever `Context::request_repaint` is called, and applies viewport
commands (including `CancelClose`) from it. Tray and single-instance events are
sent over a channel and drained in `logic`, so no Win32 window manipulation is
needed in the app.

**Single instance:** a named mutex plus a named auto-reset event
(`platform-windows::single_instance`). A second launch signals the event and
exits; the primary shows its window.

**Measured memory (release, Windows 11, NVIDIA GPU, 1040×680 window):**

| Renderer | Private working set (Task Manager) | Commit |
|---|---|---|
| wgpu (all backends) | ~126 MB | ~439 MB |
| glow (OpenGL) | ~93 MB | ~131 MB |
| WinShotX reference (WebView2 + process) | ~246 MB | — |

Hiding the window does not release GPU memory; the renderer stays alive. Most
of the wgpu commit charge is GPU-driver reservation. CPU while hidden: 0%.

**Open question:** the spec's idle (tray-only) target is <75 MB. Reaching it
needs the window and renderer torn down when hidden and recreated on demand,
with the tray living outside the eframe event loop. Evaluate before M0 exit.

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

## ADR-003 — Tray menu: owner-drawn Direct2D popup

**Decision:** the tray's right-click menu is CapShotX's own popup
(`platform-windows::popup_menu`), not a native `HMENU`/`muda` menu.

**Why:** the native menu can't match the app's look (header with logo, rounded
hover rows, app theme). An egui popup would need the renderer alive, which
conflicts with the plan to tear it down while hidden (ADR-001). The popup is a
`WS_POPUP` window drawn with Direct2D/DirectWrite; DWM supplies Windows 11
rounded corners and shadow. It runs on the UI thread's message loop, follows
the app's light/dark theme, scales per-monitor DPI, supports keyboard
navigation (↑/↓/Enter/Esc) and closes when it loses activation.

Visual test: `cargo run -p platform-windows --example popup_menu [dark]`.

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
import database). `gallery::scan` walks it (dated sub-folders, depth ≤ 4) on a
background thread at startup, after each saved capture, rename or delete, and
every 10 s while the window is visible. Measured: 1,500 files in ~1 ms.

**Thumbnails:** two worker threads, most-recent-request-first so whatever is on
screen wins over rows already scrolled past; queued work for tiles that left
the viewport is dropped each frame. 320 px PNGs are cached on disk under
`%LOCALAPPDATA%\CapShotX\data\thumbs`, keyed by path + size + mtime. At most
120 thumbnail textures stay in GPU memory (least-recently-drawn evicted).
Measured: one screenful (30) in 116 ms cold, 7 ms from cache.

**UI:** a Gallery page (day headers, search, sort, right-click actions) whose
grid lays out all rows up front but draws only those in the viewport; Home
shows the newest two rows. Preview loads the full image off-thread (≤ 8192 px).
Delete goes to the Recycle Bin via `SHFileOperationW` with `FOF_ALLOWUNDO`.
