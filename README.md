# binmerge-rs

A fast Rust tool for merging multi-bin CUE sheets into a single bin/cue pair and splitting them back into per-track bins.

```
binmerge-rs <cuefile> [basename]
binmerge-rs --split <cuefile> [basename]
binmerge-rs --batch <directory>
binmerge-rs --dry-run <cuefile>
```

## What it does

**Merge** combines all `.bin` files referenced by a multi-file CUE sheet into one contiguous `.bin`, and writes a new `.cue` with unified timestamps.

**Split** takes a single merged `.bin` + its merged CUE and extracts each track into individual `.bin` files (Redump naming convention), producing a multi-file CUE.

Handles any CD blocksize: `AUDIO` (2352), `CDG` (2448), `MODE1/2352` (2352), `MODE1/2048` (2048), `MODE2/2352` (2352), `MODE2/2336` (2336), `CDI/2352` (2352), `CDI/2336` (2336).

## Usage

### Merge (default)

```sh
binmerge-rs "game.cue" "game-merged"
# Basename is now optional — defaults to the CUE filename:
binmerge-rs "game.cue"
```

Reads `game.cue`, concatenates all referenced `.bin` files into `game-merged.bin`, and writes `game-merged.cue` with merged indexes.

### Split

```sh
binmerge-rs --split "game-merged.cue" "game"
binmerge-rs -s "game-merged.cue"
```

Reads the single-bin CUE, extracts each track into `game (Track NN).bin` files, and writes `game.cue` referencing them.

### Batch processing

```sh
binmerge-rs --batch ./games/
binmerge-rs -b ./games/ -o ./merged/
```

Finds all `.cue` files in the directory and processes each one. Output names are auto-derived from CUE filenames.

### Dry-run

```sh
binmerge-rs --dry-run "game.cue"
```

Parses the CUE, validates all referenced files exist, and prints a summary (track count, total size, metadata) without writing any output.

### Other flags

| Flag | Effect |
|---|---|
| `-o`, `--outdir <dir>` | Write output to `<dir>` instead of the CUE file's directory |
| `-p`, `--progress` | Show real-time progress bar on stderr |
| `-v`, `--verbose` | Print debug timing information |
| `--overwrite` | Allow overwriting existing output files |

### Metadata preservation

binmerge-rs preserves `REM` comments, `PREGAP`, `POSTGAP`, `CATALOG`, `FLAGS`, `ISRC`, and CD-TEXT (`TITLE`, `PERFORMER`, `SONGWRITER`) directives through both merge and split operations.

## Install

```sh
cargo install --git https://github.com/pxdl/binmerge-rs.git
```

Or build from source:

```sh
git clone https://github.com/pxdl/binmerge-rs.git
cd binmerge-rs
cargo build --release
```

Binary will be at `target/release/binmerge-rs`.

## Why

Multi-track CD images ripped with per-track `.bin` files need their individual files recombined into one contiguous image for many emulators and tools. Splitting is the inverse: useful when tools or preservation workflows require per-track bins.

binmerge-rs is a fast, cross-platform alternative to other related tools. It streams with a 1 MiB buffer and parses lazily, making it suitable for large (multi-GB) images.

## License

MIT
