use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Instant;

use clap::Parser;
use regex::Regex;

// ─────────────────────────── Regex patterns ───────────────────────────

static FILE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"FILE "?(.*?)"? BINARY"#).unwrap());
static TRACK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"TRACK (\d+) ([^\s]*)").unwrap());
static INDEX_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"INDEX (\d+) (\d+:\d+:\d+)").unwrap());
static CUESTAMP_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\d+):(\d+):(\d+)").unwrap());

const BUF_SIZE: usize = 1024 * 1024; // 1 MiB

// ─────────────────────────── Data structures ──────────────────────────

struct Index {
    number: u32,
    /// Sector offset from the start of the bin file containing this index.
    file_offset: u32,
}
struct Track {
    number: u32,
    track_type: String,
    indexes: Vec<Index>,
    /// Track length in sectors (populated for single-file / split mode).
    sectors: Option<u32>,
}
struct BinFile {
    filename: String,
    tracks: Vec<Track>,
    /// File size in bytes.
    size: u64,
}

// ──────────────────────── CLI ─────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "binmerge-rs",
    about = "Merge multi-bin CUE sheets into a single bin/cue pair (or split them back)."
)]
struct Args {
    /// Path to the .cue file
    cuefile: String,

    /// Base name (without extension) for the output .bin/.cue files
    basename: String,

    /// Reverse: split a merged bin back into per-track bins
    #[arg(short = 's', long = "split")]
    split: bool,

    /// Output directory (default: same dir as cue file)
    #[arg(short = 'o', long = "outdir")]
    outdir: Option<String>,

    /// Verbose output
    #[arg(short = 'v', long = "verbose")]
    verbose: bool,
}

// ──────────────────────── Timestamp helpers ──────────────────────────

/// Convert "MM:SS:FF" → sectors (75 frames/s, 60s/min).
fn cuestamp_to_sectors(stamp: &str) -> Result<u32, &'static str> {
    let caps = CUESTAMP_RE.captures(stamp).ok_or("Invalid timestamp format")?;
    let m: u32 = caps[1].parse().map_err(|_| "bad minutes")?;
    let s: u32 = caps[2].parse().map_err(|_| "bad seconds")?;
    let f: u32 = caps[3].parse().map_err(|_| "bad frames")?;
    Ok(f + s * 75 + m * 60 * 75)
}

/// Convert sectors → "MM:SS:FF".
fn sectors_to_cuestamp(sectors: u32) -> String {
    let minutes = sectors / 4500;
    let remainder = sectors % 4500;
    let seconds = remainder / 75;
    let frames = remainder % 75;
    format!("{minutes:02}:{seconds:02}:{frames:02}")
}

// ──────────────────────── Blocksize ───────────────────────────────────

/// Map track type string to byte blocksize.
fn blocksize_for_type(track_type: &str) -> Option<u32> {
    match track_type {
        "AUDIO" | "MODE1/2352" | "MODE2/2352" | "CDI/2352" => Some(2352),
        "CDG" => Some(2448),
        "MODE1/2048" => Some(2048),
        "MODE2/2336" | "CDI/2336" => Some(2336),
        _ => None,
    }
}

// ──────────────────────── CUE parser ──────────────────────────────────

/// Parse a .cue file into a list of BinFile structs.
///
/// Each FILE directive starts a new bin; TRACK and INDEX directives
/// are associated with the most recent FILE.
fn parse_cue(cue_path: &Path) -> io::Result<Vec<BinFile>> {
    let cue_dir = cue_path.parent().unwrap_or(Path::new("."));
    let file = File::open(cue_path)?;
    let reader = io::BufReader::new(file);

    let mut bin_files: Vec<BinFile> = Vec::new();

    for line in reader.lines() {
        let line = line?;

        if let Some(caps) = FILE_RE.captures(&line) {
            let name = caps
                .get(1)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "FILE regex matched but capture group missing"))?
                .as_str();
            let path = cue_dir.join(name);

            if !path.is_file() {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("Bin file not found: {}", path.display()),
                ));
            }
            let size = fs::metadata(&path)?.len();
            bin_files.push(BinFile {
                filename: name.to_string(),
                tracks: Vec::new(),
                size,
            });
            continue;
        }

        if let Some(caps) = TRACK_RE.captures(&line) {
            if let Some(current) = bin_files.last_mut() {
                let number: u32 = caps[1]
                    .parse()
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("Invalid track number: {e}")))?;
                let track_type = caps[2].to_string();
                current.tracks.push(Track {
                    number,
                    track_type,
                    indexes: Vec::new(),
                    sectors: None,
                });
            }
            continue;
        }

        if let Some(caps) = INDEX_RE.captures(&line) {
            if let Some(current) = bin_files.last_mut() {
                if let Some(track) = current.tracks.last_mut() {
                    let number: u32 = caps[1]
                        .parse()
                        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("Invalid index number: {e}")))?;
                    let file_offset = cuestamp_to_sectors(&caps[2])
                        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                    track.indexes.push(Index {
                        number,
                        file_offset,
                    });
                }
            }
        }
    }

    if bin_files.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "No bin files found in cue sheet",
        ));
    }

    Ok(bin_files)
}

// ──────────────────────── Sector calculation ──────────────────────────

/// For a single-bin cue (split scenario), calculate each track's length
/// in sectors by working backwards from the end of the file.
fn calc_track_sectors(files: &mut [BinFile], blocksize: u32) -> io::Result<()> {
    if files.len() != 1 {
        return Ok(());
    }
    let file = &mut files[0];
    let total_sectors = (file.size / blocksize as u64) as u32;
    let mut next_offset = total_sectors;

    for track in file.tracks.iter_mut().rev() {
        let first_index = track
            .indexes
            .first()
            .map(|i| i.file_offset)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, format!("Track {} has no INDEX entries", track.number)))?;
        track.sectors = Some(next_offset - first_index);
        next_offset = first_index;
    }
    Ok(())
}
// ──────────────────────── CUE generation ──────────────────────────────
fn gen_merged_cuesheet(basename: &str, files: &[BinFile], blocksize: u32) -> String {
    let mut out = String::new();
    out.push_str(&format!("FILE \"{basename}.bin\" BINARY\n"));
    let mut sector_pos: u32 = 0;

    for file in files {
        let file_sectors = (file.size / blocksize as u64) as u32;
        for track in &file.tracks {
            out.push_str(&format!("  TRACK {:02} {}\n", track.number, track.track_type));
            for idx in &track.indexes {
                let abs_offset = sector_pos + idx.file_offset;
                out.push_str(&format!(
                    "    INDEX {:02} {}\n",
                    idx.number,
                    sectors_to_cuestamp(abs_offset)
                ));
            }
        }
        sector_pos += file_sectors;
    }

    out.replace('\n', "\r\n")
}

/// Generate a split cue sheet: one FILE per track.
fn gen_split_cuesheet(basename: &str, file: &BinFile) -> String {
    let mut out = String::new();
    let track_count = file.tracks.len();

    for track in &file.tracks {
        let track_fn = track_filename(basename, track.number, track_count);
        out.push_str(&format!("FILE \"{track_fn}\" BINARY\n"));
        // Safety: calc_track_sectors already verified every track has at least one INDEX.
        // The unwrap_or(0) is defensive only.
        let base_offset = track.indexes.first().map(|i| i.file_offset).unwrap_or(0);

        for idx in &track.indexes {
            let rel_offset = idx.file_offset - base_offset;
            out.push_str(&format!(
                "    INDEX {:02} {}\n",
                idx.number,
                sectors_to_cuestamp(rel_offset)
            ));
        }
    }

    out.replace('\n', "\r\n")
}

/// Redump-style track filename.
fn track_filename(prefix: &str, track_num: u32, track_count: usize) -> String {
    if track_count == 1 {
        format!("{prefix}.bin")
    } else if track_count > 9 {
        format!("{prefix} (Track {track_num:02}).bin")
    } else {
        format!("{prefix} (Track {track_num}).bin")
    }
}

// ──────────────────────── Merge / Split ───────────────────────────────

/// Concatenate bin files into a single output file.
fn merge_files(merged_path: &Path, files: &[BinFile], cue_dir: &Path) -> io::Result<()> {
    if merged_path.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("Output file already exists: {}", merged_path.display()),
        ));
    }

    let mut outfile = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(merged_path)?;

    // 1 MiB heap buffer
    let mut buf = vec![0u8; BUF_SIZE];

    for bin in files {
        let bin_path = cue_dir.join(&bin.filename);
        let mut infile = File::open(&bin_path)?;
        loop {
            let n = infile.read(&mut buf)?;
            if n == 0 {
                break;
            }
            outfile.write_all(&buf[..n])?;
        }
    }

    outfile.flush()?;
    Ok(())
}

/// Split a merged bin file into per-track bin files.
fn split_files(
    merged_file: &BinFile,
    cue_dir: &Path,
    outdir: &Path,
    basename: &str,
    blocksize: u32,
) -> io::Result<()> {
    let merged_path = cue_dir.join(&merged_file.filename);
    let mut infile = File::open(&merged_path)?;
    let track_count = merged_file.tracks.len();

    // Pre-check all output paths before any writes (all-or-nothing)
    for track in &merged_file.tracks {
        let out_name = track_filename(basename, track.number, track_count);
        let out_path = outdir.join(&out_name);
        if out_path.exists() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("Output file already exists: {}", out_path.display()),
            ));
        }
    }
    let mut buf = vec![0u8; BUF_SIZE];

    for track in &merged_file.tracks {
        let first_index = track
            .indexes
            .first()
            .map(|i| i.file_offset)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, format!("Track {} has no INDEX entries", track.number)))?;
        let sectors = track
            .sectors
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, format!("Track {} has no sector length computed", track.number)))?;
        let offset_bytes = first_index as u64 * blocksize as u64;
        let length_bytes = sectors as u64 * blocksize as u64;

        let out_name = track_filename(basename, track.number, track_count);
        let out_path = outdir.join(&out_name);

        let mut outfile = File::create(&out_path)?;

        // Seek + copy exact byte range
        infile.seek(std::io::SeekFrom::Start(offset_bytes))?;
        let mut chunk = (&mut infile).take(length_bytes);
        loop {
            let n = chunk.read(&mut buf)?;
            if n == 0 {
                break;
            }
            outfile.write_all(&buf[..n])?;
        }
    }

    Ok(())
}

fn info(msg: &str) {
    println!("[INFO]\t{msg}");
}

fn fatal(msg: &str) -> ! {
    eprintln!("[ERROR]\t{msg}");
    std::process::exit(1);
}

fn debug(verbose: bool, msg: &str) {
    if verbose {
        println!("[DEBUG]\t{msg}");
    }
}

// ──────────────────────── Main ────────────────────────────────────────


fn write_cue_file(outdir: &Path, basename: &str, cuesheet: &str) -> io::Result<PathBuf> {
    let path = outdir.join(format!("{basename}.cue"));
    if path.exists() {
        fatal(&format!("Output cue file already exists: {}", path.display()));
    }
    fs::write(&path, cuesheet)?;
    Ok(path)
}
fn main() -> io::Result<()> {
    let total_start = Instant::now();
    let args = Args::parse();

    // --- Resolve paths ---
    let cue_path = Path::new(&args.cuefile)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(&args.cuefile));

    if !cue_path.is_file() {
        fatal(&format!("Cue file not found or not a regular file: {}", cue_path.display()));
    }

    let cue_dir = cue_path.parent().unwrap_or(Path::new("."));

    let outdir = match &args.outdir {
        Some(d) => {
            let p = PathBuf::from(d);
            fs::create_dir_all(&p)?;
            p
        }
        None => cue_dir.to_path_buf(),
    };

    // --- Parse CUE ---
    info(&format!("Opening cue: {}", cue_path.display()));
    let parse_start = Instant::now();
    let mut bin_files = parse_cue(&cue_path)?;
    let parse_ms = parse_start.elapsed().as_millis();
    debug(args.verbose, &format!("Parsed {} file(s) in {parse_ms}ms", bin_files.len()));

    // --- Determine blocksize ---
    let blocksize = bin_files
        .iter()
        .find_map(|f| f.tracks.first())
        .and_then(|t| blocksize_for_type(&t.track_type))
        .unwrap_or(2352); // default for AUDIO/MODE2

    debug(args.verbose, &format!("Blocksize: {blocksize} bytes/sector"));

    // --- Single-file: compute track sectors ---
    calc_track_sectors(&mut bin_files, blocksize)?;

    // --- Merge or Split ---
    if args.split {
        if bin_files.len() != 1 {
            fatal("Split mode requires a cue with exactly one FILE.");
        }

        let merged = &bin_files[0];
        let track_count = merged.tracks.len();
        info(&format!("Splitting {} tracks...", track_count));

        split_files(merged, cue_dir, &outdir, &args.basename, blocksize)?;

        let cuesheet = gen_split_cuesheet(&args.basename, merged);
        let cue_out = write_cue_file(&outdir, &args.basename, &cuesheet)?;
        info(&format!("Wrote {} tracks and cue: {}", track_count, cue_out.display()));
    } else {
        // Merge mode
        let track_count: usize = bin_files.iter().map(|f| f.tracks.len()).sum();
        info(&format!("Merging {} tracks...", track_count));

        let merged_path = outdir.join(format!("{}.bin", args.basename));
        merge_files(&merged_path, &bin_files, cue_dir)?;
        info(&format!("Wrote {}", merged_path.display()));

        let cuesheet = gen_merged_cuesheet(&args.basename, &bin_files, blocksize);
        let cue_out = write_cue_file(&outdir, &args.basename, &cuesheet)?;
        info(&format!("Wrote new cue: {}", cue_out.display()));
    }

    let total_ms = total_start.elapsed().as_millis();
    debug(args.verbose, &format!("Total time: {total_ms}ms"));

    Ok(())
}
