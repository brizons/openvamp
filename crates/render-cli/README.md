# render-cli

Headless renderer for tests and CI. It renders one arrangement of a song folder offline to a stereo 32-bit float WAV file, with no audio hardware.

```text
render-cli <SONG_DIR> --output <FILE> [--arrangement <ID>] [--no-click]
```

| Flag | Meaning |
|------|---------|
| `<SONG_DIR>` | Song folder: `song.json` plus the stems it lists, mono or stereo: WAV (16-, 24-, or 32-bit integer or 32-bit float) or FLAC (4- to 32-bit). A stem's format comes from its first bytes, not its extension. |
| `--output <FILE>` | WAV file to write, at the song sample rate. |
| `--arrangement <ID>` | Arrangement to render. Default: `default`. |
| `--no-click` | Do not generate the click. A recorded `click` stem still plays. |

## Example

```console
$ render-cli songs/example --output example.wav --arrangement live
rendered "Example Song" (arrangement "live"): 5 segments, 3:12, 230 clicks -> example.wav
```

The summary is the only line on stdout; control characters in it, such as a newline in the title, are escaped. The click count is the generated ticks that are audible: none with `--no-click` or when the song has a `click` stem.

The file is written to a temporary file next to `--output` and renamed over it only when complete. A WAV file holds at most 4 GiB of audio (536 870 904 stereo 32-bit float frames, about 18 h 38 min at 8 kHz or 3 h 6 min at 48 kHz); a longer render is rejected before any audio is produced.

## Exit codes

| Code | When | Output |
|------|------|--------|
| 0 | The file was written. | The summary line on stdout; warnings on stderr, each prefixed `warning:`. |
| 1 | The song folder cannot be loaded or validated, the arrangement does not exist, the render would not fit in a WAV file, or rendering or writing fails. | Every error on its own stderr line, prefixed `error:`. An existing output file is left byte-identical, and no partial or temporary file is left behind. |
| 2 | Usage error, such as a missing `--output`. | clap's message on stderr. |

Rendering is deterministic: the same song and flags always write a byte-identical file.
