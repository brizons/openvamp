<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/brand/openvamp-mark.svg">
    <img src="docs/brand/openvamp-mark-light.svg" width="96" height="96" alt="OpenVamp logo">
  </picture>
</p>

<h1 align="center">OpenVamp</h1>

<p align="center">Open-source live multitrack playback for worship bands.</p>

OpenVamp is built to play a band's own backing stems live, with a click, spoken guide cues, section jumps, loops and fades. It targets Mac, Windows, Linux, iPhone, iPad and Android, and it uses an open song format instead of a content store.

## Status

Early development. Built so far:

- **Rust core:** the song package format, the arrangement compiler, a deterministic offline renderer, and a real-time live player with queued jumps, loops, fade and panic.
- **Disk streaming:** stems stream from disk, so memory use stays nearly flat as songs get longer.
- **`app` API:** one API of commands and state snapshots that every app uses, with a `cpal` audio output.
- **`play-cli`:** a terminal player with single-key controls, for one song or a setlist.
- **Song import:** a folder or ZIP of WAV, FLAC, MP3 or M4A stems becomes a song package; parts come from the file names, and MP3 and M4A are stored as FLAC.
- **Desktop app:** the Perform screen, which plays one song or a setlist over the song's waveform; the Mixer and Audio output screens; and a Library screen that edits setlists, imports stems, and opens the Song editor to mark sections on the waveform and build arrangements, built with [GPUI](https://github.com/zed-industries/zed).

Not built yet: MIDI control, and the iPhone, iPad and Android apps. The plan is in [docs/mvp-spec.md](docs/mvp-spec.md), and the design is in [docs/architecture.md](docs/architecture.md).

## Try it

You need Rust 1.88 or later.

Run the tests:

```bash
cargo test --workspace
```

Write a demo song folder:

```bash
cargo run -p openvamp --example demo_song -- <DIR>
```

Open it in the desktop app:

```bash
cargo run -p openvamp --release -- <DIR>
```

Play it from the terminal:

```bash
cargo run -p play-cli -- <DIR>
```

Render it to a WAV file:

```bash
cargo run -p render-cli -- <DIR> --output song.wav
```

Each binary has its own README with options and keys: [apps/desktop](apps/desktop/README.md), [crates/play-cli](crates/play-cli/README.md), [crates/render-cli](crates/render-cli/README.md).

## Repository

| Path | What it holds |
|------|---------------|
| `crates/domain` | Song model, validation and the arrangement compiler (pure, no I/O) |
| `crates/engine` | Offline renderer and the real-time live player |
| `crates/library` | Song package loading, WAV output and disk streaming |
| `crates/app` | Use cases and the one API the apps call |
| `crates/io-cpal` | Audio output through `cpal` |
| `crates/play-cli`, `crates/render-cli` | Command-line tools |
| `apps/desktop` | The GPUI desktop app |
| `docs/` | Spec, architecture and brand files |
| `odd/tasks/` | Feature plans and their logs |

## Clean room

OpenVamp reproduces the *behaviour* of commercial playback apps, learned from public documentation and observation. It contains no code, sounds, voices or song content from any of them.

## License

[MPL-2.0](LICENSE).
