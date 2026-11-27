# MVP Spec — OpenVamp

An open-source, offline-first app that plays a band's backing stems live — with click, spoken guide cues, section navigation, and multi-output routing — on every major platform. It targets the same job as MultiTracks.com Playback, which ships only on Apple platforms, while using the user's own audio and an open song format.

> **Status:** Draft proposal. Items marked **Decision needed** are listed in [Open decisions](#open-decisions).

## MVP at a glance

| In the MVP | After the MVP | Out of scope |
|------------|---------------|--------------|
| Import your own stems into an open song package | Key and tempo change (offline render) | Selling or streaming a content catalog |
| Section markers, named arrangements | Ambient pads that follow the song key | Accounts and subscriptions |
| Generated click, count-in, dynamic guide cues | MIDI Out cues (lyrics, lights, patches) and MIDI Clock | Reading MultiTracks content or formats |
| Setlists with transitions | Automations on the timeline | |
| Live section jump, live loop, fade | Remote control over the local network | |
| Mixer: stems → buses → physical outputs | SMPTE timecode output, chart sync | |
| MIDI and keyboard control (learn mode) | Planning Center import, redundancy | |
| | Library sources in any cloud (cloud release) | |

## Core scenario

The MVP is done when a worship team can run this flow without touching anything else:

1. On Thursday, the music director imports four songs from DAW-exported stems, marks sections, and saves an arrangement per song.
2. They build a setlist, choose transitions, and set the output routing for the church's 8-output interface.
3. On Sunday, the keys player opens the setlist **offline**, presses a foot pedal to start, and the band hears click and guide cues in their in-ears while the front of house receives stems by bus.
4. The leader repeats a chorus: the keys player taps **Chorus**, and the jump happens on the section boundary with no glitch.
5. The last song ends with a fade; click and guide keep running until the band stops.

## Clean-room rules

This project reimplements **documented behavior**, never code or assets.

| Allowed | Not allowed |
|---------|-------------|
| Behavior described in public docs and observed in the UI | Copying code, decompiled logic, or binary resources |
| Generic domain knowledge (sections, buses, click) | Shipping their JSON tables, click sounds, guide-cue recordings, or pads |
| Our own data tables written from scratch | Reading, converting, or de-obfuscating their downloaded content |

All bundled sounds (click, guide voices) must be original or under a license compatible with the project license.

## Glossary

| Term | Meaning |
|------|---------|
| **Stem** | One audio file of a song (e.g. "Bass", "EG 1"). All stems of a song share start point and sample rate. |
| **Part** | The instrument role of a stem (drums, bass, vocals…). Drives auto-routing and transposition. |
| **Bus** | A group of stems mixed together (e.g. "Drums"). Buses are routed to physical outputs. |
| **Section** | A named time range of the song (Intro, Verse 1, Chorus…). Defined by a start marker. |
| **Arrangement** | An ordered list of sections to play; sections can repeat or be skipped. |
| **Song map** | The arrangement as shown on the waveform during playback. |
| **Setlist** | An ordered list of songs, each with its arrangement and a transition to the next. |
| **Click** | A metronome generated from the song's tempo and time-signature maps. |
| **Guide cue** | A spoken cue ("Chorus", "1, 2, 3, 4") placed automatically before each section. |
| **Count-in** | One bar of click and spoken count before a section starts. |

## Functional requirements

Each requirement has an ID for traceability in issues and tests.

### F1 — Song import and packages

| ID | Requirement |
|----|-------------|
| F1.1 | Import a folder or ZIP of stems in WAV, FLAC, MP3, or M4A (AAC). |
| F1.2 | Assign a part to each stem automatically from its file name, using our own rules table; the user can override it. |
| F1.3 | Enter BPM, time signature, and key; optionally add tempo and time-signature changes at any position. |
| F1.4 | Save the result as an [open song package](#song-package-format-v1). |
| F1.5 | Reject a package whose stems differ in sample rate, and warn when stem lengths differ by more than one second. |

- [ ] Importing 24 stems of 5 minutes produces a playable package on the reference hardware.
- [ ] A stem named `EG 1.wav` is assigned to the electric-guitar part and bus without user action.
- [ ] A package re-opened after an app restart plays identically (sample-exact render).

### F2 — Sections and arrangements

| ID | Requirement |
|----|-------------|
| F2.1 | Place, move, rename, and delete section markers on the waveform, snapped to the bar grid. |
| F2.2 | Choose a section type from a built-in list (Intro, Verse, Pre-Chorus, Chorus, Bridge, Tag, Vamp, Outro…) plus a number. |
| F2.3 | Create named arrangements by reordering, repeating, and removing sections. A **Default** arrangement always exists and cannot be deleted. |
| F2.4 | Mark a section as **infinite loop** by default in an arrangement. |

- [ ] An arrangement `Intro, Verse, Chorus, Chorus, Outro` renders the chorus twice with no gap or click at the seam.
- [ ] Moving a marker updates every arrangement that uses that section.

### F3 — Click and count-in

| ID | Requirement |
|----|-------------|
| F3.1 | Generate the click from the tempo and time-signature maps, with an accent on beat 1. |
| F3.2 | Mix four click levels independently: accent, quarter, eighth, and sixteenth notes. |
| F3.3 | Ship at least three original click sounds. |
| F3.4 | Enable a one-bar count-in per section (click plus spoken count). |
| F3.5 | Accept a click stem from the package instead of the generated click. |

- [ ] Every click lands within ±1 sample of its computed beat position, including across tempo changes.
- [ ] The click keeps running during a fade-out (see F6.4).

### F4 — Guide cues

| ID | Requirement |
|----|-------------|
| F4.1 | Generate guide cues from the arrangement: before each section, play the cue for its type and number. |
| F4.2 | Default timing is **one bar before the section starts**; the user can switch to **at section start**. |
| F4.3 | Cues follow the arrangement, so reordering sections updates the cues (dynamic guide). |
| F4.4 | Turn cues on or off per section. |
| F4.5 | Ship guide voices in English and Spanish at minimum. **Decision needed:** recorded voices or open TTS. |
| F4.6 | Accept a guide stem from the package; it plays linearly and does not follow rearrangements. |

- [ ] Reordering `Verse → Chorus` to `Chorus → Verse` makes the cues say "Chorus" then "Verse".
- [ ] A cue never overlaps the next cue; if a section is shorter than the cue, the cue is skipped.

### F5 — Setlists and transitions

| ID | Requirement |
|----|-------------|
| F5.1 | Create, rename, duplicate, and reorder setlists and the songs in them. |
| F5.2 | Pick an arrangement per song in the setlist. |
| F5.3 | Set start and end points per song, with an optional five-second fade-out at the end. |
| F5.4 | Choose a transition per song: **Cue next** (stop and select next), **Stay** (stop in song), **AutoLink** (start the next song immediately), **Crossfade** (blend over the last five seconds). |
| F5.5 | Insert a **click-only** tile at any tempo and time signature. |

- [ ] AutoLink starts the next song on the sample after the previous one ends.
- [ ] Crossfade keeps the combined output below 0 dBFS when both songs peak at −1 dBFS.

### F6 — Transport and live navigation

| ID | Requirement |
|----|-------------|
| F6.1 | Play, pause, stop, back to start, previous or next song. |
| F6.2 | **Jump to section:** queue any section; the jump happens at the end of the current section by default, or at the next bar if the user chooses. |
| F6.3 | **Live loop:** repeat the current section once more. **Infinite loop:** repeat until switched off. |
| F6.4 | **Fade:** fade out all stems over a configurable time while click and guide keep playing; press again to fade back in. |
| F6.5 | Show the queued action on the song map until it executes or is cancelled. |
| F6.6 | **Panic:** fade everything out in 0.5 seconds and stop. |

- [ ] A queued jump executes on the exact boundary sample.
- [ ] If the target audio is not buffered in time, the jump moves to the next boundary instead of producing silence or a glitch.

### F7 — Mixer and buses

| ID | Requirement |
|----|-------------|
| F7.1 | Each stem has volume, mute, and solo; each bus has volume and mute; there is one master volume. |
| F7.2 | Stems go to buses automatically by part (F1.2); the user can reassign a stem to another bus. |
| F7.3 | Solo never mutes click or guide. |
| F7.4 | Volume changes ramp smoothly, with no zipper noise. |
| F7.5 | The mix is saved per arrangement; bus settings are global unless the user enables per-song buses. |

- [ ] Muting a bus silences every stem in it and restores their levels on unmute.

### F8 — Output routing

| ID | Requirement |
|----|-------------|
| F8.1 | List the audio devices available and their output channel count. |
| F8.2 | Route each bus to a mono output or a stereo pair, up to 32 outputs. |
| F8.3 | Apply a default routing preset based on the device's output count; the user can edit it and the routing is saved per device. |
| F8.4 | **Split mode** for 2-output devices: click and guide on the left, the stems summed to mono on the right. |
| F8.5 | Choose the buffer size (64 to 1024 frames) and show the resulting latency. |

- [ ] Unplugging the device pauses playback with a clear message; plugging it back in restores the routing.

### F9 — MIDI and keyboard control

| ID | Requirement |
|----|-------------|
| F9.1 | Map Note, CC, and Program Change messages to actions with a **learn** mode. |
| F9.2 | Mappable actions: play/pause, stop, fade, panic, previous/next song, previous/next section, jump to section N, live loop, infinite loop, master volume, bus volume. |
| F9.3 | Mappings are global in the MVP. |
| F9.4 | Desktop keyboard shortcuts cover the same actions; they are user-editable. |

- [ ] A foot pedal sending CC 64 can be learned as play/pause in under three taps.

### F10 — Storage and offline operation

| ID | Requirement |
|----|-------------|
| F10.1 | All features work with no network connection and no account. |
| F10.2 | Songs and setlists live in a user-visible library folder. |
| F10.3 | Copying a song package folder to another device makes it playable there without conversion. |

## Song package format v1

A song is a folder with a manifest and its stems. The format is plain JSON so other tools can read and write it.

```text
Example Song.song/
├── song.json
├── stems/
│   ├── drums.flac
│   ├── bass.flac
│   └── eg1.flac
└── waveforms/          # cache, safe to delete
```

```json
{
  "formatVersion": 1,
  "title": "Example Song",
  "artist": "Example Artist",
  "sampleRate": 48000,
  "key": "G",
  "tempoMap": [{ "at": 0, "bpm": 72.0 }],
  "timeSignatureMap": [{ "at": 0, "numerator": 4, "denominator": 4 }],
  "stems": [
    { "id": "drums", "file": "stems/drums.flac", "part": "drums", "role": "track" },
    { "id": "click", "file": "stems/click.flac", "part": "click", "role": "click" }
  ],
  "sections": [
    { "id": "s1", "type": "intro", "number": null, "start": 0 },
    { "id": "s2", "type": "verse", "number": 1, "start": 640000 }
  ],
  "arrangements": [
    { "id": "default", "name": "Default", "sections": ["s1", "s2"], "infiniteLoop": [] }
  ]
}
```

| Rule | Why |
|------|-----|
| Positions (`at`, `start`) are **sample offsets** at `sampleRate`. | Exact and unambiguous; the editor shows bars and time. |
| A section ends where the next section starts; the last one ends at the song end. | No overlapping or gapped sections by construction. |
| `role` is `track`, `click`, or `guide`. | Click and guide stems replace the generated ones and bypass solo. |
| Lossy imports (MP3, M4A) are transcoded to FLAC; WAV and FLAC stems are copied as they are. | Sample-accurate seeking for section jumps. Decided 2026-11-26: the extra storage is accepted. |

## Non-functional requirements

Targets are validated on the reference hardware (**Decision needed:** choose it).

| Area | Target |
|------|--------|
| Sync | All stems stay sample-aligned for 60 minutes (zero drift). |
| Glitches | Zero buffer underruns in a 60-minute run with 32 stems at 256 frames, 48 kHz. |
| Capacity | At least 32 stems per song and 32 output channels. |
| Load time | A 5-minute, 24-stem song is ready to play within 2 seconds of selection. |
| Memory | Audio streams from disk; memory use does not grow with song length. |
| Responsiveness | The UI acknowledges a transport action within 50 ms. |
| Determinism | Rendering a song offline twice produces bit-identical output. |

## Platforms

| Tier | Platforms | Meaning |
|------|-----------|---------|
| Tier 1 (MVP release) | Windows, macOS, Android tablets | Fully tested; these fill the gap left by Apple-only tools. |
| Tier 2 (MVP beta) | Linux, iPadOS | Builds from the same code; tested less often. |
| Later | iPhone, Android phones | Small-screen layout. |

**Decision needed:** confirm the tiers. iPadOS distribution needs an Apple developer account.

Shells: the SwiftUI app covers iPadOS and iPhone; the Compose app covers Android; the GPUI app covers macOS, Windows and Linux.

## Roadmap after the MVP

| Version | Adds |
|---------|------|
| Cloud release (order to be decided) | Library sources in any cloud, so users are not tied to one content store: first the operating system's cloud integrations (Files app, Android Storage Access Framework, desktop sync folders), then direct connectors (WebDAV, S3-compatible, Google Drive, Dropbox, OneDrive). Setlists are downloaded and verified before playing; playback never streams from the network |
| v0.2 | Key and tempo change (offline render), setlist export and import, per-song MIDI mappings |
| v0.3 | MIDI Out cues (including a lyrics bus for presentation software) and MIDI Clock, automations |
| v0.4 | Ambient pads that follow the key, local-network remote control |
| v1.0 | SMPTE timecode output, chart sync protocol, Planning Center import, redundant playback |

## Open decisions

- [x] **Engine and UI stack** — decided: Rust core with a native UI per platform; see [architecture.md](architecture.md#decision-1--engine-and-ui-stack).
- [x] **Project license** — MPL-2.0, decided 2026-11-19 (see `LICENSE` and the architecture doc).
- [ ] **Guide voices** — record original voices or use an open TTS voice with a compatible license.
- [ ] **Reference hardware** — one low-end device per Tier 1 platform for the performance targets.
- [ ] **Platform tiers** — confirm the table above.
- [x] **Lossy import** — decided 2026-11-26: MP3 and M4A are transcoded to 24-bit FLAC (exact seeking, larger files); WAV and FLAC are copied.
- [x] **Project name** — OpenVamp, decided 2026-11-19 ("open" for open source, "vamp" for the section a band repeats); logo files in [brand/](brand/).

## Next step

Read the [architecture proposal](architecture.md), settle Decision 1, then start with the timeline compiler and the offline renderer: they hold most of the risk and need no audio hardware to test.
