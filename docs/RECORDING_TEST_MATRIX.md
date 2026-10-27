# Recording Test Matrix (Phase 2 / M3)

Verification scenarios, automated fixtures, and hardware checks for the CapShotX
screen-recording engine (`crates/media`, `crates/recorder`, `crates/platform-windows`).

## 1. Automated Verification Suite

Run all deterministic unit and integration fixtures with:

```sh
cargo test --workspace
```

| Test | Crate | What it proves |
|---|---|---|
| `mp4_round_trip_keeps_duration_and_audible_audio` | `media` | Fragmented H.264 + AAC MP4 round-trips at exact duration with audible decoded 440 Hz tone and readable RGB poster frame. |
| `sparse_video_frames_still_give_full_duration` | `media` | Long-duration frames across static screens preserve exact container duration. |
| `excludes_pauses` / `stop_while_paused_keeps_duration_at_pause` | `recorder` | `SessionClock` omits paused intervals and never includes trailing pause duration if stopped while paused. |
| `resume_then_immediate_pause_preserves_elapsed` | `recorder` | Back-to-back resume/pause transitions preserve accumulated active time without boundary underflow. |
| `gaps_become_silence_and_timing_holds` | `recorder` | Silent WASAPI loopback stretches fill with zero PCM so resumed sound lands on its exact timestamp. |
| `small_jitter_stays_continuous` | `recorder` | Endpoint timestamp jitter within 20 ms (`RESYNC_FRAMES`) stays sample-continuous without clicks or holes. |
| `sources_are_summed_and_limited` | `recorder` | Simultaneous desktop + microphone sources sum cleanly and soft-clip above 0.8 without int16 wrap-around. |
| `mono_and_resampled_input` | `recorder` | 44.1 kHz mono endpoints resample accurately onto the 48 kHz stereo timeline. |
| `mute_preserves_gain_and_caps_huge_gaps` | `recorder` | Live mute zeroes mixed output and restores per-track gain on unmute; anomalous QPC jumps cap buffer growth at 10 s (`REC-05`). |
| `instant_stop_produces_valid_mp4` | `recorder` | Stopping immediately after start writes at least one complete frame slot and finalises a valid MP4 (`MF_E_SINK_NO_SAMPLES_PROCESSED` regression). |
| `recovers_unfinalised_fragmented_partial_and_keeps_corrupt_partial` | `recorder` | Dropping `Mp4Writer` mid-recording without `Finalize()` leaves playable 2 s fMP4 fragments recovered as `(recovered).mp4`, while corrupt `.partial` files are never deleted. |
| `default_template_routes_images_and_videos_under_date_folder` | `capture` | Outputs route to `<YYYY-MM>/Images/*.png` and `<YYYY-MM>/Video/*.mp4` (including legacy templates without `{type}`). |
| `creates_excludes_toggles_and_drops_cleanly` | `platform-windows` | Live recording outline sets `WDA_EXCLUDEFROMCAPTURE` (verified via `GetWindowDisplayAffinity`), transitions countdown/recording/paused states, toggles visibility live, and cleans up on drop. |

## 2. Live Hardware A/V Sync & Pause Check

Run the end-to-end hardware fixture on a live Windows desktop:

```sh
cargo run -p recorder --example record_check
cargo run -p recorder --example record_check -- --mic
```

Assertions verified automatically:
1. **Duration accuracy:** 2.0 s active + 1.5 s paused + 2.0 s quiet/static active = `4.0 s ± 0.15 s` container duration.
2. **Audio/video length parity:** decoded PCM length matches container video duration within `± 0.15 s`.
3. **Loopback capture:** 440 Hz tone played at `t = 0.30 s` is audible (`RMS > 0.05`) and lands within `0.30 s ± 0.10 s`.
4. **Pause isolation:** 440 Hz tone played while paused (`2.0–3.5 s` wall clock) is absent from the output MP4.
5. **Silent/still stability:** 2.0 s of silent desktop + static screen after resume neither shortens the file nor shifts sync.

## 3. Device & Workflow Scenario Matrix

| Scenario | Expected Behaviour | Verification |
|---|---|---|
| **Camtasia bounds staging (`ConfirmBounds`)** | Initial picker sets bounds; live desktop frame allows resizing (8 handles), moving, `Full`/`1080p`/`720p`/`Reselect`, click-through inside interior, live `Sys`/`Mic` toggles & meters, then `Start` (`Enter` or `Ctrl+Shift+5`). | Manual + UI workflow |
| **Fast record (`Fast`)** | Selecting region/window/`F` skips staging and begins countdown/recording immediately. | Home `Fast record` card, Tray menu, Settings mode |
| **Output routing** | Screenshots save under `<save_folder>/<date>/Images/*.png`; videos save under `<save_folder>/<date>/Video/*.mp4`. | Unit tests + live capture/record |
| **48 kHz vs 44.1 kHz endpoints** | Resampler converts non-48 kHz inputs smoothly to 48 kHz stereo AAC without pitch or duration drift. | `mono_and_resampled_input` + USB/Bluetooth headset check |
| **Silent desktop loopback** | Encoder mixes zero-filled gaps across silent stretches; audio starting mid-recording aligns with video. | `gaps_become_silence_and_timing_holds` + `record_check` |
| **Device unplugged mid-recording** | `AudioSource` marks source disconnected, floating toolbar displays amber warning banner, video + remaining audio continue and save cleanly. | Unplug USB mic/headset during recording |
| **Minimised target window** | Starting capture on an iconic window fails immediately with `"The selected window is minimised — restore it to record"`. | Minimise window during countdown |
| **Target window resized or closed** | Resizing recreates the WGC frame pool cleanly; closing sets `video_lost`, warns on toolbar, repeats last frame, and saves valid MP4. | Resize/close recorded window |
| **Mixed-DPI monitor drag (`PerMonitorV2`)** | Dragging either the pre-record staging bar or the live recording toolbar across displays with different DPI scales handles `WM_DPICHANGED`, resizes the window, and rebuilds Direct2D targets crisply. | Drag floating bars across 100% / 150%+ monitors |
| **Process crash mid-recording** | Fragmented `.mp4.partial` flushes every 2 s; next launch recovers playable `<stem> (recovered).mp4` and shows toast; corrupt partials stay untouched. | `recovers_unfinalised_fragmented_partial_and_keeps_corrupt_partial` |
