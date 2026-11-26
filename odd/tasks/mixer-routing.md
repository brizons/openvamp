# Feature: mixer, buses and output routing

Objective: mix stems through buses with per-stem, per-bus and master levels, and route each bus to the interface's outputs, saved per arrangement and per device, in the engine, the `app` API and the desktop app.
Branch: `feat/mixer-routing` (from `setlists/apps` at a3b3e12, now in `main` through PR #14) · Delivery: work-unit commits, stacked PRs to `main` · Test runner: `cargo test --workspace` with Rust 1.93.0 (`rust-toolchain.toml`).

## Specs

Requirements quote `docs/mvp-spec.md` verbatim. Items marked **Assumption** are not in that document and are the parent's call until the user says otherwise.

### S1 — Mixer and buses (F7)
- F7.1: "Each stem has volume, mute, and solo; each bus has volume and mute; there is one master volume."
- F7.2: "Stems go to buses automatically by part (F1.2); the user can reassign a stem to another bus."
- F1.2: "Assign a part to each stem automatically from its file name, using our own rules table; the user can override it."
- F7.3: "Solo never mutes click or guide."
- F7.4: "Volume changes ramp smoothly, with no zipper noise."
- F7.5: "The mix is saved per arrangement; bus settings are global unless the user enables per-song buses."
- Acceptance: "Muting a bus silences every stem in it and restores their levels on unmute."
- **Decision (user, L2):** the mix of each arrangement (stem levels, mutes, solos, bus assignments) is saved in a `mix.json` beside `song.json` in the song folder; `song.json` is never rewritten. Global bus settings live in the app's configuration folder.
- **Assumption:** six fixed buses as in the design canvas: Click & Guide, Drums, Bass, Keys & Pads, Guitars, Vocals; the generated click and click/guide stems go to Click & Guide.
- **Assumption:** solo applies to stems only (no bus solo, as in the design); a muted bus stays silent even with a soloed stem in it.
- **Assumption:** in a setlist each song plays with its own saved mix; bus levels are global, so they carry from song to song.

### S2 — Output routing (F8)
- F8.1: "List the audio devices available and their output channel count."
- F8.2: "Route each bus to a mono output or a stereo pair, up to 32 outputs."
- F8.3: "Apply a default routing preset based on the device's output count; the user can edit it and the routing is saved per device."
- F8.4: "**Split mode** for 2-output devices: click and guide on the left, the stems summed to mono on the right."
- F8.5: "Choose the buffer size (64 to 1024 frames) and show the resulting latency."
- Acceptance: "Unplugging the device pauses playback with a clear message; plugging it back in restores the routing."
- **Decision (user, L2):** routing is saved per device in the app's configuration folder.
- **Assumption:** a mono output receives the bus's left and right summed; several buses may share an output (they are summed), as the design's bus layer shows.
- **Assumption (rewritten in L3 from the design data):** default presets by output count: 1 output all on 1; 2 outputs split mode on; 3 outputs Click & Guide on 1 and the rest on 2/3; 4-5 outputs Click & Guide on 1/2 and the rest on 3/4; 6-7 outputs one mono output per bus; 8-32 outputs Click & Guide 1, Drums 2, Bass 3, Guitars 4, Keys & Pads 5/6, Vocals 7/8; destinations that do not fit the device fall back to outputs 1/2.
- **Assumption:** at unity gain with the stereo routing on outputs 1/2, the output stays bit-identical to today's (offline renders, `render-cli`, live null tests unchanged).

### S3 — In the apps
- The design canvas's Mixer screen (stems and buses layers, "Save mix to arrangement", "Reset to song mix") and Audio output screen (device, sample rate shown, buffer 64–1024 with latency, split mode, routing matrix with presets and reset).
- **Assumption:** `play-cli` and `render-cli` keep today's stereo behaviour; `play-cli` may gain options to pick a routing preset for hardware spikes.

Out of scope: MIDI (F9), guide-voice cues (F4), per-song buses (F7.5's opt-in), meters beyond what the screens show, effects.

## Tasks

| ID | Specs | Route | Status | Commit |
|----|-------|-------|--------|--------|
| T1 | S1, S2 | delegated (context backstop) | done (review approved, L5) | 4ff89e8, 107f8e9, 3b3c432, 0884c85, 650ef50, e4266bf |
| T2 | S1, S2 | delegated (context backstop) | in progress | — |
| T3 | S1, S2 | delegated (context backstop) | pending | — |
| T4 | S1, S2 | delegated (context backstop) | pending | — |
| T5 | S3 | delegated (context backstop) | pending | — |

T1 `domain`: buses, part-to-bus rules table, arrangement mix model and `mix.json` v1, routing model, presets and split mode. T2 `library` and app config: read and write `mix.json` atomically, per-device routing and global bus settings in the config folder. T3 `engine`: per-stem, per-bus and master gain ramps, mute and solo rules, bus rendering and the router into N channels, allocation-free, unity bit-exact. T4 `io-cpal` and `app`: multichannel open, routing in the output request (restored on reopen), mixer and routing commands, snapshot levels, setlist carry. T5 desktop: Mixer and Audio output screens.

Route evidence: the parent session is long and past the ~150k-token backstop. Order: model, storage, engine, app, screens; each needs the previous one. Forecast: ~4,700 authored lines. Risk forecast: T1 item 3 (a new format others may read, `mix.json`, additive); T2 item 1 (writes into song folders and a config folder); T3 item 4 and 6 (audio thread, bit-exact goldens); T4 item 3 (the output port contract changes from stereo to N channels); T5 item 6 (GPUI drawing).

## Log

- **L1** (2026-11-20, user, verbatim): "ahora si está bien, creo que podemos pasar a la siguiente fase". Asked which phase follows the setlists merge, the user chose verbatim "Mezcla + ruteo (F7+F8) (Recommended)". The parent merged PRs #11, #12 and #13 into `main` in order (merge commits; CI green on Linux, macOS and Windows), retargeted #14 to `main` and waits for its CI (the repository does not allow auto-merge). A read-only explorer mapped the code: the engine, the `AudioRenderer` port and `io-cpal` carry one interleaved stereo pair and `write_interleaved` writes silence above output 2; `io-cpal::choose_config` opens the fewest channels at or above two (pinned by tests); `domain::Stem.part` is unvalidated free text and the package has no bus, mix or routing fields; the app persists no settings; summing through buses would reorder float additions and break the bit-exact tests unless the stereo mixdown keeps a stem-order sum with effective gains.
- **L2** (2026-11-20, user, verbatim): asked where the mix of each arrangement and the per-device routing are saved, the user chose "mix.json en la carpeta (Recommended)": the mix per arrangement in a `mix.json` beside `song.json` in the song folder (`song.json` is never rewritten); global bus settings and per-device routing in the app's configuration folder.
- **L3** (2026-11-21): T1 by its writer in five commits (+2,847, about half tests; no existing code, format or test changed): 4ff89e8 (`Bus` with camelCase ids and names; `bus_for_stem`: click/guide roles go to Click & Guide, then the `part` text, then the file name, then `DEFAULT_BUS` = Keys & Pads; normalization splits at non-letters and case changes, the last instrument word wins, generic words decide only without one, two-word phrases first), 107f8e9 (`Volume` in dB with `"-inf"` silent and +12 dB max, exactly 1.0 at 0 dB; `StemMix`, `ArrangementMix`, `MixFile` = `mix.json` v1 with every validation problem reported at once, defaults for missing entries), 3b3c432 (`BusSettings` with bus levels and master; `effective_gains` in stem order: stem × bus × master in f64 cast to f32, bit-exact 1.0 at unity; mute wins over solo; click/guide role or Click & Guide bus never silenced by solo; a muted bus silences its stems and restores them on unmute), 0884c85 (`Destination` mono or stereo pair from output 1; `Routing` with output count, split mode, bus destinations and an optional master destination; presets by output count as rewritten in S2; `stereo()` = every bus on 1/2 as today; `fit` sends what does not fit to 1/2; `RoutingFile` per device), 650ef50 (docs/architecture.md lists the three files). RED then GREEN per rule; 592 tests pass, 6 ignored; fmt, clippy and rustdoc with `-D warnings` clean; the parent re-ran fmt and the suite (592). Writer's readings to confirm later: master destination as an optional extra output carrying the main mix without Click & Guide; split mode needs 2+ outputs. The rules table is English only; Spanish words (batería, bajo, voz, coro, guitarra, teclado...) sent as a follow-up because the library's songs may be named in Spanish. PR #14 CI went green and the parent merged it into `main` (a3599dd), closing the setlists feature. Risk: item 3 (new public formats), additive.
- **L4** (2026-11-21): e4266bf (+178 −15): Spanish words in the rules table (batería, bombo, caja, redoblante, platillo, percusión, secuencia; bajo; teclado, órgano, sintetizador, sinte, colchón, cuerda, violines, chelo, viento, metales, trompeta, saxo, flauta, campana; guitarra, requinto, charango, eléctrica/o, acústica/o; voz, voces, coro, armonía; clic, metrónomo, guía, cuenta), accents folded in the tokenizer, phrases "bajo sinte", "bajo sintetizador" and "voz guía" (Vocals, since Spanish puts the noun first); irregular plurals listed whole so English words such as "metal" stay out. RED then GREEN (3 tests); 595 tests pass; the parent re-ran the domain tests.
- **L5** (2026-11-21): native review of T1 (d9be060..e4266bf, 11 files, 3018 lines; lenses risk, readability, reliability; consent granted by the user) approved and acknowledged (review-79679f89ecf6ce60). One warning, folded into T2: the valid-destination rule is written twice (`Destination::fits` and the private `check_destination` behind `Routing::validate`). T2 starts with the same writer.
