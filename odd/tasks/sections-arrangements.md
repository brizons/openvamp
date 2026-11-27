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
- **Assumption:** the import editor stays as it is (one Intro section). Its sections are edited after the save, from the song's edit link.

### S5 — Perform song map
- **Assumption:** the design's Perform song map shows the waveform: one bar per musical bar inside each position's block, played bars at full opacity and bars ahead at 0.38. Each position shows the audio of its own source range, since positions repeat and reorder. Until the overview is ready, the map shows today's blocks.

### S6 — Docs
- `docs/mvp-spec.md` marks F2's acceptance items; `README.md` and `apps/desktop/README.md` describe the editing and the waveform. `docs/architecture.md` describes the `waveforms/` cache.

## Tasks

| ID | Specs | Route | Status | Commits |
|----|-------|-------|--------|---------|
| T1 | S1, S2 (domain) | delegated (parallel unit with T2, disjoint surface `crates/domain`) | pending | |
| T2 | S3, S4 save (library) | delegated (parallel unit with T1, disjoint surface `crates/library`) | pending | |
| T3 | S1–S4 (desktop Song editor) | delegated (context backstop); needs T1 and T2 | pending | |
| T4 | S5 (Perform song map) | delegated (context backstop); needs T2 and T3 | pending | |
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
