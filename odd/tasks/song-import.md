# Feature: song import

Objective: import a folder or ZIP of stems (WAV, FLAC, MP3, M4A) into an open song package, with parts assigned from file names, the song's tempo, meter and key, lossy files transcoded to FLAC, and FLAC stems playable everywhere; desktop flow from the Library's "Import stems" to the Song editor.
Branch: `feat/song-import` (from `main` at bc999ea) · Delivery: work-unit commits, stacked PRs to `main` (the strategy of the setlists and mixer features) · Test runner: `cargo test --workspace` with Rust 1.93.0 (`rust-toolchain.toml`).

## Specs

Requirements quote `docs/mvp-spec.md` (F1) and the user verbatim. Items marked **Assumption** are not in those words and are the parent's call until the user says otherwise.

### S1 — Sources and formats (F1.1)
- F1.1: "Import a folder or ZIP of stems in WAV, FLAC, MP3, or M4A (AAC)."
- User (L1): "sí, arrancá con la importación a FLAC, tambien hay gente que usa otros formatos como waw".
- Architecture: "Lossy files become FLAC for exact seeking" (`docs/architecture.md`, Decision 6).
- **Assumption:** WAV and FLAC files the player can read are copied unchanged. MP3 and M4A are decoded and written as 24-bit FLAC. Encoder delay and padding are trimmed when the file records them (gapless), so every stem starts on the same sample.
- **Assumption:** a folder import takes every audio file in the folder and its subfolders. It skips hidden files and folders, and `__MACOSX` in a ZIP, and ignores files that are not audio. A source with no audio file is refused.
- **Assumption (rewritten in L5):** a WAV or FLAC file the player cannot read as-is (for example an 8-bit or 64-bit float WAV, or a FLAC that starts with an ID3 tag) is decoded and written as FLAC like a lossy file. A file with more than two channels, or one nothing can decode, is refused with its file name and the reason, and nothing is written. Every stem the import writes is opened with the player's own reader before the package is finished.
- **Assumption:** ZIP entries are never written under their own names. Each stem is written as `stems/<id>.wav` or `stems/<id>.flac`, with an id the import chooses. An entry named like `../x` therefore cannot write outside the song folder.
- Acceptance (F1): "Importing 24 stems of 5 minutes produces a playable package on the reference hardware." The reference hardware is still an open decision, so the time is measured on the development Mac and logged.

### S2 — Parts (F1.2)
- F1.2: "Assign a part to each stem automatically from its file name, using our own rules table; the user can override it."
- Acceptance (F1): "A stem named `EG 1.wav` is assigned to the electric-guitar part and bus without user action."
- **Assumption:** `domain` gets a fixed list of parts, and each part belongs to one of the six buses. A stem's part is stored by its kebab-case id (`electric-guitar`) in `song.json`'s existing `part` string, so the format does not change.
- **Assumption:** one word table decides both the part and the bus. These things stay the same:
  - `bus_for_name` and `bus_for_stem` give the same bus for every word and phrase of today's table and for every existing test;
  - existing packages, whose parts are free strings, keep their buses;
  - a name with no word of the table gets part `other`, on `DEFAULT_BUS`.
- **Assumption:** a stem whose part is the click gets role `click`, and one whose part is the guide gets role `guide`, so a recorded click or guide replaces the generated one.

### S3 — Song facts (F1.3)
- F1.3: "Enter BPM, time signature, and key; optionally add tempo and time-signature changes at any position."
- **Assumption:** the title defaults to the folder or ZIP name without its extension, the tempo to 120 BPM and the time signature to 4/4, and the key is empty. The key is optional text.
- **Assumption:** a change is entered at a bar number (bar 1 is the start) as the design's "Tempo or meter change". It is stored as a sample offset computed from the maps before it, and bars restart at a time-signature change as the domain already does.

### S4 — Checks (F1.5)
- F1.5: "Reject a package whose stems differ in sample rate, and warn when stem lengths differ by more than one second."
- These checks reuse `domain::validate` (`StemSampleRateMismatch`, `StemLengthsDiffer`). A mismatch blocks saving and names the files. The length warning shows, and saving stays allowed.

### S5 — Saving (F1.4)
- F1.4: "Save the result as an open song package."
- Acceptance (F1): "A package re-opened after an app restart plays identically (sample-exact render)."
- **Assumption:** the song folder goes in the library root as `<Title>.song`, and a taken name gets " 2", " 3" and so on. The folder is built in a hidden temporary folder inside the library and renamed into place only when complete. A failed or cancelled import leaves nothing behind. The Library's song list shows the new song at once.
- **Assumption:** the package gets one `Intro` section at the start and a `Default` arrangement holding it. A valid package needs at least one section and a default arrangement, and marking real sections is F2.

### S6 — FLAC playback
- FLAC stems play like WAV stems, in the full decode (`render-cli`, `load_song`) and in the disk streaming the live player uses, including a seek to any frame. This is needed by S1 for lossy files and FLAC inputs.
- Acceptance: a FLAC stem renders sample-identical to the same audio stored as WAV, and a seek into the middle of a FLAC stem plays the exact frames that follow.

### S7 — Desktop
- The design canvas (https://claude.ai/artifact/Ddx1h1CfCBDrxcTzqqUKyb, `Library.dc.html` and `SongEditor.dc.html`) defines the flow:
  - the Library's "Import stems" button ("Drop a folder or ZIP · WAV, FLAC, MP3, M4A");
  - then the Song editor for the new song, with Title, Key, Tempo, Time and "Tempo or meter change";
  - a stems panel with a header like "16 · 48 kHz · FLAC", and per stem the icon, the file name, the AUTO badge and the bus button;
  - "Discard" and "Save song".
- **Assumption:**
  - The button opens a picker for a folder or a ZIP; a drop on the button works too if GPUI 0.2.2 offers external drops (the writer checks and logs it).
  - The bus button opens a menu of parts grouped by bus, shows the bus as in the design, and loses AUTO once the user picks a part.
  - Section markers and the arrangement panel are F2 and stay out.
  - Decoding and transcoding run off the UI thread with progress. After "Save song" the screen returns to the Library with the new song listed.

### S8 — Docs
- `docs/mvp-spec.md`: the "Lossy import" open decision is decided (user, 2026-11-26, L1). `docs/architecture.md`: the license table lists `hound`, `symphonia`, the FLAC encoder and the ZIP crate, and Decision 6 says what was built. `README.md`: the status says what is built (the "Not built yet" line still names the mixer and routing, which are done). `apps/desktop/README.md` describes the import.

## Tasks

| ID | Specs | Route | Status | Commits |
|----|-------|-------|--------|---------|
| T1 | S2 | delegated (parallel unit with T2, disjoint surface `crates/domain`) | done (L4) | see L4 |
| T2 | S6 | delegated (parallel unit with T1, disjoint surface `crates/library` decoding, `Cargo.toml`, `Cargo.lock`) | done (L5); independent verifier pending | see L5 |
| T3 | S1, S3, S4, S5 (library side) | delegated (context backstop); needs T1 and T2 | pending | |
| T4 | S7 | delegated (context backstop); needs T3 | pending | |
| T5 | S8 | inline, with each task's own docs | pending | |

Risk: T2 and T3 add dependencies (item 5) and change `LoadError`, which `app` and the desktop consume (item 3), so each gets an independent verifier. T1 changes the shared bus rules (item 3).

## Log

- **L1** (2026-11-26, user, verbatim): "sí, arrancá con la importación a FLAC, tambien hay gente que usa otros formatos como waw". This answers the parent's proposal to start the MVP's F1 (song import), transcoding MP3 and M4A to FLAC. WAV is already in F1.1.
- **L2** (2026-11-26): map from a read-only explorer.
  - Playback decodes WAV only through `hound` (`crates/library/src/lib.rs:319` `open_stem`, `:557` `decode`, `stream/disk.rs`), and `LoadError::Decode` carries a `hound::Error`.
  - There is no `Part` type: `Stem.part` is a free `String` (`crates/domain/src/package.rs:85`). The file-name rules map to buses (`crates/domain/src/bus.rs` `RULES`, `bus_for_name`).
  - F1.5 is already in `domain::validate`. Nothing writes a whole package; `write_file_atomically` (`lib.rs:467`) is private. The Library page reads its catalog once (`library_page.rs:222`).
  - Crates chosen: `symphonia` 0.6.1 (MPL-2.0; WAV, FLAC, MP3, AAC/isomp4), `flacenc` 0.5.1 (Apache-2.0, pure Rust), and `zip` 9.5.1 (MIT). 9.5.1 is the newest `zip` accepting rust-version 1.88 and more than two weeks old.
  - Free disk space at the start: 9.7 GiB.
- **L3** (2026-11-26): T1 and T2 were launched together as parallel writers in the same tree. Their edit surfaces are disjoint: T1 has `crates/domain`; T2 has the library decoding and its tests, `Cargo.toml`, `Cargo.lock` and `crates/render-cli/README.md`. The parent owns git and commits each unit after checking its diff against its surface.
  - The design check found the Library's "Import stems" button ("Drop a folder or ZIP · WAV, FLAC, MP3, M4A") and the Song editor's stems panel, whose rows show a bus button and an AUTO badge.
- **L4** (2026-11-26): T1 delivered.
  - `crates/domain/src/part.rs` has 23 parts in `Part::ALL`, grouped by bus:
    - Click & Guide: click, guide
    - Drums: drums, percussion, loop
    - Bass: bass, synth-bass
    - Keys & Pads: keys, piano, organ, synth, pad, strings, brass, woodwinds, bells, fx, other
    - Guitars: electric-guitar, acoustic-guitar, guitar
    - Vocals: vocals, backing-vocals
  - It also has `id`, `from_id`, `name`, `bus`, `role`, `part_for_name` and `part_for_file`.
  - `RULES` and `PHRASES` in bus.rs now map to parts, and `bus_for_name` is the part's bus. There are 16 new two-word phrases, such as "acoustic guitar" and "guitarra electrica", and no new single words.
  - Checks:
    - RED: 10 parts tests failed against a stub that panicked.
    - GREEN: `cargo test -p domain` passes 152 tests; `cargo fmt -p domain -- --check` is clean. The parent re-ran both.
    - Differential: 36,894 names gave the same `bus_for_name` and `bus_for_stem` on the base and on this change. A mutant copy, used as a control, gave 225 differences.
  - Open point, decided by the parent for T3: a stem overridden to `other` stores `"other"`, which is no table word, so `bus_for_stem` falls back to the file name. `EG 1.wav` set to Other would therefore stay on Guitars. T3 makes `bus_for_stem` read `Part::from_id` first; only a stored `"other"` changes, because every other part id already reads back as its own part.
- **L5** (2026-11-26): T2 delivered.
  - **Dependencies:** `symphonia` 0.6.1 (`default-features = false`, `["flac"]`) and `flacenc` 0.5.1 (a dev-dependency for now). All new crates are MPL-2.0, Apache-2.0, MIT or MIT OR Apache-2.0.
  - **Reading:** `crates/library/src/decode.rs` adds `DecodeError` and `starts_as_flac`, which detects FLAC from its first bytes. `decode/flac.rs` adds `FlacStem`, which reuses the WAV `int_scale(bits)`, so a FLAC stem decodes bit-identical to hound's WAV.
  - **Errors:** `OpenedStem` is now a WAV/FLAC enum, `LoadError::Decode.source` is a `DecodeError`, and `SongProblem::UnsupportedFlacFormat` is new. Nothing outside the library matches these exhaustively.
  - **Streaming:** `disk.rs` streams both formats. A damaged FLAC frame is an error and never shifts the stem.
  - **Checks:**
    - RED: 8 tests failed before the change.
    - GREEN: `cargo test -p library` passes 133 tests (2 ignored); `cargo fmt -p library -- --check` is clean. The parent re-ran both.
    - The writer also ran `cargo test -p app`, `cargo test -p engine --test live_allocations` and `cargo check --workspace --all-targets`, all clean.
    - Differential against bc999ea through render-cli: identical output bytes, stderr and exit codes, for the demo song and for missing, garbage and truncated WAV stems.
  - **Seek timing:** 2–3 ms (debug build) for a seek in a 5-minute, 24-bit stereo FLAC with no SEEKTABLE, so the import needs no SEEKTABLE.
  - **Findings for T3:**
    1. flacenc 0.5.1 writes files symphonia rejects unless `stream.stream_info_mut().set_block_sizes(block, block)` is called before writing. The test helper `write_flac` does this.
    2. A FLAC that starts with an ID3 tag is read as WAV and fails.
    3. A STREAMINFO with no length is refused.
  - **S1 assumption rewritten:** a WAV or FLAC file the player cannot read as-is (for example an 8-bit or 64-bit float WAV, or a FLAC with an ID3 tag) is decoded and written as FLAC like a lossy file, instead of being refused. Only more than two channels is refused. Every stem the import writes is opened with the player's own reader before the package is finished.
  - Risk: item 3 (`LoadError::Decode`'s source type and a new public `SongProblem` variant) and item 5 (new dependencies), so T2 gets an independent verifier.
