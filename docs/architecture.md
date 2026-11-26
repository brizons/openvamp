# Architecture — OpenVamp

**Decision:** a Rust audio engine built as a hexagonal core, shared by one UI per platform family: SwiftUI for iPhone and iPad, Jetpack Compose for Android, and GPUI (Rust) for Mac, Windows and Linux. The default build uses only permissive or weak-copyleft dependencies, so the app can ship on every store, including the Apple App Store. This document supports the [MVP spec](mvp-spec.md).

> **Status:** Decision 1 (stack) decided on 2026-10-30; on 2026-11-19 the desktop shell moved from Tauri with React to GPUI, and the Mac moved from the SwiftUI app to the GPUI app. Decision 5 (project license) decided on 2026-11-19: MPL-2.0. The Rust core (`domain`, `engine`, `library`, `render-cli`) is built and reviewed on `feat/core-render-pipeline`.

## Decisions at a glance

| # | Decision | Proposal | Status |
|---|----------|----------|--------|
| 1 | Engine and UI stack | Rust core + one UI per platform family (SwiftUI, Compose, GPUI) | Decided |
| 2 | Engine model | One sample clock driving a compiled timeline | Proposed |
| 3 | Threads | Real-time audio thread fed by lock-free queues | Proposed |
| 4 | Key and tempo change (v0.2) | Offline render with Signalsmith Stretch | Proposed |
| 5 | Licenses | MPL-2.0 project; permissive dependencies by default; GPL-only features are opt-in | Decided |
| 6 | Storage | Open song package, stems stored as FLAC | Proposed |

## System overview

```text
┌─ SwiftUI ───────┐ ┌─ Compose ─────────┐ ┌─ GPUI (Rust) ───┐
│  iPhone · iPad  │ │      Android      │ │ Mac·Win·Linux   │
└────────┬────────┘ └─────────┬─────────┘ └────────┬────────┘
   UniFFI (Swift)      UniFFI (Kotlin)     direct Rust calls
         │                    │                    │
┌────────▼────────────────────▼────────────────────▼─────────┐
│  app        use cases, library, MIDI mapping, settings    │
├───────────────────────────────────────────────────────────┤
│  domain     Song, Arrangement, Setlist, TempoMap,         │
│             timeline compiler            (pure, no I/O)   │
├───────────────────────────────────────────────────────────┤
│  engine     transport, stem readers, click, guide,        │
│             mixer, router                (real-time)      │
├──────────────────────── ports ────────────────────────────┤
│  AudioOutput · MidiInput · SongStorage · Decoder · Stretch│
└──────┬─────────────┬─────────────┬─────────────┬──────────┘
     cpal          midir       file system   symphonia, signalsmith-stretch
```

Each shell sends commands and reads state snapshots (~30 Hz) through the same `app` API. Dependencies point inward: `domain` knows nothing about audio devices, files, or any UI. Every external system sits behind a port, so a platform adapter can be swapped without touching the engine.

## Decision 1 — Engine and UI stack

**Decided (2026-10-30, desktop shell updated 2026-11-19): one Rust core, one shell per platform family.**

| Shell | Platforms | Language | Talks to the core through |
|-------|-----------|----------|---------------------------|
| `apps/apple` | iPhone, iPad (one SwiftUI app) | Swift | UniFFI-generated Swift bindings (`uniffi` 0.32, MPL-2.0) |
| `apps/android` | Android tablets and phones | Kotlin | UniFFI-generated Kotlin bindings |
| `apps/desktop` | Mac, Windows, Linux | Rust | Direct calls to the `app` crate, no FFI; UI drawn with GPUI (`gpui` 0.2, Apache-2.0) |

| Criterion | JUCE (C++) for everything | Rust core + one shared UI (Flutter) | **Rust core + per-family shells** |
|-----------|---------------------------|-------------------------------------|-----------------------------------|
| Audio I/O maturity | Best, built in | `cpal` behind the `AudioOutput` port | Same as Flutter; native adapters are natural per shell |
| License impact | AGPLv3 unless commercial | Permissive | Permissive |
| Apple App Store | Blocked under AGPL | Allowed | Allowed for iPhone and iPad; Mac App Store with GPUI unproven |
| Native feel and OS integration (audio session, USB permissions, Files) | Custom-drawn | Good | Best on mobile; custom-drawn on desktop |
| UI work | One codebase | One codebase | Three shells |
| Feature parity | Automatic | Automatic | Must be kept on purpose |
| Languages | C++ | Rust, Dart | Rust, Swift, Kotlin |

**Why native shells on mobile:** natural access to platform audio and device APIs (AVAudioSession on iPhone and iPad, USB permissions on Android), the best touch feel, and every store stays open.

**Why GPUI on desktop (Mac, Windows, Linux):**
- The shell is Rust, so it calls the `app` crate directly: no bindings, no IPC, one language from the audio callback to the screen.
- GPUI draws everything on the GPU (Metal on macOS, DirectX on Windows, Vulkan on Linux), which suits waveforms, meters and a playhead redrawn every frame.
- One desktop UI for three operating systems. The desktop use is the same everywhere (laptop, keyboard, mouse, MIDI); the iPad is touch-first and gets its own layout anyway.
- macOS is GPUI's most mature backend, and the development machine is a Mac, so the shipped desktop app is the one used every day.

**Known GPUI risks and how they are handled:**
- Pre-1.0 API that still changes, with sparse documentation: pin the exact `gpui` version and upgrade on purpose. `gpui-component` 0.10.0 builds on the `gpui-pre` fork, not on `gpui` 0.2.2, so the desktop widgets are our own.
- The macOS build uses gpui's `runtime_shaders` feature, because Xcode 26 ships the Metal toolchain separately; shaders then compile when the app starts.
- Screen-reader support upstream is not confirmed (forks add it with AccessKit): spike S5 checks it before release.
- Mac App Store distribution of a GPUI app is unproven: ship the Mac build signed and notarized outside the store first.
- No platform look: the app uses its own design (the design canvas) on every desktop.

**How the cost stays bounded:**
- All behavior lives in the core: validation, timeline compilation, transport, queued jumps, mixing. The `app` crate exposes one API of commands and state snapshots; shells only render snapshots and send commands.
- Shells share one design (the screens in the design canvas) and one set of acceptance tests written against the `app` API.
- Shells are built one at a time: a shell starts only when the previous one plays a setlist end to end.

**What would change this:** if GPUI's churn or missing accessibility blocks the desktop shell, Tauri with React (the 2026-10-30 choice) can replace it without touching the core, because both call the `app` crate from Rust. If keeping three shells in parity becomes the bottleneck, a shared UI layer can replace two of them, again without touching the core.

## Decision 2 — Engine model: one clock, compiled timeline

The UI never tells the engine "play section 3". Instead, the `domain` layer **compiles** a song, an arrangement, and its settings into a `RenderPlan`. The engine just plays that plan against a single sample counter.

```text
Song + Arrangement + Settings ──compile()──▶ RenderPlan
                                              ├─ segments: [out_start, src_start, length, fade_in, fade_out]
                                              └─ events:   click ticks, guide cues, count-ins (output sample time)
```

| Rule | Consequence |
|------|-------------|
| One sample counter drives every stem reader. | Zero drift between stems by construction. |
| `compile()` is a pure function. | It can be unit- and property-tested without audio. |
| A live change (jump, loop, fade) takes effect at a boundary sample: the live player walks the arrangement and applies the same seam rule as `compile` (`domain::seam_fade`), so with no live change it reproduces the compiled plan bit for bit. | Live edits are sample-exact and never touch audio mid-buffer. |
| A queued jump starts prefetching the target audio right away; if it is not ready at the boundary, the jump moves to the next boundary. | The engine never plays silence or glitches to honor a jump. |

## Decision 3 — Threads and real-time rules

| Thread | Does | Talks through |
|--------|------|---------------|
| Audio (OS callback) | Renders the current plan: mix, click, guide, routing | Reads per-stem ring buffers (`rtrb`); publishes state through `triple_buffer` |
| Disk | Decodes stems ahead of the playhead and prefetches jump targets | Writes the ring buffers |
| Control | Runs use cases, compiles plans, handles MIDI input | Sends commands to the audio thread over a lock-free queue |
| UI | Platform shell (SwiftUI, Compose or GPUI); draws state snapshots | Calls the `app` API; polls snapshots at about 30 Hz |

Real-time checklist for every line that runs in the audio callback:

- [ ] No memory allocation or deallocation.
- [ ] No locks, no blocking calls, no file or network I/O, no logging.
- [ ] All buffers are preallocated when the plan is loaded.
- [ ] Gain changes are smoothed per sample to avoid zipper noise.

## Engine components

| Component | Job | MVP |
|-----------|-----|-----|
| Transport | Owns the sample counter, applies queued commands at boundaries | Yes |
| StemReader | One per stem; reads its ring buffer at the plan's source offset | Yes |
| ClickGenerator | Synthesizes accent and subdivision ticks from plan events | Yes |
| GuideSampler | Plays cue samples at plan event times | Yes |
| BusMixer | Sums stems into buses, applies volume, mute, and solo | Yes |
| Router | Maps buses to physical output channels; split mode for 2 outputs | Yes |
| MeterTap | Publishes peak levels for the UI | Yes |
| MidiOutScheduler | Sends cues and MIDI Clock at plan event times | v0.3 |
| AutomationRunner | Applies timeline automation | v0.3 |
| PadPlayer | Loops ambient pads in the current key | v0.4 |
| LtcGenerator | Outputs SMPTE timecode as audio | v1.0 |

## Decision 4 — Key and tempo change (v0.2)

Render transposed and time-stretched stems **offline**, into a cache keyed by key and tempo, then play them like any other stems. Stems whose part is non-tonal (drums, percussion) get tempo change only. The library is Signalsmith Stretch (MIT) through the `signalsmith-stretch` crate. Rubber Band is excluded because its GPL license cannot ship on the App Store.

## Decision 5 — Dependency and project licenses

| Dependency | Version | License | Default build |
|------------|---------|---------|---------------|
| `cpal` (audio I/O) | 0.18.2 | Apache-2.0 | Yes |
| `midir` (MIDI) | 0.11.1 | MIT | Yes |
| `symphonia` (decoding) | 0.6.1 | MPL-2.0 | Yes |
| `signalsmith-stretch` | 0.1.3 | MIT | Yes (v0.2) |
| `uniffi` (Swift and Kotlin bindings) | 0.32.2 | MPL-2.0 | Yes |
| `gpui` (desktop shell) | 0.2.2 | Apache-2.0 | Yes |
| `rtrb` (ring buffer) | 0.4.1 | MIT or Apache-2.0 | Yes |
| `triple_buffer` | 9.0.1 | MPL-2.0 | Yes |
| `crossterm` (`play-cli` key input only) | 0.29.0 | MIT | Yes |
| `trash` (removed setlists go to the system trash) | 5.4.0 | MIT (permissive) | Yes |
| Fonts Barlow, Barlow Semi Condensed, JetBrains Mono (bundled in the desktop app, `apps/desktop/assets/fonts`) | — | SIL Open Font License 1.1 | Yes |
| ASIO SDK (Windows) | — | GPLv3 or proprietary | Opt-in feature only |
| Ableton Link (`rusty_link`) | 0.4.9 | GPL-2.0-or-later | Opt-in feature only, if ever |

**Project license — decided (2026-11-19): MPL-2.0** (see `LICENSE`): changes to our files must stay open, yet the app can ship on the App Store and link permissive libraries. A Windows build with the ASIO feature becomes a GPLv3 binary and is distributed separately.

## Decision 6 — Storage

Songs use the [open song package](mvp-spec.md#song-package-format-v1). Lossy imports are transcoded to FLAC so the disk thread can seek to any sample exactly; that costs storage and is listed as an open decision in the spec.

Mixing and routing add three JSON files, each with its own `formatVersion` (1); `domain` documents the formats and `library` reads and writes them:

| File | Where | Holds | Format docs |
|------|-------|-------|-------------|
| `mix.json` | Beside `song.json` in the song folder; `song.json` is never rewritten | Each arrangement's stem volumes, mutes, solos and bus choices | `domain::mix` |
| `bus-settings.json` | App configuration folder | The global bus volumes and mutes and the master volume | `domain::mixer` |
| `routing.json` | App configuration folder | Each device's bus-to-output routing and split mode | `domain::routing` |

The app configuration folder is `~/Library/Application Support/OpenVamp` on macOS, `%APPDATA%\OpenVamp` on Windows, and `$XDG_CONFIG_HOME/openvamp` (or `~/.config/openvamp`) on Linux; the phone and tablet shells pass their own (`library::ConfigStore`). A missing file means the defaults, so a song without `mix.json` plays as before. A file that cannot be used (unreadable, malformed, a symlink out of its folder, a FIFO, over 1 MiB, or a newer `formatVersion`) is reported, the defaults play, and the file is left alone until the user changes a setting; a newer `formatVersion` is never written over. Writes are atomic, like setlists.

## Repository layout

```text
crates/
  domain/       pure model and timeline compiler (no I/O, no audio)
  engine/       real-time renderer
  library/      song package read/write, import, transcode
  io-cpal/      AudioOutput adapter
  io-midir/     MidiInput adapter
  app/          use cases; the one API every shell calls
  ffi/          UniFFI bindings of `app` for Swift and Kotlin
  render-cli/   headless renderer for tests and CI
apps/
  apple/        SwiftUI app for iPhone and iPad
  android/      Jetpack Compose app
  desktop/      GPUI app for Mac, Windows and Linux
docs/
```

## Testing strategy

| Layer | How | Proves |
|-------|-----|--------|
| `domain` | Unit and property tests on `compile()` | Arrangements produce contiguous, non-overlapping segments; cues never overlap |
| `engine` | Render offline through `render-cli`, compare with golden WAV files | Output is sample-exact and bit-identical across runs |
| `engine` | Null test: engine output minus the plain sum of the stems | Mixing and seams add nothing |
| `engine` | Impulse test on the click | Every tick lands on its computed sample |
| Adapters | Hardware matrix per Tier 1 device | Multichannel output, latency, hot-plug |
| UI | Widget tests against a fake engine port | Screens behave without audio hardware |
| Performance | 60-minute soak test with an underrun counter | The non-functional targets in the spec |

## Risks and spikes

Run these before building the UI. Each one is a few days and decides whether Decision 1 holds.

| Spike | Question | Pass criterion |
|-------|----------|----------------|
| S1 | Can `cpal` open 8 outputs on an Android tablet with a class-compliant USB interface? | Distinct audio on all 8 outputs on two tablets from different makers |
| S2 | Same on iPadOS | Distinct audio on all 8 outputs |
| S3 | Can a low-end Android tablet decode 32 FLAC stems in real time? | Under 50% of one core at 48 kHz |
| S4 | Shell → Rust → audio round trip for transport commands (UniFFI on mobile, direct calls in the GPUI app) | Under 50 ms from tap to sound |
| S5 | Does the GPUI app expose the Perform screen to screen readers (VoiceOver, Narrator, Orca)? | Transport, section pads and panic are reachable and labelled on all three desktops |

Android USB multichannel varies by device maker, and the official Android docs still describe 1–2 channels for USB host mode. S1 is the highest risk in the project.

## Prior art: how Playback 8.6.4 is built

Static analysis of the shipped macOS binary, used only to understand the architecture (clean-room: no code or assets reused).

| Concern | Playback | This project |
|---------|----------|--------------|
| Engine | Custom AUv3 audio units chained inside the app | Rust components behind ports |
| Stem streaming | One player per stem, disk reader plus lock-free ring buffer, one player acting as master clock | One shared sample clock, one ring buffer per stem |
| Guide | Audio unit with a list of cue samples placed at section times | GuideSampler driven by compiled plan events |
| Pitch and tempo | zplane élastique (commercial), rendered on the device | Signalsmith Stretch (MIT), rendered offline |
| Auto-routing | Static tables: part → bus, and bus → output per output count (1–32) | Our own rules file and routing presets |
| Remote and sync | WebSocket server on the local network, Bonjour discovery | Planned for v0.4 with an open protocol |
| Data | Core Data: Song, Arrangement, Section, Track, Bus, MIDI event, Automation cue, Setlist | Open JSON song package plus a local library database |

## Next step

1. Live playback is built — the `app` API, the real-time engine with `cpal`, `play-cli` and the GPUI desktop shell — as recorded in [odd/tasks/live-playback.md](../odd/tasks/live-playback.md).
2. Next feature: setlists and transitions (F5).
3. Run spikes S1–S5 when the hardware is available (an Android tablet, an iPad and a multichannel USB interface).
