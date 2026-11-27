# Feature: sections, arrangements and waveforms

Objective: edit a song's section markers and arrangements in the desktop Song editor, on a waveform of the song, and show the same waveform on the Perform song map (MVP F2).
Branch: `feat/sections-arrangements` (from `main` at d9e25b1) · Delivery: work-unit commits, stacked PRs to `main` (the strategy of the earlier features) · Test runner: `cargo test --workspace` with Rust 1.93.0 (`rust-toolchain.toml`).

## Specs

Requirements quote `docs/mvp-spec.md` (F2) and the user verbatim. Items marked **Assumption** are not in those words and are the parent's call until the user says otherwise. The design canvas is https://claude.ai/artifact/Ddx1h1CfCBDrxcTzqqUKyb; a local copy of its screens is in `/Users/cdec/projects/openvamp-demo/design/` (`SongEditor.dc.html`, `Main.dc.html`, `Library.dc.html`).

### S1 — Section markers (F2.1, F2.2)
- F2.1: "Place, move, rename, and delete section markers on the waveform, snapped to the bar grid."
- F2.2: "Choose a section type from a built-in list (Intro, Verse, Pre-Chorus, Chorus, Bridge, Tag, Vamp, Outro…) plus a number."
- Acceptance (F2): "Moving a marker updates every arrangement that uses that section."
- **Assumption:** the built-in list is today's `SectionType` (15 types) in its declared order, and the number is optional.
- **Assumption:** "Snap to bar", as in the design, snaps a marker to the nearest bar line. With it off, the marker snaps to the nearest beat.
- **Assumption:** the first marker always starts at the song's start, and it cannot be moved or deleted. Two markers never share a start.
- **Assumption:** adding a marker inside a section splits it. In every arrangement, the new section is inserted right after each occurrence of the section it splits, so every arrangement still plays the same audio.
- **Assumption:** deleting a marker removes its section from every arrangement. The delete is refused, with a message naming the arrangement, if it would leave an arrangement empty.
- **Assumption:** there is no audio preview in the editor. The "playhead" of "Add marker at playhead" is the editor's cursor, which a click on the waveform or the ruler places.

### S2 — Arrangements (F2.3, F2.4)
- F2.3: "Create named arrangements by reordering, repeating, and removing sections. A **Default** arrangement always exists and cannot be deleted."
- F2.4: "Mark a section as **infinite loop** by default in an arrangement."
- Acceptance (F2): "An arrangement `Intro, Verse, Chorus, Chorus, Outro` renders the chorus twice with no gap or click at the seam." The engine already renders arrangements; this is pinned with a test.
- **Assumption:** the design's arrangement panel defines the controls:
  - tabs per arrangement and "New";
  - chips that can be reordered by dragging and removed with ×;
  - "Add a section to the end";
  - the summary "N sections · N bars · m:ss".

  An arrangement can be renamed and deleted, except Default.
- **Assumption:** the design puts "Infinite loop by default" in the marker inspector, but the package stores it per arrangement. The switch applies to the selected section in the arrangement shown.
- **Assumption:** the inspector's "Guide cue" and "Count-in" switches have no field in the song package. They stay out (F4).

### S3 — Waveforms
- User (L1): "hacerlo ya con la forma de onda".
- Package format: the song folder has "`waveforms/` # cache, safe to delete" (`docs/mvp-spec.md`, Song package format v1).
- **Assumption:** the overview is the peak level of the song's `track` stems mixed at unity, at a fixed resolution (for example 100 values per second). It is cached in the song's `waveforms/` folder and computed in the background when missing or stale. Stale means a stem file changed, judged by its name, size and modified time. A deleted cache is rebuilt, and a cache that cannot be written (read-only library) still gives the overview, just not cached.
- **Assumption:** the design draws one bar per musical bar, its height the bar's level (16–94 % in the editor, 18–96 % on Perform). The per-bar levels are computed from the cached overview with the song's current bar grid, so tempo and meter edits redraw at once.

### S4 — Song editor for existing songs
- **Assumption:** the design's Library song rows have an edit link ("Edit <song>") that opens the Song editor for that song. The editor shows:
  - the same header as for an import: Title, Key, Tempo, Time, "Tempo or meter change", Discard and "Save song";
  - the "Section markers" panel: "Add marker at playhead", "Snap to bar", zoom out and in, a bar ruler, the marker strip and the waveform;
  - the marker inspector (Type, Number, "Starts at", "Length", "Infinite loop by default", "Delete marker");
  - the "Arrangement" panel;
  - the Stems panel, where each stem's part can be changed.
- **Assumption:** "Save song" validates the package and rewrites `song.json` atomically. Discard drops the edits. Positions stay sample offsets, so a tempo edit keeps every marker on the same audio.
- **Assumption:** a song saved while it is open on Perform keeps playing as it was loaded, and the edits apply the next time it opens.
- User (L11): "Ajustar solos". On save, setlist items that play part of an edited arrangement (F5.3 start and end positions) are remapped, so they keep playing the same audio. If a range can no longer be kept (a position it used was removed, or it is no longer contiguous), the save asks first and names the setlist. "Save anyway" clears that item's range, and "Keep editing" cancels. A setlist open in the Library is read again after the save, and quitting waits for a song save in flight.
- **Assumption:** the import editor stays as it is (one Intro section). Its sections are edited after the save, from the song's edit link.

### S5 — Perform song map
- **Assumption:** the design's Perform song map shows the waveform: one bar per musical bar inside each position's block, played bars at full opacity and bars ahead at 0.38. Each position shows the audio of its own source range, since positions repeat and reorder. Until the overview is ready, the map shows today's blocks.

### S6 — Docs
- `docs/mvp-spec.md` marks F2's acceptance items; `README.md` and `apps/desktop/README.md` describe the editing and the waveform. `docs/architecture.md` describes the `waveforms/` cache.

## Tasks

| ID | Specs | Route | Status | Commits |
|----|-------|-------|--------|---------|
| T1 | S1, S2 (domain) | delegated (parallel unit with T2, disjoint surface `crates/domain`) | done (L3; verified L4; review approved L6) | 57965f1 |
| T2 | S3, S4 save (library) | delegated (parallel unit with T1, disjoint surface `crates/library`) | done (L5; verified L7; review approved L8) | d35205d |
| T3 | S1–S4 (desktop Song editor) | delegated (context backstop); needs T1 and T2 | done (L9); split, verifier and screenshot check pending | |
| T4 | S5 (Perform song map) | delegated (context backstop); needs T2 and T3 | done (L10); verifier and screenshot check pending | see L10 |
| T5 | S6 | inline, with each task's own docs | pending | |

Risk: T2 rewrites existing `song.json` files (item 1, changing stored data) and T3 drives background jobs (item 4), so each gets an independent verifier.

## Log

- **L1** (2026-11-27, user, verbatim): "hacerlo ya con la forma de onda". This answers the parent's question whether to build F2 with a bar ruler only or with the waveform now. The waveform had been deferred on 2026-11-19.
- **L2** (2026-11-27): map from a read-only explorer.
  - **Domain:**
    - `SectionMarker`, `SectionType` (15 kebab-case variants, no `ALL`) and `Arrangement` (`sections` may repeat; `infinite_loop` per arrangement) are in `crates/domain/src/package.rs`.
    - `validate` requires a first section at 0, strictly increasing starts, a Default arrangement, no empty arrangement and known ids. It does not check that an `infinite_loop` id is in its arrangement.
    - There is no editing API for sections or arrangements. The setlist edit pattern to follow is in `crates/domain/src/setlist.rs` (`SetlistEditError`, `&mut self` edits, a separate `validate`).
    - There is no public bar-to-sample function: `BeatGrid` maps samples to bars, and `import.rs::bar_lines` is private.
  - **Library:**
    - Nothing rewrites an existing `song.json`; `write_file_atomically` is crate-private.
    - `read_through` streams WAV and FLAC stems in 4,096-frame blocks with a cancel check, which is the template for a peaks pass.
    - `waveforms/` is not in the code.
  - **Desktop:**
    - The Song editor (`song_import.rs`, `song_editor_ui.rs`) only wraps an import draft, and the Library's song rows have no edit link.
    - The Perform song map (`ui.rs:948`) draws blocks, a track and a ruler with no waveform.
    - Background jobs use `cx.background_executor()` with tickets, and `facts.rs` has a per-folder cache to copy.
  - **Design:** `SongEditor.dc.html` holds the marker panel (waveform one bar per musical bar), the inspector and the arrangement panel; `Main.dc.html`'s song map shows the waveform. The explorer's handoff lists the colors and sizes.
  - Free disk: 14 GiB.
- **L3** (2026-11-27): T1 delivered.
  - **New code:**
    - `domain::edit::SongEditor` over a `SongPackage` plus its length: markers (`add_marker`, `move_marker`, `set_marker_type`, `delete_marker`), arrangements (`new_arrangement`, `rename_arrangement`, `delete_arrangement`, `insert_section`, `append_section`, `remove_section`, `move_section`, `set_infinite_loop`), and `snap`, `section_range`, `bars_in`, and `summary` (Display "4 sections · 12 bars · 0:24").
    - `Snap { Bar, Beat }` and a typed `SongEditError`, with sections named by label and arrangements by name. Each edit runs on a copy that must pass `validate`.
    - `SectionType::ALL`, and `BeatGrid::from_maps`, `bar_lines` and `beat_lines`. The private `import.rs::bar_lines` now uses them, so there is still one grid.
  - **Writer's assumptions, kept:**
    - Default can be neither renamed nor deleted.
    - A new arrangement starts with every section once, in song order.
    - Splitting a looped section keeps the flag on its first part.
    - New ids are slugs (`verse-2`).
    - A tie snaps to the earlier line.
    - A bar counts in the section it starts in.
  - **For T3:** deleting an arrangement leaves stale entries in `mix.json` and in setlists that name it, so the desktop must handle that.
  - **Checks:**
    - RED: the tests did not compile first. Five deliberate mutations were each caught.
    - `crates/engine/tests/arrangement_seams.rs` pins the F2 seam acceptance; it fails with the crossfade turned off.
    - `cargo test -p domain` and the clippy, fmt, doc and workspace checks are clean. The parent re-ran `--test song_edit` (24) and `--test grid` (10).
  - Risk: item 3 (shared helpers in import.rs: stem ids and bar lines; covered by the existing import tests and a new grid regression test).
- **L4** (2026-11-27): the T1 verifier passed every probe, with no blockers.
  - **Grid:** bar and beat lines equal a hand calculation on three songs.
  - **Add and split:** 18,270 add attempts, all snapped as computed by hand, and every split kept each arrangement's audio.
  - **Random sequences:** 36,000 random edits, each keeping `validate` passing; every refusal left the package equal.
  - **Acceptance:** the move acceptance holds in two arrangements, and the seam test fails on two hard-cut mutants.
  - **No change elsewhere:** `SongDraft::maps`, stem ids and `BeatGrid` queries are byte-identical at baseline and candidate over 135 drafts and 7 songs. `cargo test -p domain` passed 198.
  - **Advisories:**
    - a split looped section keeps the flag on its first part only;
    - duplicate arrangement names were possible, including a second "Default";
    - long names give ids up to 66 bytes;
    - a name with no ASCII gives the id `arrangement`.
  - **Parent fix:** arrangement names are now unique without regard to case. `new_arrangement` and `rename_arrangement` refuse a taken name with `SongEditError::NameTaken` ("an arrangement named \"…\" already exists"), and renaming an arrangement to its own name in another case is allowed.
    - RED: the new test did not compile against the old error type.
    - GREEN: `cargo test -p domain` passes 199; fmt and clippy are clean.
    - Amended into the T1 commit, which had not been pushed.
- **L5** (2026-11-27): T2 delivered.
  - **`library::waveform`:**
    - `Overview` holds the peak of the track stems mixed at unity, about 100 values per second (480 frames at 48 kHz), with `range_level` and `peak`.
    - `load_overview(dir)` reads a fresh cache without decoding; `build_overview(dir, progress, cancel)` reads the stems in lockstep through the player's readers and writes the cache atomically.
    - The cache is `waveforms/overview.bin`, format v1: a magic, the stems' fingerprints (name, role, size, modified time), u16 values and an FNV-1a hash. It holds about 61 KB for 5 minutes and 24 stems.
  - **`Library::save_song(song, &SongPackage)`:**
    - It validates the package against the stems, as loading does, and rewrites `song.json` atomically.
    - Fields the package does not own survive at every level: list items are matched by id, map points by `at`.
    - It refuses a read-only, malformed or newer-format `song.json`.
  - **Shared helpers changed:** `check_package` (used by load and save), `read_confined_bytes`, and `write_file_atomically` (now takes `AsRef<[u8]>`).
  - **Checks:**
    - RED: 13 waveform and 9 save tests failed against stubs.
    - GREEN: `cargo test -p library` passes everything (4 ignored measurements); clippy, doc and the workspace check are clean.
    - The parent re-ran `--test waveform` (13) and `--test save_song` (10).
    - Release build of 24 five-minute stems: 4.1 s (WAV) and 4.7 s (FLAC); a cache read takes 0.6 ms.
  - Risk: item 1 (it rewrites existing `song.json` files) and item 3 (shared load, setlist and settings helpers), so T2 gets an independent verifier.
- **L6** (2026-11-27): T1's native review (lineage review-83f1cc62ad90ed8c, 2827125..57965f1) approved and was acknowledged, so the reviewed boundary is now 57965f1. Non-blocking findings, kept as follow-ups:
  - WARNING ×2: the edit.rs docs promise that a split keeps every arrangement's audio, but a looped section split keeps its loop flag on the first part only, and no test pins that;
  - WARNING: `SectionType::ALL` is not tied to the enum at compile time;
  - SUGGESTION: the 24-hour song limit is computed inline in `from_maps`;
  - SUGGESTION: `import::bar_lines` hides a broken invariant with `unwrap_or_default`.
- T2's independent verifier and the T3 writer (the desktop Song editor) were launched at the same time; the verifier works in a scratch copy.
- **L7** (2026-11-27): the T2 verifier passed every probe, with no blockers.
  - **Overview:** it matches an independent reference over 24 random songs and every format and channel pair, with click and guide stems excluded, and no allocation above 64 KiB.
  - **Cache:**
    - it is read in about 350 µs from stem stats only, and is stale on any fingerprint change;
    - hostile caches (every truncation and flipped byte, huge counts, symlinks) are stale, never a panic;
    - read-only folders still give the overview, and cancel leaves nothing.
  - **Save:**
    - it round-trips;
    - ten invalid packages are refused with the tree unchanged;
    - unknown fields survive on kept items and are dropped with deleted or re-id'd items;
    - every error path writes nothing.
  - **No change elsewhere:** load, list, mix, setlist and settings behaviour, and render-cli output, are byte-identical at baseline and candidate. `cargo test -p library` passed 195.
  - **Advisories:**
    - a 0600 `song.json` becomes 0644 after a save;
    - a symlinked `song.json` inside the folder is replaced by a file;
    - the FNV hash is not tamper-proof;
    - a junk cache up to 64 MiB is read before it is rejected.
  - **Review:** the user granted T2's review ("Revisar este cambio"), slice 57965f1..d35205d, lenses risk, resilience and reliability.
- **L8** (2026-11-27): T2's native review (lineage review-bd5aeee7f21cbb92, 57965f1..d35205d) approved and was acknowledged, so the reviewed boundary is now d35205d. Non-blocking findings, kept as follow-ups:
  - WARNING ×2: on coarse-timestamp drives (FAT, exFAT, HFS+), a stem re-exported at the same size within one timestamp tick keeps a stale cache; a cheap content check, such as a hash of the first and last blocks, would close it;
  - SUGGESTION: a failed cache write is silent;
  - SUGGESTION: `save_song` has no compare-and-swap against another writer;
  - SUGGESTION: with unknown fields present, keys are rewritten in alphabetical order;
  - SUGGESTION: map-point matching by `at` is untested for inserted points;
  - SUGGESTION: `load_overview` reads `song.json` unbounded and follows symlinks; it should use `read_confined`.
- **L9** (2026-11-27): T3 delivered.
  - **Domain:** a public `BarMaps` (`from_maps`, `to_maps`, `BarMapsError::OffBarLine`), which `SongDraft` now uses.
  - **Desktop:**
    - `song_header.rs` (the header shared with the import editor);
    - `song_edit.rs` with `jobs.rs` (open, waveform, save; cleans `mix.json` of deleted arrangements; reads which setlists play each arrangement) and `view.rs`;
    - `song_sections.rs` (ruler, blocks, waveform heights mapped into 16–94 %) and `song_sections_ui.rs` (markers panel, inspector, Arrangement panel);
    - the edit link on Library rows (a waveform icon with the tooltip "Edit <title>");
    - a `Leaving` question (Discard changes / Keep editing) before Discard, another library, quit or close;
    - 5 icons and the README.
  - **Checks:**
    - `cargo test -p openvamp` passes 321 (35 new in tests/song_edit.rs), and `cargo test -p domain` passes 203; clippy, fmt and the workspace check are clean. The parent re-ran `--test song_edit` (35).
    - RED came first for the domain only. For the desktop, 12 deliberate mutations were each caught.
  - **Known design differences:**
    - the selection's right edge is solid, not dashed;
    - the selected block has a single ring;
    - an amber cursor;
    - Type is a list of buttons;
    - rename and delete are icons after New;
    - a tempo panel sits under the inspector;
    - a beat position reads "Bar 9.3";
    - no scrollbar.
  - **Open points:**
    - a quit that cannot ask (Dock, logout) drops unsaved edits;
    - a part change makes the cached waveform stale until the editor reopens;
    - setlists are matched to the song by its exact path;
    - opening runs `list_songs`.
  - **Size:** the change is about 6,400 lines, too big for one review slice, so the writer is splitting it into 2–3 compiling commits in a scratch clone. The parent will apply them.
  - Risk: item 3 (the import header and `SongDraft` maps, which are shared) and item 4 (background jobs and cancel), so T3 gets an independent verifier.
- **L10** (2026-11-27): the T3 split landed as three commits:
  - 0d0975e: domain `BarMaps`, the shared header and `song_sections.rs`;
  - 19e1a0f: the editor's view model, jobs and Library wiring;
  - dc274d7: the drawing, theme, icons and README.

  Each compiles and passes on its own. The applied series matched the tested working tree file for file, and the T3 verifier was launched on dc274d7.

  T4 delivered:
  - **`map_wave.rs` (`MapWaveform`):** it reads the cache for the song on Perform and builds it with the editor's `WaveformJob`.
  - **`view.rs` and `ui.rs`:** the song map's track shows one bar per source bar inside each position, 18–96 % high, with played bars at opacity 1 and bars ahead at 0.38. The playhead gets a 1 px background ring.
  - **Shared bar height:** `song_sections::bar_height` is now used by both the editor and Perform.
  - **Stems panel:** stem rows show the file name only.
  - **README:** describes the Perform waveform.

  **Parent decision (live safety):** Perform builds a missing overview only while the transport is stopped or paused. Play cancels a running build within a tick (the cancel leaves no cache). A cached overview shows at any time, because `load_overview` only stats files.

  **Checks:**
  - RED first for each rule, and 4 deliberate mutations were caught.
  - `cargo test -p openvamp` passes 338; fmt, clippy and the workspace check are clean. The parent re-ran `--test song_map_wave` (12).
  - Size: about 1,300 changed lines.

  **Still open:** a failed read is not retried while its song shows; a Perform read and an editor build can run at once (safe, since both write atomically).

  Risk: item 4 (background jobs tied to the transport), so T4 gets an independent verifier.
- **L11** (2026-11-27): the T3 verifier found one blocker; everything else passed.
  - **What passes:**
    - 2,400 random editor steps matched `domain::SongEditor`, with the refusal notes word for word;
    - the waveform heights are right by hand, and placeholders, redraw without rebuild and cancel on close all work;
    - save, leaving and the unsaved question, `mix.json` cleanup, unknown fields and the read-only error work;
    - header maps round-trip;
    - the import editor is byte-identical at baseline and candidate (400 drafts, 12 × 150 controls);
    - stale results are ignored;
    - `cargo test -p openvamp` passes 325 and `-p domain` 203.
  - **B1:** setlist items address arrangement positions by index, so a split, a removed chip or a reorder silently changes what a setlist with start and end positions (F5.3) plays, or makes it invalid. An open setlist editor is not refreshed.
  - **Advisories:**
    - quit does not wait for a library song save in flight;
    - a stale note after a bad number;
    - the import notice is hidden behind the editor;
    - "Mix.json" is capitalized in a warning;
    - setlists that cannot be read are left out of the in-use check.
  - **User decision (verbatim):** "Ajustar solos". The option chosen: "Al guardar, el setlist se corrige para que siga tocando el mismo audio: un marcador nuevo o un reordenamiento mueven sus límites. Si un bloque que usa el setlist desaparece, se pregunta antes de guardar y se nombra el setlist."
  - **S4 assumption added:**
    - On save, setlist ranges that use an edited arrangement are remapped to the same audio.
    - When a range can no longer be kept, the save asks first, naming the setlist. "Save anyway" clears that item's range, and "Keep editing" cancels.
    - A setlist open in the Library is refreshed.
    - Quit waits for a song save in flight.
  - **Correction:** the T3 writer was resumed for the fix, which lands after T4.

