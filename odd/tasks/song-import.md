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
| T1 | S2 | delegated (parallel unit with T2, disjoint surface `crates/domain`) | done (L4; verified L7; review approved L8) | 883b398 |
| T2 | S6 | delegated (parallel unit with T1, disjoint surface `crates/library` decoding, `Cargo.toml`, `Cargo.lock`) | done (L5; blockers fixed L12; verified L15; review approved L16) | 0c390d5, b906f5e |
| T3 | S1, S3, S4, S5 (library side) | delegated (context backstop); needs T1 and T2 | done (L9; blockers fixed L19; verified L21; reviews approved L22) | 6ea066c, 05b8af2, 4318887, b05d730 |
| T4 | S7 | delegated (context backstop); needs T3 | done (L13; app run L17; blocker fixed L23; verified and reviews approved L24) | dca6066, f8c6194 |
| T5 | S8 | inline, with each task's own docs | done (L23) | see L23 |

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
- **L6** (2026-11-26): native review, with the slices in detached worktrees under `multitrack-app-worktrees/`.
  - **T1** (`si-t1`, bc999ea..883b398): high risk, `review_due`; lenses reliability and readability.
  - **T2** (`si-t2`, 883b398..0c390d5): high risk, `review_due`; lenses risk, resilience and reliability.
  - The user granted both consents ("Revisar este cambio" for each).
  - Each review starts only after its independent verifier, because the tier's verifier runs first. T1 names item 3, so it gets a verifier too; it was launched with this entry. The T2 verifier was launched at L5.
  - T3 was launched in the main tree at the same time, on top of 0c390d5. The writer was told that fixes to T2's code may be forwarded.
- **L7** (2026-11-26): the T1 verifier passed every probe, with no blockers and no advisories.
  - **Corpus:** its own corpus of 35,806 names gave identical `bus_for_name` and `bus_for_stem` at bc999ea and at 883b398.
  - **Acceptance:** the `EG 1.wav` acceptance passes for all its spellings.
  - **Tests:** `cargo test -p domain` passes 152; `bus_rules.rs` only gained lines. `cargo test -p app --test mixer` passes 9 and `cargo test -p openvamp --test mixer_screen` passes 25.
  - **Scratch:** the parent removed the T1 writer's and verifier's scratch folders, and the T2 writer's.
  - **Review:** T1's native review started from the user's grant.
- **L8** (2026-11-26): the T1 review (lineage review-fd7223ce85e0071f) approved and was acknowledged (`gentle-ai.review-acknowledged/v1`), so the reviewed boundary is now 883b398. It left non-blocking findings:
  - (WARNING) the `PHRASES` doc comment in bus.rs states a rule that "bass guitar", "bass synth" and "voz guia" break;
  - (SUGGESTION) `Part::ALL` is hand-maintained, with no check that it lists every variant;
  - (SUGGESTION) a parts test compares `bus_for_name` with the expression it is defined as;
  - (SUGGESTION) the "buses unchanged" invariant lives only in the log, not in a committed test.

  The warning and the two test suggestions were sent to the T3 writer, because they fall within its surface. The `ALL` check stays a follow-up.
- **L9** (2026-11-26): T3 delivered.
  - **Domain:** `domain::import` adds `SongDraft`, `MapChange`, `DraftStem`, `to_package` → `PackagedDraft` with warnings, `DraftError`, and `song_folder_name`. `bus_for_stem` now reads `Part::from_id` first, as decided in L4.
  - **Library:** `library::import` (with `source`, `audio`, `mp4` and `save`) adds:
    - `probe_import(&Path) -> ImportDraft`, which exposes `stems()`, `refused()` and `song`;
    - `Library::save_import(&draft, &mut progress, &AtomicBool)`, which writes stems on scoped threads and reports `ImportProgress`;
    - `ImportError` (`Draft`, `Refused`, `Cancelled`, `Stem`, `Write`, `Source`, `Zip`, `NoAudio`, `TooManyFiles`, `StemsChanged`).
  - **Dependencies:** symphonia features `aac, isomp4, mp3, pcm, wav, id3v2`; flacenc is now a normal dependency; `zip` 9.5.1 with zlib-rs; `constant_time_eq` pinned to 0.6.1, because 0.6.2 is one day old. The 9 new crates are MIT, MIT OR Apache-2.0, CC0-1.0 OR MIT-0 OR Apache-2.0, and MPL-2.0. `Cargo.lock` only adds packages.
  - **Gapless:** MP3 lengths come from the LAME/Xing header, because symphonia 0.6.1 keeps full packet durations. M4A edit lists are read by `import/mp4.rs`, because symphonia does not apply them. Both fixtures import with their exact source length.
  - **T1 review follow-ups:** the `PHRASES` comment is reworded, a test pins every bc999ea word and phrase to its base bus, and the parts test asserts literal buses.
  - **Writer's assumptions, kept:**
    - a refused file blocks the whole save;
    - a folder's title is its whole name, and only ".zip" is removed;
    - a change at bar 1 is an error;
    - the song's rate is the one most stems share;
    - imports take at most 256 files, ZIP entries over 4 GiB are refused, and empty audio files are refused.
  - **Checks:**
    - RED: 14 domain and 18 library tests failed against stubs.
    - GREEN: `cargo test -p library -p domain` passes 323 tests (3 ignored); `cargo test -p app` passes 139; workspace check, clippy and `cargo doc -D warnings` are clean.
    - Differential against 0c390d5 through render-cli on six songs: identical.
    - Release timing: 24 five-minute stems (12 MP3, 12 WAV) save in 6.2–9.8 s in parallel, 26 s with one worker, and produce 1.4 GB.
  - **Parent fix:** `import/save.rs` had copied the Windows reserved-name list from `setlists.rs`. It now calls `setlists::is_windows_reserved` (now `pub(crate)`). The parent re-ran fmt, `cargo test -p library -p domain` (323 pass) and clippy, all clean.
  - **Disk:** the parent removed T3's scratch, `target/release` and `target/debug/incremental` (free 6.4 GiB).
  - Risk: item 5 (dependencies) and item 2 (ZIP entry names as untrusted paths), so T3 gets an independent verifier.
- **L10** (2026-11-26): T3 is now two commits, so native review can take each part alone: 7a4c0a2 (domain: song drafts and `bus_for_stem`, about 1,400 lines) and c1f710f (library import, dependencies and this document, about 3,000 lines). T4 launched on c1f710f. The T3 verifier waits for the T2 verifier, so that their scratch builds do not run out of disk.
- **L11** (2026-11-26): the T2 verifier found three blockers in 0c390d5.
  - **What passes:**
    - Full decodes of 648 FLAC stems (16/24-bit, 44.1/48/96 kHz, mono/stereo, odd lengths, flacenc and flac 1.5.0 in 9 encodings) match their WAV twins.
    - WAV behaviour is unchanged: render-cli gives 63/63 identical runs over 21 songs, and messages and listings are identical.
    - Real-time allocation tests and the 13 new crates' licenses pass. flacenc is dev-only, and symphonia has FLAC only.
    - Release seek plus first frame takes 0.1–0.5 ms.
  - **Blockers:**
    - **B1:** a short or mostly silent FLAC streamed from its start loses frames 4096–6143. symphonia 0.6.1 skips its parser reset after the open-time last-frame read; a fresh `FlacReader` per seek fixes it.
    - **B2:** 32-bit stereo FLAC decodes wrong in release and panics in debug (a symphonia limit).
    - **B3:** a truncated `flac` CLI file with a SEEKTABLE panics in `open_streaming` in debug.
  - **Advisories:**
    - a forged SEEKTABLE panics on the disk thread in debug;
    - 32-bit mono `-b 256` overflows in debug;
    - bad metadata blocks are reported as "ends before its first audio frame";
    - the ID3 prefix and zero-length cases from L5 remain;
    - the writer's tests used only noise;
    - variable-blocksize streams are untested.
  - **Correction batch:** the T2 writer was resumed in the main tree, at the same time as T4 (disjoint: library vs apps/desktop). It fixes B1–B3, and the import refuses FLAC over 24 bits. The parent will commit the playback fix and the import refusal separately, so the playback fix can be moved next to 0c390d5 before T2's native review. The review waits for the corrected code, and the verifier then rechecks only B1–B3.
- **L12** (2026-11-26): the correction batch landed in two commits.
  - **904002a (a):**
    - **B1:** each seek opens a fresh `FlacReader`, and frame 0 needs no seek.
    - **B2:** FLAC over 24 bits is refused as `UnsupportedFlacFormat` ("supported FLAC stems are 4- to 24-bit, mono or stereo"), and the render-cli README now says 4- to 24-bit.
    - **B3:** the new `decode/flac/frames.rs` hands symphonia only STREAMINFO plus the audio frames, after checking that the metadata blocks fit the file. This also fixes advisory 1, skips damaged tag and picture blocks, and gives precise metadata messages.
    - **Tests:** the new tests/flac_stream.rs, tone and silence signals, a SEEKTABLE written by hand, and fixtures `tone-32bit-stereo.flac` (28.5 KB) and `tone-32bit-mono-b256.flac` (18.8 KB).
  - **dc218ab (b):** the import refuses FLAC over 24 bits by codec, before decoding.
  - **Checks:**
    - RED: the gap, 32-bit and cut-SEEKTABLE tests failed or panicked first.
    - GREEN: the writer's full verification list passes in the main tree, and the verifier's own probes pass against the fix (283 hostile cases, 0 bad).
    - The parent re-ran `--test flac_stream` (3 passed) and `--test hostile_header` (6 passed).
- **L13** (2026-11-26): T4 delivered.
  - **New files:** `song_import.rs` (the Song editor view model, the background probe and save jobs on `cx.background_executor()` as PR #21 does, process-unique tickets, cancel on Discard, the header line) and `song_editor_ui.rs` (header, notes, a "Tempo and meter" panel, and the Stems panel with the part menu and "Show N more").
  - **Changed files:** `library_page.rs`, `library_ui.rs` (the "Import stems" drop zone), `screen.rs` (picker for a folder or file, drop, save), `tick.rs`, `action.rs`, `theme.rs`, icons `import.svg` and `left.svg`, and the "WAV or FLAC stems" wording in `ui.rs` and `main.rs`. The desktop README has an import section.
  - **Drop:** GPUI 0.2.2 supports dropping through `ExternalPaths`.
  - **Design differences:**
    - the part menu opens inline, as the Mixer menus do;
    - a left "Tempo and meter" panel, 330 px wide;
    - "From <source>", the notes box, "Saving… N%" and a marked new song row are additions;
    - "WAV + FLAC" for mixed formats.
  - **Writer's assumptions, kept:**
    - the "Library" back link discards the draft;
    - a new change goes 8 bars after the last;
    - Discard after a finished save keeps the song with a warning;
    - opening another library cancels a running save.
  - **Checks:**
    - RED: 27 import tests failed against stubs, plus the picker test.
    - GREEN: `cargo test -p openvamp` passes 281 tests, with clippy, fmt and the workspace check clean. The parent re-ran `--test song_import` (29 passed).
  - Risk: item 4 (background jobs, the cancel flag and result ordering), so T4 gets an independent verifier.
- **L14** (2026-11-26): the parent reordered the branch, which had not been pushed, so the playback fix sits next to T2 and each review slice is contiguous. Commits:
  - 883b398 T1
  - 0c390d5 T2
  - b906f5e T2 fix (a)
  - 6ea066c T3 domain
  - 05b8af2 T3 library
  - 4318887 import refusal (b)
  - 6aa0db8 T4

  The final tree is identical to the old head 290f8b8, which is kept as the branch `song-import-backup`. The commit ids in L10–L13 refer to the old order (0c390d5 is unchanged): 7a4c0a2 → 6ea066c, c1f710f → 05b8af2, 904002a → b906f5e, dc218ab → 4318887, 290f8b8 → 6aa0db8.

  Review slices:
  - T2: 883b398..b906f5e
  - T3 domain: b906f5e..6ea066c
  - T3 library: 6ea066c..4318887
  - T4: 4318887..6aa0db8

  The T2 verifier is rechecking B1–B3 on b906f5e.
- **L15** (2026-11-26): the machine rebooted, which cleared every temp scratch folder. Nothing in the repository was lost.
  - **Recheck:** the T2 verifier redid its limited recheck on b906f5e, using its own new probe and the `flac` 1.5.0 CLI rather than the committed fixtures. All three blockers are fixed, in debug and release:
    - **B1:** 8 compressible stems, 2,192 reads, 0 bad.
    - **B2:** 32-bit stereo and mono `-b 256` files give one clean `UnsupportedFlacFormat` in `load_song` and `open_streaming`, and the 24-bit controls stay exact.
    - **B3:** 58 cut or forged SEEKTABLE cases, 0 bad; cuts give clean errors and uncut files play exactly.
  - **Suites in the copy:** `cargo test -p library` passes 138 tests (2 ignored), and `live_allocations` passes 6.
  - **T2 review:** slice 883b398..b906f5e, worktree `si-t2`, lenses risk, resilience and reliability. It is high risk, 17 files and 1,988 lines. The user granted it ("Revisar este cambio").
  - **T3:** its independent verifier launched on 4318887 against b906f5e.
- **L16** (2026-11-26): the T2 review (lineage review-427e0445a3f6049e, slice 883b398..b906f5e) approved and was acknowledged, so the reviewed boundary is now b906f5e. It left two non-blocking reliability suggestions, kept as follow-ups:
  - the depth check has no lower bound, though the message says 4- to 24-bit;
  - bit-identical WAV/FLAC equality is only tested at 16 and 24 bits, and an 8-, 12- or 20-bit case would pin the left-justify shift.
- **L17** (2026-11-26): the parent ran the dev app (`target/OpenVamp-dev.app`, the binary at 6aa0db8).
  - **The run:** it opened a library through a test setlist and imported `/Users/cdec/projects/openvamp-demo/Way Maker Stems` with "Import stems" and the macOS picker. The folder holds 11 WAV, 1 MP3 and 1 M4A.
    - The Song editor showed the 13 stems with AUTO and the expected buses (AG → Acoustic guitar on Guitars, EG 1 → Guitars, Kick → Drums, Click and Guide → Click & Guide) and the header "13 · 48 kHz · WAV + FLAC".
    - The part menu lists the parts grouped by bus.
    - "Save song" showed its progress and returned to the Library, which lists "Way Maker Stems · 120 · 0:20 · 13 stems". The WAVs were copied, and the MP3 and M4A were written as FLAC.
    - The progress label may have gone from 96% to 90%; the T4 verifier checks this.
  - **Design canvas:** not compared side by side, because the browser pane is not signed in to claude.ai. The screenshots were sent to the user.
- **L18** (2026-11-26): the T3 verifier found four blockers.
  - **B1:** loud high-frequency lossy stems produce huge FLAC frames (flacenc never falls back to verbatim; one frame was 33.5 MB, and STREAMINFO's max frame size wraps), so the song saves and does not load, or the save fails with a message naming the source folder.
  - **B2:** Apple AAC M4A (afconvert, iTunes, Logic) keeps 2112 frames of delay, because `iTunSMPB` is not read (there is no `elst`).
  - **B3:** a Xing-less 192 kbps 44.1 kHz CBR MP3 passes the probe and is refused at save, one packet short.
  - **B4:** a truncated WAV or FLAC source is saved as a song that does not play, because the pre-rename check opens headers only.
  - **What passes:**
    - folder and ZIP sources, including `../evil.wav`, absolute paths, encrypted entries, zip bombs and symlinks; nothing was written outside the song folder;
    - byte-identical copies and exact lossless transcodes;
    - LAME MP3 and ffmpeg AAC gapless (+0 frames);
    - the draft edits, the bar-to-sample conversion (four hand calculations), folder naming, cancel and mid-save failures (nothing left behind);
    - render identity with packages written by hand;
    - `bus_for_stem`, which only changed for a stored `other`;
    - render-cli unchanged on 22 runs; `cargo test -p library -p domain` 329 passed;
    - the dependencies, all within the license policy and more than two weeks old.
  - **Advisories:**
    - A1: an honest-size ZIP entry is extracted in full before it is refused.
    - A2: with duplicate ZIP entry names, the last one wins.
    - A3: folders more than 32 levels deep are skipped.
    - A4: `kill -9` leaves a hidden `.import-*.tmp` folder behind.
    - A5: some messages are misleading (fixed with B1).
    - A6: HE-AAC decodes at its core rate.
    - A7: an M4A named `.mp3` skips the edit list.
    - A8: zlib-rs becomes flate2's backend for the other crates too.
  - **Next:**
    - the T3 writer was resumed for B1–B4, as one fix commit to be moved after 05b8af2;
    - the T4 verifier was launched at the same time in a scratch copy.
- **L19** (2026-11-26): the T3 correction batch landed as b05d730. The parent moved it right after 4318887, so the T3 library slice is 6ea066c..b05d730. T4 is now dca6066 (was 6aa0db8), and the doc commits follow it. The final tree is identical to the previous head, which is kept as `song-import-backup`. The fixes:
  - **B1:** a frame larger than its raw 24-bit samples is re-encoded verbatim, and STREAMINFO's frame sizes are the written ones. A save failure names the stem and keeps the `LoadError` detail.
  - **B2:** the `iTunSMPB` fallback in `import/mp4.rs`, with the fixture `tests/import/apple.m4a` (4.5 KB).
  - **B3:** `ProbedStem.estimated_length`; the save trusts the decoder's frame count for those stems. Fixture `tests/import/noxing.mp3` (32 KB).
  - **B4:** the probe refuses sources cut short, and `read_through` reads every written stem before the rename.
  - **Checks:**
    - RED/GREEN for each blocker;
    - `cargo test -p library -p domain` passes 335 tests (3 ignored); the workspace check and clippy are clean;
    - the parent re-ran `--test import` (28 passed, 1 ignored);
    - the release timing was not re-measured because of low disk.

  The T3 verifier is rechecking B1–B4 on b05d730.
- **L20** (2026-11-26): the T4 verifier (on 6aa0db8, now dca6066) found one blocker.
  - **B1:** quitting during "Saving…" (Cmd-Q or closing the last window) leaves the partial song in a hidden `.import-<pid>-<n>.tmp` folder forever, 149 MB in its probe. GPUI gives quit handlers 100 ms and `on_app_quit` only closes the song.
  - **What passes:**
    - the S7 flow (probe errors, stems with AUTO and bus, headers, part override, fields, tempo and meter changes, rate mismatch, length warning, refused files, save and mark);
    - concurrency: stale probe and save results are ignored, 400 of 400 across pages, from one process-wide ticket counter;
    - Discard mid-save leaves nothing behind;
    - progress never goes backwards within a save; the 96% then 90% the parent saw were two different saves;
    - keys typed in the editor do not reach the shortcuts;
    - wording;
    - `cargo test -p openvamp` 281 passed; the tests only gained lines.
  - **Advisories:**
    - A1: the macOS picker is modeless, so a late pick replaces an open draft silently;
    - A2: the "Cannot import stems while a song is being saved." notice is hidden behind the editor;
    - the button stays at 100% during the final read-through.
  - **Correction:** the T4 writer was resumed to fix B1. It will ask before quitting while a save runs and defer the quit until the save has stopped and cleaned up. A library backstop will remove certainly stale `.import-*.tmp` folders when an import starts, which also covers the T3 advisory A4 (`kill -9`).
- **L21** (2026-11-26): the T3 verifier's limited recheck on b05d730 confirmed B1–B4 fixed.
  - **B1:** the crash MP3 gives 2.0 MB and white noise 2.88 MB, both near raw size, with max frame 24,586 B and `flac -t` ok. Quiet sources are unchanged and exact copies are still byte-identical.
  - **B2:** afconvert AAC at 44.1 and 48 kHz, mono and stereo, gives +0 frames with the impulse at offset 0.
  - **B3:** Xing-less 192 kbps 44.1 kHz MP3s save at the decoder's length and render.
  - **B4:** sources cut before the probe are refused with the reason; sources cut between probe and save are refused naming the stem, and the library stays empty.
  - **Suite:** `cargo test -p library -p domain` 335 passed.
  - **New advisory:** for a Xing-less MP3 the draft keeps the probe's estimate, one packet (1,152 frames) longer than the saved length, so the length warning, the past-the-end bar check and progress totals can be off by one packet.
  - **Reviews:** the user granted both T3 slices ("Revisar este cambio" for each):
    - domain, `si-t3d`, b906f5e..6ea066c, 1,433 lines, lenses risk, reliability and readability;
    - library, `si-t3l`, 6ea066c..b05d730, 3,701 lines, lenses risk, resilience and reliability.
- **L22** (2026-11-26): both T3 reviews approved and were acknowledged. The reviewed boundary is now b05d730.
  - **Domain review** (lineage review-1e6cb1007531f263):
    - WARNING: `bus_for_stem` matches part ids case-sensitively, and the docs do not say so.
    - WARNING: a legacy package whose part is exactly `other` now goes to `DEFAULT_BUS`. This is intended (L4), but no migration note exists.
    - WARNING: the combined value-and-stem error path of `to_package` is untested.
    - SUGGESTION: the song-length limit is duplicated in `SongDraft::maps`.
  - **Library review** (lineage review-3a694fbba2888f49):
    - WARNING: mp4.rs box size overflow (a debug panic or a release wrap on a crafted M4A). A scan error refuses an otherwise decodable M4A.
    - WARNING: a ZIP entry copied as is is not `sync_all`ed before its rename.
    - WARNING: a long ZIP entry extraction ignores cancel and sends no progress.
    - SUGGESTIONS: the probe extracts every ZIP entry; one `report` result is discarded; no test saves a transcoded stem from a ZIP.
  - **Routing:** the box overflow, the ZIP sync and the discarded `report` were sent to the T4 writer, to land in the T4 slice. The rest are follow-ups, as is the user's preference ([[fewer-review-rounds]]).
- **L23** (2026-11-26): the T4 correction landed as f8c6194.
  - **Quit while saving:**
    - Quitting while a song saves asks "A song is being saved" (Quit / Keep saving). Quit cancels the save and quits only after the job reports back, with a 10 s fallback.
    - Quits that cannot ask (Dock, logout) block in GPUI's synchronous quit handler until the job stops, at most 5 s.
    - Dropping a running save waits too.
  - **Sweep:** `save_import` removes `.import-<pid>-<n>.tmp` folders from another pid that have been unchanged for over an hour, and nothing else.
  - **T3 review fixes folded in:** the mp4 box size check (an unreadable box keeps the M4A untrimmed and estimated), `sync_all` for ZIP entries copied as they are, and `Cancelled` after a copied ZIP entry.
  - **Checks:**
    - `cargo test -p openvamp` 285 passed; `--test import` 31 passed; fmt, clippy and the workspace check are clean;
    - the parent re-ran `--test song_import` (31) and `--test ending` (12).
  - **Risk:** item 4 (the cancel and wait across threads); the sweep deletes leftover data under a strict rule (item 1).
  - **The T4 verifier rechecks B1 on f8c6194.**
  - **T5 docs:**
    - `docs/mvp-spec.md`: the lossy-import decision is decided, and the storage note is updated.
    - `docs/architecture.md`:
      - Decision 6 is now Decided and says what was built;
      - the license table adds `hound`, `flacenc` and `zip`;
      - the `symphonia` row lists FLAC, MP3, AAC and M4A.
    - `README.md`: the status adds the song import and the Mixer and Audio output screens, and "Not built yet" no longer lists the mixer and routing, which were merged in PRs #15–#21.
    - The desktop README's import section came with T4.
- **L24** (2026-11-26): T4 closed.
  - **Recheck:** the T4 verifier confirmed B1 fixed on f8c6194. All four quit paths left only the old song, with 0 bytes of stems. Its independent sweep check removed only stale folders of the exact pattern from another pid and kept every other case. `--test import` passed 31. Low disk kept it from building the desktop suite.
  - **Reviews:** the T4 slice of 4,090 lines went over the review's context budget, so it was reviewed in two slices, each granted by the user:
    - b05d730..dca6066 (lineage review-578e9015a5a9bd88)
    - dca6066..f8c6194 (lineage review-4188f24682624610)

    Both approved and were acknowledged, so the reviewed boundary is now f8c6194.
  - **Seam check:** the parent ran `cargo test --workspace`: 934 passed, 0 failed, 8 ignored.
  - **Follow-ups** (non-blocking review warnings and verifier advisories, not fixed here, per [[fewer-review-rounds]]):
    - **Song editor:**
      - Discard during a save has no escape if the save never returns, for example on a hung drive;
      - `Saving::drop` blocks the UI for up to 5 s when another library opens mid-save;
      - the deferred-quit glue in screen.rs is untested;
      - the probe-during-save branch is untested;
      - the saving question promises more than a cancel at about 100% guarantees;
      - the `ChangeEdit` doc is stale, and `ClosePartMenu` is unused by the UI;
      - on Windows the import picker may not pick files and folders at once;
      - a late macOS picker result replaces an open draft;
      - the save notice can be hidden behind the editor.
    - **Library:**
      - the sweep trusts mtimes, which can be wrong on a network share with clock skew;
      - `mp4::edit_list` swallows every I/O error, not just a cut-short box;
      - cancel is not checked during a long ZIP entry extraction;
      - the probe extracts every ZIP entry;
      - a Xing-less MP3's draft length is one packet long;
      - with duplicate ZIP names, the last entry wins;
      - folders more than 32 levels deep are skipped;
      - HE-AAC decodes at its core rate;
      - an M4A named `.mp3` is not trimmed;
      - zlib-rs is now flate2's backend for the other crates too.
    - **Domain and FLAC:**
      - part-id case sensitivity is not documented;
      - no migration note for a legacy part `other`;
      - the combined `to_package` error path is untested;
      - the song-length limit is duplicated;
      - `Part::ALL` coverage is unchecked;
      - the FLAC depth check has no lower bound;
      - WAV/FLAC equality is tested only at 16 and 24 bits.
  - **Delivery:** the branch is ready for the stacked PRs. Pushing and opening the PRs are the user's call.

