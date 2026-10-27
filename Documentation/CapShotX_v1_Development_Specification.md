# CAPSHOTX
## Product and Development Specification — Version 1.0

**Status:** v1.0 development baseline · **Date:** 27 October 2026 · **Target platform:** Windows 11 (x64) · **Implementation:** Native Rust desktop · **Project name:** CapShotX (chosen; legal availability not yet verified)

> **Product promise:** Capture instantly. Edit naturally. Record reliably. Produce polished screenshots and tutorial videos without the complexity of ShareX, Photoshop, Camtasia or a full video-production suite.
>
> **Name status:** CapShotX is the selected project name. It has not been legally cleared. Before public release, check UK trade marks (including similar names), GitHub organisations, domains and other existing software brands. Name availability is not a technical release guarantee.

---

## 1. Executive brief

CapShotX 1.0 is a lightweight, local-first screen-capture and screen-recording application. It combines five integrated areas: **Capture**, **Gallery**, **Recorder**, and the on-demand **Image** and **Video Studio** editors. The everyday experience should take seconds: press a shortcut, drag a rectangle, copy or save; press another shortcut, select a source, record screen and audio, then edit or export.

When opened intentionally, the editors provide substantial creative capability: the image editor behaves like a small, approachable Photoshop (layers, groups, selections, text, drawing and adjustments); the video editor behaves like a streamlined Camtasia/Screen Studio (timeline editing, live preview, narration, cursor presentation, zoom/pan, annotations, backgrounds and polished export). Complexity must be progressive: **never force an editor to open for routine capture, and never show advanced controls until their tool is selected**.

The project will be implemented afresh rather than porting WinShotX's current Tauri/React interface or inheriting its recording/cache architecture. Existing WinShotX features are a product reference and regression checklist, not the new implementation's design constraints.

### 1.1 Release outcomes

1. **Capture and gallery work first**, independently of recording and editors: region/window/monitor screenshots, clipboard, local saving, thumbnail browsing and simple file actions.
2. Screen capture remains quick and correct across mixed-DPI, multi-monitor Windows 11 configurations.
3. **Recording comes next**: desktop audio and/or microphone **actually appear in recorded output**, without drift, silent failure or unexpectedly shortened footage.
4. **Image editing follows recording**: beginner-friendly tools, editable layers and native project saving.
5. **Video Studio comes last**: trimming, multitrack overlays, cursor effects, zooms and audio mixing without becoming a full production suite.
6. The app stays unobtrusive when idle: tray-first, no cloud account, no telemetry by default, on-demand editors, and no permanent recording engine unless replay is explicitly enabled.
7. Installer, portable distribution, recovery and updates are polished enough for public Windows 11 release.

### 1.2 Intended audience

Developers documenting bugs, educators recording walkthroughs, support staff creating instructions, creators producing short screen-based videos, and everyday users who want screenshots with annotations. **Not** a professional colour-grading, 3D motion-graphics or live-streaming package.

### 1.3 Non-negotiable development order

Build the product in this sequence, completing and testing each stage before beginning the next:

1. **Phase 1 — Screenshot capture + gallery:** native shell/tray, global hotkeys, frozen region-selection overlay, monitor/window screenshots, clipboard, save, recent captures, thumbnails, search and file management. This must stand alone as a useful lightweight screenshot utility; recording and editor code are not required to use it.
2. **Phase 2 — Screen recording + audio:** direct MP4 recording, desktop audio, microphone, device selection, level meters, accurate timebase, pause/resume, recoverable saves, and video entries/previews in the existing gallery. **Audio reliability is the release blocker for this phase.**
3. **Phase 3 — Image editor:** approachable layered image editor (a lightweight, beginner-friendly Photoshop), non-destructive project files, annotations, basic adjustments, image comparison, OCR/pin workflow where appropriate.
4. **Phase 4 — Video editor / Studio:** approachable Camtasia/Screen Studio-style timeline editor, trim/split, track-based editing, smooth zoom, cursor effects, callouts, audio mixing and dependable exports.
5. **Phase 5 — Public v1.0 hardening:** overall integration, onboarding, accessibility, performance verification, installer, updates and full release tests.

**Do not start the image or video editors to unlock capture, gallery or recording features.** The fast path remains usable even if an editor module fails to load. Complexity is progressive: each editor loads only when opened.

These are **development phases**, not separate products. Version **1.0.0** is the combined, tested result; earlier internal/preview builds may ship phase by phase. Where functionality is deferred, label it clearly rather than displaying dead buttons.

---

## 2. Reference inventory: existing WinShotX fork

Source: https://github.com/ITheFallenI/winshotx (public `main`, reviewed 27 October 2026). README identifies current fork version 0.5.0, upstream roots, and added features. Relevant code includes `src-tauri/src/record`, `record/audio.rs`, `replay.rs`, `platform/snipping.rs`, `src/components/foto`, `src/components/editor`, `src/components/gallery`, and `docs/TRAMPAS.md`.

| Existing capability | V1 decision | Notes |
|---|---|---|
| Region/monitor/window capture, frozen overlay, pixel magnifier and colour picker | **Retain, rebuild** | Correct window-under-cursor targeting and monitor boundaries are mandatory. |
| Window spotlight, theme/custom colour, capture toolbar | **Retain, refine** | Overlay must always be dismissible; editor controls must not contaminate capture. |
| Instant copy, optional save, dated output folders, configurable shortcut flows | **Retain** | One-action workflows, no forced prompts. |
| Layered screenshot editor: select, shapes, arrow, text, step numbers, highlight, pen, pixelation | **Expand** | Proper beginner-friendly layer model and native project format. |
| Groups, lock/hide, reorder/duplicate, resizable panels and undo/redo | **Retain** | A foundation for a lightweight Photoshop-like editor. |
| Gallery, thumbnails, date/type search, previews and recycle-bin delete | **Retain** | Background indexing and lazy thumbnail creation. |
| Two-image reveal/difference comparison | **Retain** | A valuable distinctive tool. |
| OCR, pinned screenshots, sensitive-area masking, opening external images | **Retain** | OCR is local; redact safely. |
| GIF, MP4 H.264/H.265, system sound, recording editor | **Replace core** | Original recording and audio behaviour are not acceptable as a base architecture. |
| Rolling replay buffer (last 15–120 seconds) | **V1 optional toggle** | Separate capped encoded segments, not unconstrained raw frames. |
| Themes, translations, tray, shortcut customisation | **Retain** | Ship English v1; translation-ready; other languages after QA. |

### 2.1 Lessons from the repository

The WinShotX fork contains specific fixes for long static-screen footage, paused recording duration, empty recording export, and fixed-rate frame-timing mismatch. These become **permanent regression tests**. Its current audio module captures WASAPI loopback and microphone samples independently; new v1 must verify the entire device-selection → capture → synchronisation → encode → mux → playback chain. The existing `docs/TRAMPAS.md` records multi-monitor overlay, window activation, input focus and release-build pitfalls; the rewrite should keep those **behavioural lessons** while eliminating WebView/Tauri-specific complexity.

---

## 3. Product structure and navigation

### 3.1 Four workspaces

**Capture:** temporary full-screen frozen-desktop overlay. No full app window needed. Contextual floating strip: Copy, Save, Edit, OCR, Pin, Record. Hover-window spotlight and pixel magnifier are optional and configurable.

**Image:** a dedicated tabbed/dockable image editor with central canvas, compact left toolbar, context-sensitive top options bar, layers panel, optional colour/properties panel, clear Save/Export actions and status zoom bar. Starts in a simple layout; advanced panels can be exposed without switching applications.

**Video Studio:** separate workspace: media/source bin and effects panel at left, large live preview at centre, context-sensitive controls at right, non-destructive multitrack timeline at bottom. Recording overlay/toolbar is a small separate window; editing need not run while recording.

**Gallery/Home:** recent captures and projects, New Screenshot, New Recording, Open Image/Video, search and filters; contextual quick actions. Opens only when requested; tray launch is the default.

### 3.2 Core UX rules

- **Quick mode first:** select/copy/save within the capture overlay; never open a heavyweight editor without an explicit choice.
- **One tool, one clear options strip:** changing brush selects brush settings; choosing video zoom exposes zoom parameters, not 50 unrelated preferences.
- **Direct manipulation:** drag/resize layers and annotations on the image; trim clips and move keyframes directly on the timeline.
- **Destructive export, non-destructive authoring:** working files preserve editability; rasterised images/video are only output formats.
- **Explain advanced terms:** use “Blur sensitive area” and “Make text larger” in the basic UI; advanced properties can show precise terminology.
- **Safe defaults:** use short labelled tooltips, visible shortcut hints, editable presets, gentle first-use guidance, and a contextual undo history.
- **Accessibility:** keyboard operability, high-contrast focus, scalable UI, colour-independent indicators, screen-reader labels where supported.

### 3.3 Suggested default shortcuts

| Action | Default |
|---|---|
| Select screenshot region | `Ctrl+Shift+2` |
| Region straight to image editor (Phase 3 onwards) | `Ctrl+Shift+E` |
| Start/stop recording (Phase 2 onwards) | `Ctrl+Shift+5` |
| Open recent capture gallery | `Ctrl+Shift+G` |
| Save last moments (Phase 2, if buffer enabled) | `Ctrl+Shift+6` |
| Copy region from overlay | `Enter` |
| Open editor from overlay (Phase 3 onwards) | `E` |
| Cancel overlay | `Esc` or right-click release |

All global shortcuts are user-configurable. Registration failures and Windows-reserved shortcuts must be explained rather than silently ignored. Prefer `RegisterHotKey`; do not use persistent low-level keyboard hooks for standard app operation. Elevated-window support must be opt-in, clearly disclosed and isolated.

---

## 4. Screenshot and capture specification

### CAP-01 Sources

**Phase 1 core:** region by drag; visible top-level window; current monitor; explicitly chosen monitor; clipboard/save. The selection shows pixel dimensions and allows adjusting its edges. **Phase 1 refinement, before Phase 1 exit if stable:** all-monitor capture, keyboard nudging, ratio presets, exact coordinate entry and delayed capture (3, 5, 10 seconds). **Later refinement:** auto-detect smaller rectangles/window bounds for faster precision. No image editor is necessary to use these tools.

### CAP-02 Overlay

Pre-capture a frozen frame; show a cursor-position magnifier (minimum 6×), pixel/hex sampler, screen resolution and clear selection border. Optional hovered-window spotlight is determined by visible Z-order, not simply the smallest nested window. Selection must work with displays to the left/above the primary display (negative coordinates), with mixed scaling, HDR/SDR and taskbar/Start visibility. Exclude overlay and recording controls from output whenever Windows capture facilities permit; do not rely on window hiding at unpredictable frame times.

### CAP-03 Capture actions

**Phase 1 core:** copy PNG data to clipboard; save PNG (plus JPEG if straightforward); choose **Instant copy**, **Copy + auto-save**, or **Ask via small toolbar**; configurable output folder and filename template with timestamped dated folders. **Phase 1 refinements:** WebP, copy sampled colour. **Phase 3, once Image exists:** open in Image, copy recognised text, pin an image above windows, and related annotation/redaction actions. Single-page PDF export is a stretch feature for v1. **Never expose an Edit action that does nothing in Phase 1 or 2.**

### CAP-04 OCR and privacy

**Phase 3 (Image/editor milestone):** local Windows OCR; selectable text blocks and one-action copy. Blur, mosaic and solid redaction with editable overlay before export. **Security requirement:** solid redaction must irreversibly replace underlying pixels in flattened output; blurred previews must not be described as secure redaction. Warn when the image being copied contains potentially uncommitted privacy edits.

### CAP-05 Stability

Every overlay has an unconditional Esc/cancel path; time out with an error if rendering fails, never trap the user behind an opaque screen. Rapid repeated shortcuts must reuse or serialise overlay sessions without focus loss or duplicate windows.

---

## 5. Gallery, basic workflows and settings — Phase 1 foundation

### GAL-01 Phase 1 screenshot library (must ship with capture)

The first milestone includes a usable gallery, not a placeholder. Save screenshots directly to a configurable local folder, display the newly captured image immediately in Recent, and support thumbnail browsing, preview, sort/group by date, filename search, rename, copy, open in Explorer and Recycle Bin deletion. All actions work without any editor installed or running.

**Stage expansion:** after recording arrives, index videos, produce poster thumbnails, show duration and play/launch video files; after the image editor arrives, add project entries, edit-open actions and image comparison. Indexing is asynchronous, with no blocking directory scan on application startup.

### GAL-02 Complete v1.0 library

Fast day-grouped view of images, GIFs, videos and project files; name/date/type filters; lazy thumbnails and video poster frames; preview with zoom and pan; two-image compare; multi-select; copy, rename, share via OS share/open-with, reveal in Explorer and soft-delete to Recycle Bin. Operate on original files rather than unnecessarily importing into an application database.

### SET-01 Preferences (add only when the owning feature exists)

**Phase 1:** capture actions and profiles, save directory, filename templates, global shortcuts, cursor/magnifier/spotlight options, tray left-click behaviour, Windows theme/light/dark and UI scaling. **Phase 2:** video codecs, FPS, quality, default audio endpoints, recording countdown and replay buffer. **Later:** editor preferences and project recovery settings. Auto-start remains opt-in; updates are manual or configurable. Export/import user preferences. Detect failed shortcut bindings, unwritable folders and missing microphone permissions/devices with specific error messages.

### AUTO-01 Workflow automation

Expose internal command actions behind a typed action dispatcher (e.g. `capture.region`, `editor.open`, `record.start`, `gallery.open`) but **do not** ship an arbitrary script runner or network upload service in 1.0. Optional simple CLI flags for capture/record may follow as a post-1.0 feature once safety, single-instance logic and test coverage are mature.

---

## 6. Recording engine — Phase 2, after capture + gallery

**Critical:** Do **not** make ordinary MP4 recording depend on a growing temporary archive of all frames. Record directly to an encoded, recoverable media stream; maintain an independent project/session manifest only for optional post-capture cursor effects, edit decisions, markers and thumbnails. A segmented rolling replay buffer is a separate subsystem.

### REC-01 Sources and format

Region, window or selected monitor. Defaults: **1080p at 30 FPS or source native resolution up to 60 FPS according to selected profile**, H.264 video, AAC audio, MP4 container, 48 kHz stereo audio. User can choose quality, 30/60 FPS, cursor inclusion, desktop sound, microphone and recording countdown. 1440p/4K should be supported on tested hardware without forcing that resolution on low-spec machines. H.265 optional where hardware encoders and licence conditions permit; H.264 is universal fallback. **Never claim guaranteed lossless capture, exact FPS or zero frame loss across all hardware.**

### REC-02 Audio capture (release blocker)

Capture desktop output via **WASAPI loopback** and microphone via selected Windows input endpoint, each as independently timestamped input streams. Each can be enabled or disabled, changed before recording and monitored with a **live peak/RMS level meter**. Provide source selection, mute, gain and a clearly labelled short **Record audio test** action before first full recording. Prefer preserving original input streams in the editing project; create correctly mixed AAC stereo output for the simple direct-recording workflow.

**Mandatory behaviours:**
- No silent success when the requested audio device is missing, busy, denied or returning zero samples.
- Display device names and real capture activity; distinguish actual silence from failed source capture.
- Timestamp video and both audio sources from a single monotonic media clock; resample and correct drift without clicks or large discontinuities.
- If a device disconnects during a session, continue video where possible, show a warning, and preserve the recorded part; state in the result what was lost.
- Loopback must record the chosen output device's actual sound; on output-device changes, either rebind safely or stop that audio track with an explicit warning.
- Test both desktop-only, microphone-only, both combined, neither, pause/resume, headset connect/disconnect and long quiet sections.

### REC-03 Time correctness and interruptions

MP4 duration must follow session clock minus paused time, **not** timestamps of last changed frame. Windows Graphics Capture may produce no frame while a desktop is still: the encoder must maintain the correct video timeline by repeating frames or emitting correct durations for its chosen frame-rate mode. Last-frame static periods cannot truncate the file. Pause/resume must not append phantom silence or shift narration. Stopping while paused must be correct. Finalisation must be atomic: write temporary/intermediate fragments; finalise and rename on success; retain recoverable material after interruption.

### REC-04 Controls

Small floating start/stop/pause toolbar, elapsed clock, live audio meter, source warning indicator, optional cursor highlight, and one-click mark/chapter point. Hide toolbar from recording output wherever supported. Keyboard shortcuts always visible and rebindable. Show recording status in the tray; never leave an ambiguous invisible active capture.

### REC-05 Live encoded workflow and replay

Use bounded GPU frame queues and bounded audio buffers. Video encode and mux run off the UI thread; backpressure drops or duplicates video frames with diagnostics, never stalls global input. Resource usage and dropped-frame counters are measurable. Rolling replay (off by default) writes short encoded segments with bounded disk usage and clear actual-buffered duration; “Save last X seconds” remuxes or joins segments without interfering with ongoing capture. Warn about replay's background disk/CPU cost.

### REC-06 Out-of-scope restrictions

Protected/DRM surfaces, secure desktops/UAC consent screens and content blocked by Windows capture are not guaranteed capturable. Do not attempt to bypass operating-system protection or hide capture from users.

---

## 7. Image Editor — Phase 3, lightweight Photoshop for beginners

**Vision:** Screenshot annotation first, genuine image editing second. The full editor should be capable of making documentation screenshots, simple thumbnails and visual assets, without attempting Photoshop's professional feature set.

### IMG-01 Document and layers

- Multiple open document tabs with drag-and-drop image import and clipboard paste.
- Layer stack: raster image layers, vector shape layers, text layers, annotation layers, group/folder layers, effect/adjustment layers where implemented.
- Move/transform, resize, rotate, opacity, blend modes (Normal, Multiply, Screen, Overlay in v1), visibility, lock, rename, duplicate, reorder and group/ungroup.
- Canvas resize, document resize, fit, 100%, zoom-to-pointer, snapping, ruler/grid (toggle), alignment and distribution controls.
- Pixel-accurate compositing and proper alpha/transparency. Mixed DPI affects UI scaling, not exported image dimensions.

### IMG-02 Toolset (v1 required)

| Group | Tools |
|---|---|
| Select & arrange | Move, transform, rectangular/elliptical marquee, basic freeform lasso, align/distribute |
| Crop & geometry | Crop, rotate, flip, resize, aspect-ratio presets |
| Drawing | Pencil/brush, eraser, straight line, arrow, rectangle/rounded rectangle/ellipse, fill bucket |
| Communication | Text, numbered steps, callouts, speech boxes, highlighter, spotlight/dim exterior |
| Privacy | Pixelate, blur, **solid irreversible redaction** on export |
| Basic photo corrections | Exposure/brightness, contrast, saturation, temperature, grayscale, simple sharpen/blur |
| Colour | Eyedropper, recent swatches, hex/RGB input, gradients for shapes/backgrounds |

Layer effects should preview live and stay editable where feasible. For v1, prioritise reliable common tools over sophisticated AI retouching, vector path editing, masks or Photoshop format compatibility.

### IMG-03 Beginner experience

A **Quick Edit** preset exposes arrows, text, numbered steps, blur/redact, crop and export. **Full Editor** reveals layers, adjustments and selections; both use the same document and there is no conversion penalty. A properties panel explains the selected object in everyday language. Hover previews, sensible colour/line defaults, and a small “How do I…?” command palette cover common tasks. Undo and redo work across creation, movement, grouping, cropping, painting and styling.

### IMG-04 Saving, importing and exporting

**Native format:** versioned `.capshotx` project container (ZIP-like layout or an equivalent documented archive) with JSON manifest, assets, per-layer data, effects and history metadata; all paths are relative; atomic saves; migration on schema evolution. Output: PNG, JPEG, WebP; copy rendered image to clipboard; export at chosen scale with optional metadata stripping. Import PNG, JPEG, BMP, WebP and clipboard images. **PSD import/export deferred** until compatibility can be tested thoroughly; do not advertise it in v1.

### IMG-05 Comparison and recovery

Two-image reveal slider, side-by-side and visual difference with tolerance, zoom and export. When closing unsaved projects, offer Save/Discard/Cancel. Autosave recovery snapshots should survive crashes; restore prompt must identify document, timestamp and recovery status.

**Image-editor acceptance example:** user captures a window, adds three text and shape layers, groups and locks them, adjusts contrast, saves a project, reopens it with every item editable, then exports a visually identical PNG.

---

## 8. Video Studio — Phase 4, lightweight Camtasia + Screen Studio

**Vision:** Turn raw screen recording into a polished tutorial without opening a separate editor. The immediate workflow is **Trim → Improve cursor/zoom → Add callouts/audio → Export**. More advanced editing exists but does not obscure that path.

### VID-01 Media and timeline

- Non-destructive multitrack timeline for video, audio, text, shapes and images.
- Import MP4 video, PNG/JPEG/WebP images, MP3/WAV/AAC audio where decoders are available.
- Waveform generation, thumbnails, snapping, magnetic playhead, zoomable timeline and keyboard scrubbing.
- Split, trim, slip/move, ripple-delete, duplicate, rearrange; playback speed presets (0.5×, 1×, 1.5×, 2×) with clear audio-handling rules.
- At least one video track, multiple overlay tracks and independent microphone/system-audio tracks; explicit mute/solo and per-track gain.
- Undo/redo, autosave, relink missing files and full project reopen.

### VID-02 Studio look and presentation

- Background presets: solid colour, gradient, blurred image and custom imported image.
- Frame padding, crop-to-focus, corner radius and drop shadow; presets for 16:9, 9:16, 1:1.
- Zoom and pan with intuitive on-canvas handles, easing presets, simple keyframes and one-click reset.
- Cursor visualisation captured as **metadata separate from video pixels when possible**: highlight ring, click pulse, optional size smoothing, pointer replacement/visibility and keystroke display with privacy controls.
- Event-based auto-zoom around clicks is a **guided optional feature**, never forced. Preview rendering and export rendering must share the same transforms and interpolation logic to avoid surprises.
- Text titles, arrows, circles, boxes, blur/pixelation, numbered callouts and static logo/image overlays.
- Simple transitions (cut, fade, dissolve), opacity fades, audio fades and clip volume envelopes.

### VID-03 Editing modes

**Quick Studio** is the default: large preview, simplified timeline, Cut / Zoom / Cursor / Captions / Audio / Export tabs and action suggestions. **Advanced timeline** exposes tracks, keyframes and inspector panels, with the same native project. A first-run example tutorial project demonstrates trim, auto-zoom and callouts in under five minutes.

### VID-04 Audio editing

Independent desktop and microphone tracks when recorded in a project; gain, mute, solo, fades, basic noise-reduction preset (only if quality can be demonstrated), simple voice-priority ducking (stretch for v1) and output loudness metering. Import voiceovers. A preview must produce the same timing and mix as export. Unsupported effect or audio hardware failures must show a visible fallback rather than an incomplete “successful” video.

### VID-05 Export

H.264/AAC MP4 at 720p, 1080p and 1440p/4K when supported; 24/30/60 FPS output; bitrate/quality presets: Small, Balanced, High. Independent output resolution from source; predictable aspect framing. GIF export for selected short, silent sections with palette/size/duration warnings. Export with progress, cancel and retry, with reproducible quality/settings. Preserve unsaved project after export.

### VID-06 Video project format

**Native format:** `.capshotx-video` project folder/container with schema version, linked/embedded media rules, timeline tracks, edits, transitions, cursor events, animation keyframes, audio routing, project settings and thumbnail caches. Store capture source metadata (DPI, monitor transform, cursor event timestamps) separately. Default to **linked originals** to avoid huge duplicate files; provide “Collect project files” for portability.

### VID-07 Explicitly deferred to v1.1+

Full green-screen compositing, advanced chroma-key, complex motion tracking, multi-camera editing, AI text-to-video, AI voice cloning, automatic transcription/subtitles, cloud collaboration, livestreaming, 3D compositing, professional LUT/colour-grade workflow, elaborate particle effects and plugin SDK. *Manual captions/text overlays* are included in 1.0; automatic speech transcription is a later feature unless a small dependable local implementation is proven in beta.

**Video-editor acceptance example:** record a four-minute Windows walkthrough with desktop sound and narration, delete a mistake, zoom into a menu, highlight three clicks, add a title and fading music, save/reopen the project and export an MP4 whose picture, audio and duration match preview.

---

## 9. Technical architecture

### 9.1 Primary stack (proposed, subject to spike tests)

| Layer | Proposed implementation | Reason |
|---|---|---|
| Application and UI | Rust 2024, `egui` + `eframe` | Native Rust UI, efficient for tool-heavy interfaces; no JS/WebView runtime. |
| GPU canvas and preview | `wgpu`/`egui-wgpu`, with D3D interoperability where needed | GPU compositing, layer transforms, video preview and texture reuse. |
| Windows integration | `windows` crate, Win32 + WinRT APIs | Monitor enumeration, hotkeys, tray, clipboard, DPI and native device management. |
| Screen capture | Windows Graphics Capture first; fallback path after tests | GPU-native capture and source selection. |
| Video encode/decode | Windows Media Foundation (H.264/AAC Sink Writer; decode path with Source Reader or transforms) | Native encode, avoid a mandatory separately shipped media framework. |
| Audio capture | WASAPI loopback + capture endpoints | System sound and mic with independent clocks and diagnostics. |
| Raster formats | Rust `image` crate, optionally specialised encoders | PNG/JPEG/WebP; explicit pixel format conversions. |
| App state and projects | `serde` JSON/TOML + versioned manifest, atomic saves | Evolvable documents and readable diagnostics. |
| Gallery metadata | Lightweight SQLite or file index, background worker | Responsive filtering without scanning all files on every launch. |
| Logging and faults | `tracing` + rotating local logs; `thiserror` | Debugging without recording private content. |
| Tests/CI | `cargo test`, Windows integration harness, golden-file media fixtures | Release confidence. |

**Architecture risk:** `egui` is productive for tools but complex Photoshop-like editing needs a custom canvas renderer and carefully designed drag/drop behaviour. Before committing, run a proof of concept: 4K textured canvas, 100+ composited layers, timeline scrubbing, 3-monitor overlay and smooth focus/transparency. The UI library choice is not final until those tests pass.

### 9.2 Suggested Rust workspace

```text
capshotx/
  Cargo.toml                 # Cargo workspace
  crates/
    app/                     # Startup, single instance, tray, settings, routing
    ui/                      # Shell, themes, editor widgets, panels
    platform-windows/        # Win32/WinRT, clipboard, hotkeys, monitors, WASAPI
    capture/                 # WGC capture, region/monitor/window, frozen overlay
    image-document/          # Layers, brushes, edit operations, project files
    image-render/            # Compositing, raster effects, image export
    recorder/                # Video capture, audio streams, clocks, recovery
    media/                   # Encode/decode/mux, format abstraction, thumbnails
    video-document/          # Timeline, clips, keyframes, non-destructive edits
    video-render/            # Live preview, render graph, studio effects
    gallery/                 # Library index, lazy thumbnail pipeline
    shared/                  # Geometry, error types, events, commands
  integration-tests/         # Capture/playback fixture and hardware tests
  assets/                    # Icons, default presets, tutorial sample
  packaging/                 # Installer and portable distribution
  docs/                      # Architecture decisions, API, QA matrix
```

### 9.3 Concurrency and interfaces

UI thread handles only user interaction/rendering. Capture session runs on dedicated capture thread. Audio endpoints each have independent high-priority acquisition paths with bounded buffers and timestamps. Encode/mux runs on background worker(s). Thumbnail creation and media indexing are lower priority. A well-defined application command/event bus carries typed messages and result errors: `StartCapture`, `BeginRecord`, `PauseRecord`, `StopRecord`, `OpenDocument`, `Export`, `MediaWarning`. No feature is permitted to block the UI while waiting for device creation or file finalisation.

### 9.4 Shared editing primitives

Image editor and Video Studio should share: geometry/transforms, layer rendering, text layout, colour model, shape/path primitives, effects where compatible, keyframe interpolation, media asset manager, undo/redo command model, project versioning and common export jobs. **Do not force them to share one giant document type.** Still images and video timelines have different lifetime, caching and playback requirements.

### 9.5 Recording pipeline sketch

```text
Selected WGC window/monitor/region --> GPU frame queue --+
                                                        |
WASAPI render-device loopback --> timestamped audio --+--> sync/compositor --> encoder --> mux --> .partial --> .mp4
                                                        |
WASAPI microphone capture --> timestamped audio ------+
                                                        |
Mouse/cursor/selection events --> metadata sidecar -----+--> Video Studio project (optional)
```

Use one monotonic session origin; convert endpoint timestamps, resolve drift, and test accurate duration. Preserve separate audio stems in project mode if selected. Standard instant-record saves a playable MP4 directly; Studio mode additionally saves source recordings and metadata for later non-destructive editing. After a crash, use partial fragments to recover what can be recovered and never delete partial data before verification.

### 9.6 Security, licensing and privacy

No sign-in, accounts, mandatory analytics, background uploads or remote media processing in 1.0. Use OS-provided secure storage for any later credentials; not needed for 1.0. Do not log captured pixel data, OCR content or raw microphone buffers. Recording and replay status visible to users; file deletion follows Windows Recycle Bin unless permanent deletion is explicitly chosen. Review third-party crate licences, patent/licensing consequences of codec choices and the prior project's MIT attribution requirements if any code/assets are actually reused.

---

## 10. Performance and quality budgets (engineering targets)

**Targets to measure on a defined Windows 11 test machine; not performance guarantees.** Record baseline hardware in benchmark report and compare at 1080p and 4K, single-/multi-monitor and idle/recording/editor cases.

| Metric | Target / release threshold |
|---|---|
| Idle (tray only) | Aim under 75 MB private working set, ≤0.5% average CPU on baseline machine; no capture loop active. |
| Open region overlay | P95 ≤350 ms warm start at 1080p on baseline machine. |
| Capture to clipboard | P95 ≤500 ms after selection (not counting user drag). |
| Editor responsiveness | No >100 ms UI stalls from routine brush/transform/undo actions at 1080p; investigate long frames. |
| Recording stability | Continuous 60-minute 1080p30 recording without failure or audio loss in supported hardware test matrix. |
| A/V sync | End-to-end audio/video offset ideally within ±50 ms in automated fixture; target no perceptible drift over 60 minutes. |
| Duration accuracy | Export differs from wall-clock-minus-pauses by ≤one video frame plus container rounding. |
| Resource containment | All queues and replay caches bounded with explicit limits and warnings. |
| File recovery | Aborted or crashed recording leaves recoverable media or clear failed-state metadata; no silently discarded data. |
| Installer | No admin requirement for standard per-user installation; portable version supported if validation passes. |

Do not optimise installer size at the expense of audio correctness, editor stability or maintainable code. “Lightweight” means **fast to use, quiet when idle, responsive and not bloated**, not necessarily the smallest executable on disk.

---

## 11. Implementation roadmap and release gates

These milestones are **strictly in build order**, not date estimates. **Capture + Gallery precede recording; both precede Image; Image precedes Video Studio.** A gate is complete only when tested on actual Windows 11 devices. Avoid creating empty modules or UI panels for later phases before their phase begins.

| Gate | Phase / deliverable | Required proof before progressing |
|---|---|---|
| M0 — Rust foundation | Basic native shell, tray, settings persistence, shortcuts, single-instance guard, monitor/DPI enumeration | App starts, exits cleanly, survives shortcuts, handles a 3-monitor mixed-DPI setup. |
| M1 — Screenshot engine | Region, monitor and visible-window selection; frozen overlay; instant clipboard and local PNG save | Correct pixel capture; overlay dismissible; rapid repeat captures; no editor or recorder dependency. |
| M2 — **Gallery first** | Recent screenshot list, date grouping, thumbnails, search, preview, rename, reveal and Recycle Bin deletion | 1,000+ screenshot folder remains responsive; saved capture appears automatically; file actions work. **Phase 1 complete.** |
| M3 — **Recording with audio** | WGC capture, WASAPI system sound and mic, device selection/meters, H.264/AAC MP4, pause/resume, error visibility | Audible, synchronised system/mic/both recordings and correct static-screen durations; new videos appear in gallery. **Phase 2 complete.** |
| M4 — Image editor | Layer model, annotations/paint/text, selection/crop, grouping, undo/redo, basic image adjustments, native project save/reopen | 25-layer test project reloads and remains editable; screenshot quick capture still runs without opening editor. **Phase 3 complete.** |
| M5 — Video Studio core | Decode, timeline, multitrack audio/video, cuts/splits, waveforms, project save/reopen and export | Tutorial recording imports, trims, reopens and exports with audio intact. |
| M6 — Studio polish | Eased zoom/pan, cursor/click effects, callouts, backgrounds, audio mix, preview/export parity | 4-minute demonstration edit has no drift, missing effects or mismatched preview/export. **Phase 4 complete.** |
| M7 — **1.0 release hardening** | Gallery integration, OCR/comparison/pinning as scoped, accessibility, profiling, installer, recoverability, updates, docs | Complete QA matrix green, clean install/uninstall, zero open release-blocking bugs. **Public 1.0.0.** |

### 11.1 Preview build progression

- **First usable build (Phase 1):** screenshots plus gallery only; no recording or editor requirement. It must already feel fast and complete for basic screenshots.
- **Recording preview (Phase 2):** screen + desktop/mic sound + gallery video playback; **do not advertise audio recording as working until real playback fixtures pass**.
- **Image preview (Phase 3):** layered editor with a readable project format, preserving the one-key capture workflow.
- **Studio preview (Phase 4):** video editor is the final major feature area; it must operate on existing recordings without compromising the recorder.
- **Public v1.0.0:** all four phases integrated and reliable; advanced optional effects can be deferred in preference to recording, capture and save integrity.

### 11.2 Scope discipline

A finished phase must remain stable during later development. Maintain regression suites for capture and gallery when recording is added; maintain capture/gallery/recording suites when Image is added; and keep all three independent when Studio is added. **Never restructure the working capture engine merely to support an editor feature.**

---

## 12. Acceptance criteria and QA test matrix

### 12.1 Critical release-blocker scenarios

1. **Desktop sound only:** record browser/media playback for 3 minutes; resulting MP4 contains identifiable continuous sound and matching picture.
2. **Microphone only:** record 3 minutes of spoken test markers; input meter moves, exported track is audible, correct device name is displayed.
3. **Both sources:** desktop sound and microphone remain distinguishable and synchronised; no unexpected double-playback, harsh clipping or hidden silence.
4. **Static desktop:** record 30 seconds of movement, 60 seconds of unchanged image, then 30 seconds of movement; output duration and resumed visuals are intact.
5. **Pause/stop:** start, pause 20 seconds, resume, stop while paused in a separate test; duration excludes paused intervals with no unexpected appended audio.
6. **Audio disruption:** disconnect microphone or change default playback device mid-recording; visible warning, recoverable file and documented behaviour.
7. **Crash/recovery:** terminate application while recording or editing; no project corruption; recover as much media as possible.
8. **Mixed monitors:** 100% + 150% DPI, one monitor left of primary, 1080p + 4K; selection dimensions and outputs match intended region.
9. **Editor round trip:** 25+ layers, locked/grouped objects, text, blur, transformations; reopen and continue editing without rasterising layers.
10. **Studio parity:** trim, zoom, cursor highlight, text overlay and voice/music mix; preview agrees with final output at marker timestamps.
11. **Hotkey contention:** press same shortcut quickly, use Windows reserved keys, display locked/secure desktop; no persistent invisible overlay or lost controls.
12. **Export interruptions:** cancel export, disk full, invalid folder, unsupported codec, missing import; appropriate error and intact original project.

### 12.2 Testing strategy

- **Unit:** timeline/timebase arithmetic, audio sample conversion, transform math, image layer tree operations, history/undo, persistence schema migration.
- **Media golden files:** generate deterministic audio tones/timecode video, then decode MP4 and compare timestamp/duration against expected data. Inspect both mux metadata and decoded PCM; a non-empty audio stream alone is **not** sufficient proof of audible recording.
- **Windows integration:** real WASAPI devices, WGC window/monitor capture, message-loop focus, foreground behaviour, tray shortcuts and clipboard across DPI setups.
- **Long run and soak:** 60/120-minute test recordings, repeated 30-second sessions, start/stop while idle, sleep/wake, hot-plug monitors and audio devices.
- **Security/privacy:** redaction exported pixels, secrets absent from logs, updates validated, no network call without explicit opt-in.
- **Usability:** first-time user can create a screenshot and annotated PNG, then record/edit/export a short tutorial without documentation.

### 12.3 Public 1.0 definition of done

**Zero known blockers** causing missing audio, corrupt MP4, wrong-length recording, unrecoverable project loss, frozen/uncancellable capture overlays, wrong-monitor capture or broken save/reopen. All M0–M7 required exit criteria pass on clean Windows 11 x64 installations. Every advertised feature has a user-facing description, tested interaction and at least one automated or recorded manual acceptance test. Known limitations are published clearly.

---

## 13. Naming, product identity and release conventions

**Chosen product name:** **CapShotX**. Use one installer and one process with three clearly labelled areas:

| User-facing area | Purpose | Typical first action |
|---|---|---|
| **CapShotX Capture** | Screenshots, GIF/MP4 screen recording, hotkeys and tray | Drag a region or start a recording |
| **CapShotX Image** | Beginner-friendly, layered image editing | Annotate, retouch, save an editable image |
| **CapShotX Studio** | Camtasia-style video editing and presentation | Cut, zoom, highlight clicks, mix audio and export |

The gallery is integrated into the home view rather than marketed as a separate application. Keep the product name out of persisted internal type and command names where possible so file formats remain stable if branding changes.

**User-facing descriptor:** “Capture. Edit. Record.” **Brand direction:** simple, recognisable Windows utility naming, modern but restrained visual design, light/dark themes and unambiguous capture/recording iconography. Do not imitate ShareX or WinShot icons or imply association with either project.

**Repository and release conventions:** suggested repository `capshotx`; binary `CapShotX.exe`; installer title `CapShotX`; SemVer releases beginning `1.0.0` after acceptance gates; release notes list fixed bugs and known limitations. Use `.capshotx` for image projects and `.capshotx-video` for Studio projects provisionally; finalise extensions before the first publicly distributed beta, along with file associations and migration tests.

**Availability warning:** The name has been selected by the project owner, not confirmed available. Before promoting or distributing under it, check the UK Intellectual Property Office trade mark database, Companies House, similar applications and spellings, GitHub naming and relevant domains. Do not present an unchecked name search as legal clearance.

---

## 14. Initial engineering backlog — start with screenshot + gallery

**Sprint A — application skeleton:** Create the Cargo workspace, native application shell, system tray, single-instance detection, settings persistence, logging, and configurable hotkey registration. Confirm multi-monitor and mixed-DPI coordinates.

**Sprint B — screenshot flow:** Implement WGC-backed frozen-screen capture, region selection, window hover targeting, monitor selection, Esc/cancel, PNG encoding, clipboard delivery and optional automatic saving. Keep the capture path independent from every editor.

**Sprint C — gallery MVP:** Build folder-based capture indexing, lazy thumbnails, day grouping, search, preview, rename, open folder and Recycle Bin deletion. Save a screenshot and see it in the gallery without reopening the application. Ensure large folders do not stall the UI.

**Phase 1 exit:** Release a functional screenshot-and-gallery preview build with test logs and measured responsiveness. **Only then begin the recording engine.** Video/audio proof-of-concept is Phase 2; image composition is Phase 3; timeline research and implementation are Phase 4.

### 14.1 Technical decisions at the relevant phase gate

1. **Phase 1:** WGC wrapper versus direct `windows` bindings for screenshot capture and correct window/monitor selection.
2. **Phase 1:** `eframe` viewports versus Win32 overlay windows for reliable multi-monitor selection and input focus.
3. **Phase 2:** Media Foundation direct H.264/AAC writing, optional alternative media backend if native tests fail, and crash-recoverable recording.
4. **Phase 3:** image layer/compositor storage, Undo/Redo operation model and editable project format.
5. **Phase 4:** project storage for large linked video sources, timeline preview cache, GPU compositor and export render parity.

These are intentional **engineering decisions**, not hidden dependencies or untested guarantees.

---

## 15. Repository documentation deliverables

The new repository should contain a concise `README.md` for users, `docs/DEVELOPMENT.md` (this specification), `docs/ARCHITECTURE.md` for engineering decisions, `docs/RECORDING_TEST_MATRIX.md` for audio/video fixtures and device tests, `docs/FILE_FORMATS.md` for editable project schema, `docs/BUILDING.md` for a reproducible Windows build, and `CHANGELOG.md` for releases. Add a visible **Known limitations** section to public documentation.

This document can be copied into `docs/DEVELOPMENT.md` unchanged. It defines release scope and acceptance, while implementation-specific API contracts should live in the dedicated architecture documentation once the initial spikes are complete.

---

## 16. Sources and verification notes

- WinShotX fork (README and implementation): https://github.com/ITheFallenI/winshotx
- WinShotX architecture/Windows trap documentation: https://github.com/ITheFallenI/winshotx/tree/main/docs
- `egui`/`eframe` current docs: https://docs.rs/eframe/latest/eframe/
- Windows Graphics Capture documentation: https://learn.microsoft.com/en-us/windows/uwp/audio-video-camera/screen-capture
- WASAPI loopback recording: https://learn.microsoft.com/en-us/windows/win32/coreaudio/loopback-recording
- Media Foundation Sink Writer: https://learn.microsoft.com/en-us/windows/win32/medfound/sink-writer
- UK Intellectual Property Office trade mark search: https://www.gov.uk/search-for-trademark

**Specification note:** This is a proposed v1.0 development baseline, not a claim that CapShotX is already implemented or tested. Technology choices depend on early hardware proofs; name clearance is an independent pre-release requirement.
