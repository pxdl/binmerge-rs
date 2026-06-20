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
static REM_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[\t ]*REM\b").unwrap());
static PREGAP_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[\t ]*PREGAP (\d+:\d+:\d+)").unwrap());
static POSTGAP_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[\t ]*POSTGAP (\d+:\d+:\d+)").unwrap());
static CATALOG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[\t ]*CATALOG (\S+)").unwrap());
static FLAGS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[\t ]*FLAGS (.+)").unwrap());
static ISRC_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[\t ]*ISRC (\S+)").unwrap());
static TITLE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^[\t ]*TITLE "?(.*?)"?$"#).unwrap());
static PERFORMER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^[\t ]*PERFORMER "?(.*?)"?$"#).unwrap());
static SONGWRITER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^[\t ]*SONGWRITER "?(.*?)"?$"#).unwrap());
static CDTEXTFILE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^[\t ]*CDTEXTFILE "?(.*?)"?$"#).unwrap());

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
    /// PREGAP duration in sectors.
    pregap: Option<u32>,
    /// POSTGAP duration in sectors.
    postgap: Option<u32>,
    /// REM comment lines associated with this track.
    remarks: Vec<String>,
    /// FLAGS: e.g. "DCP", "4CH", "PRE", "SCMS"
    flags: Vec<String>,
    /// ISRC code
    isrc: Option<String>,
    /// CD-TEXT: track title
    title: Option<String>,
    /// CD-TEXT: track performer
    performer: Option<String>,
    /// CD-TEXT: track songwriter
    songwriter: Option<String>,
}
struct BinFile {
    filename: String,
    tracks: Vec<Track>,
    /// File size in bytes.
    size: u64,
    /// REM comment lines associated with this file (between FILE and first TRACK).
    remarks: Vec<String>,
}
/// Disc-level metadata (before any FILE directive).
#[derive(Default)]
struct DiscMeta {
    catalog: Option<String>,
    title: Option<String>,
    performer: Option<String>,
    songwriter: Option<String>,
    cdtextfile: Option<String>,
}

// ──────────────────────── CLI ─────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "binmerge-rs",
    about = "Merge multi-bin CUE sheets into a single bin/cue pair (or split them back)."
)]
struct Args {
    /// Path to the .cue file (or directory with --batch)
    cuefile: String,

    /// Base name (without extension) for the output files (default: derived from cue filename)
    basename: Option<String>,

    /// Reverse: split a merged bin back into per-track bins
    #[arg(short = 's', long = "split")]
    split: bool,

    /// Output directory (default: same dir as cue file)
    #[arg(short = 'o', long = "outdir")]
    outdir: Option<String>,

    /// Verbose output
    #[arg(short = 'v', long = "verbose")]
    verbose: bool,

    /// Show progress bar during merge/split
    #[arg(short = 'p', long = "progress")]
    progress: bool,

    /// Treat cuefile as a directory; process all .cue files found
    #[arg(short = 'b', long = "batch")]
    batch: bool,

    /// Parse and validate without writing any output files
    #[arg(long = "dry-run")]
    dry_run: bool,

    /// Allow overwriting existing output files
    #[arg(long = "overwrite")]
    overwrite: bool,
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
fn parse_cue(cue_path: &Path, disc: &mut DiscMeta, preamble: &mut Vec<String>) -> io::Result<Vec<BinFile>> {
    let cue_dir = cue_path.parent().unwrap_or(Path::new("."));
    let file = File::open(cue_path)?;
    let reader = io::BufReader::new(file);

    let mut bin_files: Vec<BinFile> = Vec::new();

    for line in reader.lines() {
        let line_owned = line?;
        let line = line_owned.trim();

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
                remarks: Vec::new(),
            });
            continue;
        }

        if let Some(caps) = TRACK_RE.captures(&line) {
            let Some(current) = bin_files.last_mut() else {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "TRACK directive before any FILE"));
            };
            let number: u32 = caps[1]
                .parse()
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("Invalid track number: {e}")))?;
            let track_type = caps[2].to_string();
            current.tracks.push(Track {
                number,
                track_type,
                indexes: Vec::new(),
                sectors: None,
                pregap: None,
                postgap: None,
                remarks: Vec::new(),
                flags: Vec::new(),
                isrc: None,
                title: None,
                performer: None,
                songwriter: None,
            });
            continue;
        }
        if let Some(caps) = INDEX_RE.captures(&line) {
            let Some(current) = bin_files.last_mut() else {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "INDEX directive before any FILE"));
            };
            let Some(track) = current.tracks.last_mut() else {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "INDEX directive before any TRACK"));
            };
            let number: u32 = caps[1]
                .parse()
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("Invalid index number: {e}")))?;
            let file_offset = cuestamp_to_sectors(&caps[2])
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            track.indexes.push(Index {
                number,
                file_offset,
            });
            continue;
        }
        // PREGAP / POSTGAP on the most recent track
        if let Some(caps) = PREGAP_RE.captures(&line) {
            let Some(current) = bin_files.last_mut() else {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "PREGAP directive before any FILE"));
            };
            let Some(track) = current.tracks.last_mut() else {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "PREGAP directive before any TRACK"));
            };
            track.pregap = Some(cuestamp_to_sectors(&caps[1])
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?);
            continue;
        }

        if let Some(caps) = POSTGAP_RE.captures(&line) {
            let Some(current) = bin_files.last_mut() else {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "POSTGAP directive before any FILE"));
            };
            let Some(track) = current.tracks.last_mut() else {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "POSTGAP directive before any TRACK"));
            };
            track.postgap = Some(cuestamp_to_sectors(&caps[1])
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?);
            continue;
        }

        if let Some(caps) = FLAGS_RE.captures(&line) {
            let Some(current) = bin_files.last_mut() else {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "FLAGS directive before any FILE"));
            };
            let Some(track) = current.tracks.last_mut() else {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "FLAGS directive before any TRACK"));
            };
            track.flags.push(caps[1].to_string());
            continue;
        }

        if let Some(caps) = ISRC_RE.captures(&line) {
            let Some(current) = bin_files.last_mut() else {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "ISRC directive before any FILE"));
            };
            let Some(track) = current.tracks.last_mut() else {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "ISRC directive before any TRACK"));
            };
            track.isrc = Some(caps[1].to_string());
            continue;
        }

        // TITLE / PERFORMER / SONGWRITER — disc-level if no FILE yet, else track-level
        if let Some(caps) = TITLE_RE.captures(&line) {
            let val = caps[1].to_string();
            if bin_files.is_empty() {
                disc.title = Some(val);
            } else if let Some(current) = bin_files.last_mut() {
                if let Some(track) = current.tracks.last_mut() {
                    track.title = Some(val);
                }
            }
            continue;
        }

        if let Some(caps) = PERFORMER_RE.captures(&line) {
            let val = caps[1].to_string();
            if bin_files.is_empty() {
                disc.performer = Some(val);
            } else if let Some(current) = bin_files.last_mut() {
                if let Some(track) = current.tracks.last_mut() {
                    track.performer = Some(val);
                }
            }
            continue;
        }

        if let Some(caps) = SONGWRITER_RE.captures(&line) {
            let val = caps[1].to_string();
            if bin_files.is_empty() {
                disc.songwriter = Some(val);
            } else if let Some(current) = bin_files.last_mut() {
                if let Some(track) = current.tracks.last_mut() {
                    track.songwriter = Some(val);
                }
            }
            continue;
        }

        // CATALOG — disc-level only
        if let Some(caps) = CATALOG_RE.captures(&line) {
            disc.catalog = Some(caps[1].to_string());
            continue;
        }

        // CDTEXTFILE — disc-level only
        if let Some(caps) = CDTEXTFILE_RE.captures(&line) {
            disc.cdtextfile = Some(caps[1].to_string());
            continue;
        }

        // REM lines: store on the most specific entity we have.
        // Before any FILE → preamble; after FILE, before TRACK → file; after TRACK → track.
        if REM_RE.is_match(line) {
            if bin_files.is_empty() {
                preamble.push(line.to_string());
            } else if let Some(current) = bin_files.last_mut() {
                if current.tracks.is_empty() {
                    current.remarks.push(line.to_string());
                } else if let Some(track) = current.tracks.last_mut() {
                    track.remarks.push(line.to_string());
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
    let bs = blocksize as u64;
    if file.size % bs != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "File size {} is not a multiple of blocksize {} (remainder {})",
                file.size,
                blocksize,
                file.size % bs,
            ),
        ));
    }
    let total_sectors = (file.size / bs) as u32;
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

/// Emit disc-level metadata (CATALOG, CDTEXTFILE, TITLE, PERFORMER, SONGWRITER).
fn push_disc_meta(out: &mut String, disc: &DiscMeta) {
    if let Some(ref cat) = disc.catalog {
        out.push_str(&format!("CATALOG {cat}\r\n"));
    }
    if let Some(ref cdt) = disc.cdtextfile {
        out.push_str(&format!("CDTEXTFILE \"{cdt}\"\r\n"));
    }
    if let Some(ref t) = disc.title {
        out.push_str(&format!("TITLE \"{t}\"\r\n"));
    }
    if let Some(ref p) = disc.performer {
        out.push_str(&format!("PERFORMER \"{p}\"\r\n"));
    }
    if let Some(ref s) = disc.songwriter {
        out.push_str(&format!("SONGWRITER \"{s}\"\r\n"));
    }
}

/// Emit per-track metadata (FLAGS, ISRC, TITLE, PERFORMER, SONGWRITER, REMs, PREGAP).
fn push_track_meta(out: &mut String, track: &Track) {
    for flag in &track.flags {
        out.push_str(&format!("    FLAGS {flag}\r\n"));
    }
    if let Some(ref isrc) = track.isrc {
        out.push_str(&format!("    ISRC {isrc}\r\n"));
    }
    if let Some(ref t) = track.title {
        out.push_str(&format!("    TITLE \"{t}\"\r\n"));
    }
    if let Some(ref p) = track.performer {
        out.push_str(&format!("    PERFORMER \"{p}\"\r\n"));
    }
    if let Some(ref s) = track.songwriter {
        out.push_str(&format!("    SONGWRITER \"{s}\"\r\n"));
    }
    for rem in &track.remarks {
        out.push_str(&format!("    {rem}\r\n"));
    }
    if let Some(pg) = track.pregap {
        out.push_str(&format!("    PREGAP {}\r\n", sectors_to_cuestamp(pg)));
    }
}
fn gen_merged_cuesheet(basename: &str, files: &[BinFile], disc: &DiscMeta, preamble: &[String], blocksize: u32) -> String {
    let mut out = String::new();

    // Preamble REMs (before first FILE)
    for rem in preamble {
        out.push_str(&format!("{rem}\r\n"));
    }

    // Disc-level metadata
    push_disc_meta(&mut out, disc);

    out.push_str(&format!("FILE \"{basename}.bin\" BINARY\r\n"));
    let mut sector_pos: u32 = 0;

    for file in files {
        // File-level REMs
        for rem in &file.remarks {
            out.push_str(&format!("{rem}\r\n"));
        }
        let file_sectors = (file.size / blocksize as u64) as u32;
        for track in &file.tracks {
            out.push_str(&format!("  TRACK {:02} {}\r\n", track.number, track.track_type));
            push_track_meta(&mut out, track);
            for idx in &track.indexes {
                let abs_offset = sector_pos + idx.file_offset;
                out.push_str(&format!(
                    "    INDEX {:02} {}\r\n",
                    idx.number,
                    sectors_to_cuestamp(abs_offset)
                ));
            }
            if let Some(pg) = track.postgap {
                out.push_str(&format!("    POSTGAP {}\r\n", sectors_to_cuestamp(pg)));
            }
        }
        sector_pos += file_sectors;
    }

    out

}

/// Generate a split cue sheet: one FILE per track.
fn gen_split_cuesheet(basename: &str, file: &BinFile, disc: &DiscMeta, preamble: &[String]) -> String {
    let mut out = String::new();
    let track_count = file.tracks.len();

    // Preamble REMs (before first FILE)
    for rem in preamble {
        out.push_str(&format!("{rem}\r\n"));
    }

    // Disc-level metadata
    push_disc_meta(&mut out, disc);

    // File-level REMs (originally between FILE and first TRACK in merged CUE)
    for rem in &file.remarks {
        out.push_str(&format!("{rem}\r\n"));
    }

    for track in &file.tracks {
        let track_fn = track_filename(basename, track.number, track_count);
        out.push_str(&format!("FILE \"{track_fn}\" BINARY\r\n"));
        out.push_str(&format!("  TRACK {:02} {}\r\n", track.number, track.track_type));
        push_track_meta(&mut out, track);
        // INDEX entries — relative to first index
        let base_offset = track.indexes.first().map(|i| i.file_offset).unwrap_or(0);
        for idx in &track.indexes {
            let rel_offset = idx.file_offset - base_offset;
            out.push_str(&format!(
                "    INDEX {:02} {}\r\n",
                idx.number,
                sectors_to_cuestamp(rel_offset)
            ));
        }
        if let Some(pg) = track.postgap {
            out.push_str(&format!("    POSTGAP {}\r\n", sectors_to_cuestamp(pg)));
        }
    }

    out

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
fn merge_files(merged_path: &Path, files: &[BinFile], cue_dir: &Path, progress: bool, overwrite: bool) -> io::Result<()> {
    let mut open_opts = OpenOptions::new();
    open_opts.write(true);
    if overwrite {
        open_opts.create(true).truncate(true);
    } else {
        open_opts.create_new(true);
    }
    let mut outfile = open_opts.open(merged_path).map_err(|e| io::Error::new(
        e.kind(),
        format!("Cannot create output file {}: {e}", merged_path.display()),
    ))?;
    let total: u64 = files.iter().map(|f| f.size).sum();
    let mut done: u64 = 0;
    let mut last_pct: u64 = 0;

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
            done += n as u64;
            if progress {
                let pct = if total > 0 { (done * 100) / total } else { 100 };
                if pct != last_pct {
                    eprint!("\r[Merging] {pct}%");
                    last_pct = pct;
                }
            }
        }
    }

    if progress {
        eprintln!("\r[Merging] 100%");
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
    progress: bool,
    overwrite: bool,
) -> io::Result<()> {
    let merged_path = cue_dir.join(&merged_file.filename);
    let mut infile = File::open(&merged_path)?;
    let track_count = merged_file.tracks.len();

    // Verify all tracks have computed sectors (before progress bar or any writes)
    for track in &merged_file.tracks {
        if track.sectors.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Track {} has no sector length computed", track.number),
            ));
        }
        if track.indexes.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Track {} has no INDEX entries", track.number),
            ));
        }
    }

    let total: u64 = merged_file.tracks.iter()
        .map(|t| t.sectors.unwrap() as u64 * blocksize as u64)
        .sum();
    let mut done: u64 = 0;
    let mut last_pct: u64 = 0;

    let mut buf = vec![0u8; BUF_SIZE];
    for track in &merged_file.tracks {
        let first_index = track.indexes.first().unwrap().file_offset;
        let sectors = track.sectors.unwrap();
        let offset_bytes = first_index as u64 * blocksize as u64;
        let length_bytes = sectors as u64 * blocksize as u64;

        let out_name = track_filename(basename, track.number, track_count);
        let out_path = outdir.join(&out_name);

        let mut open_opts = OpenOptions::new();
        open_opts.write(true);
        if overwrite {
            open_opts.create(true).truncate(true);
        } else {
            open_opts.create_new(true);
        }
        let mut outfile = open_opts.open(&out_path).map_err(|e| io::Error::new(
            e.kind(),
            format!("Cannot create output file {}: {e}", out_path.display()),
        ))?;

        // Seek + copy exact byte range
        infile.seek(std::io::SeekFrom::Start(offset_bytes))?;
        let mut chunk = (&mut infile).take(length_bytes);
        loop {
            let n = chunk.read(&mut buf)?;
            if n == 0 {
                break;
            }
            outfile.write_all(&buf[..n])?;
            done += n as u64;
            if progress {
                let pct = if total > 0 { (done * 100) / total } else { 100 };
                if pct != last_pct {
                    eprint!("\r[Splitting] {pct}%");
                    last_pct = pct;
                }
            }
        }
    }

    if progress {
        eprintln!("\r[Splitting] 100%");
    }
    Ok(())
}

fn info(msg: &str) {
    println!("[INFO]\t{msg}");
}

macro_rules! fatal {
    ($($arg:tt)*) => {{
        eprintln!("[ERROR]\t{}", format!($($arg)*));
        std::process::exit(1);
    }};
}

macro_rules! debug {
    ($verbose:expr, $($arg:tt)*) => {
        if $verbose {
            println!("[DEBUG]\t{}", format!($($arg)*));
        }
    };
}

// ──────────────────────── Main ────────────────────────────────────────


fn write_cue_file(outdir: &Path, basename: &str, cuesheet: &str, overwrite: bool) -> io::Result<PathBuf> {
    let path = outdir.join(format!("{basename}.cue"));
    let mut open_opts = OpenOptions::new();
    open_opts.write(true);
    if overwrite {
        open_opts.create(true).truncate(true);
    } else {
        open_opts.create_new(true);
    }
    let mut f = open_opts.open(&path).map_err(|e| io::Error::new(
        e.kind(),
        format!("Cannot create cue file {}: {e}", path.display()),
    ))?;
    f.write_all(cuesheet.as_bytes())?;
    Ok(path)
}
fn cue_basename(cue_path: &Path) -> String {
    cue_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("output")
        .to_string()
}

fn process_cue(
    cue_path: &Path,
    basename: &str,
    args: &Args,
    total_start: Instant,
) -> io::Result<()> {
    if !cue_path.is_file() {
        fatal!("Cue file not found: {}", cue_path.display());
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
    let mut disc = DiscMeta::default();
    let mut preamble: Vec<String> = Vec::new();
    let mut bin_files = parse_cue(cue_path, &mut disc, &mut preamble)?;
    let parse_ms = parse_start.elapsed().as_millis();
    debug!(args.verbose, "Parsed {} file(s) in {parse_ms}ms", bin_files.len());

    // --- Determine blocksize ---
    let blocksize = bin_files
        .iter()
        .find_map(|f| f.tracks.first())
        .and_then(|t| blocksize_for_type(&t.track_type))
        .unwrap_or(2352);
    debug!(args.verbose, "Blocksize: {blocksize} bytes/sector");
    // --- Single-file: compute track sectors ---
    calc_track_sectors(&mut bin_files, blocksize)?;

    let track_count: usize = bin_files.iter().map(|f| f.tracks.len()).sum();
    let total_bytes: u64 = bin_files.iter().map(|f| f.size).sum();

    // --- Dry-run: print summary and exit ---
    if args.dry_run {
        info(&format!(
            "DRY-RUN: {} file(s), {} track(s), {:.2} MiB total, blocksize {}",
            bin_files.len(),
            track_count,
            total_bytes as f64 / (1024.0 * 1024.0),
            blocksize,
        ));
        if let Some(ref cat) = disc.catalog {
            info(&format!("  CATALOG: {cat}"));
        }
        if let Some(ref cdt) = disc.cdtextfile {
            info(&format!("  CDTEXTFILE: {cdt}"));
        }
        if let Some(ref t) = disc.title {
            info(&format!("  TITLE: {t}"));
        }
        for f in &bin_files {
            for t in &f.tracks {
                let idx_count = t.indexes.len();
                let fl = if t.flags.is_empty() { String::new() } else { format!(" [{}]", t.flags.join(", ")) };
                let isrc = t.isrc.as_deref().unwrap_or("");
                info(&format!(
                    "  Track {:02} {}{}  indexes={}  isrc={}",
                    t.number, t.track_type, fl, idx_count, isrc,
                ));
            }
        }
        return Ok(());
    }

    // --- Merge or Split ---
    if args.split {
        if bin_files.len() != 1 {
            fatal!("Split mode requires a cue with exactly one FILE.");
        }

        let merged = &bin_files[0];
        info(&format!("Splitting {} tracks...", track_count));

        split_files(merged, cue_dir, &outdir, basename, blocksize, args.progress, args.overwrite)?;

        let cuesheet = gen_split_cuesheet(basename, merged, &disc, &preamble);
        let cue_out = write_cue_file(&outdir, basename, &cuesheet, args.overwrite)?;
        info(&format!("Wrote {} tracks and cue: {}", track_count, cue_out.display()));
    } else {
        info(&format!("Merging {} tracks...", track_count));

        let merged_path = outdir.join(format!("{basename}.bin"));
        merge_files(&merged_path, &bin_files, cue_dir, args.progress, args.overwrite)?;
        info(&format!("Wrote {}", merged_path.display()));

        let cuesheet = gen_merged_cuesheet(basename, &bin_files, &disc, &preamble, blocksize);
        let cue_out = write_cue_file(&outdir, basename, &cuesheet, args.overwrite)?;
        info(&format!("Wrote new cue: {}", cue_out.display()));
    }
    let total_ms = total_start.elapsed().as_millis();
    debug!(args.verbose, "Total time: {total_ms}ms");

    Ok(())
}

fn main() -> io::Result<()> {
    let total_start = Instant::now();
    let args = Args::parse();

    if args.batch {
        let dir = Path::new(&args.cuefile);
        if !dir.is_dir() {
            fatal!("--batch requires a directory: {}", dir.display());
        }
        if args.basename.is_some() {
            fatal!("--batch does not accept a basename argument (names are auto-derived)");
        }

        // Collect .cue files
        let mut cue_files: Vec<PathBuf> = Vec::new();
        match fs::read_dir(dir) {
            Ok(entries) => {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.extension().map_or(false, |e| e.eq_ignore_ascii_case("cue")) {
                        cue_files.push(p);
                    }
                }
            }
            Err(e) => fatal!("Cannot read directory {}: {e}", dir.display()),
        }

        if cue_files.is_empty() {
            fatal!("No .cue files found in {}", dir.display());
        }

        cue_files.sort();
        info(&format!("Found {} .cue file(s) in {}", cue_files.len(), dir.display()));

        let mut errors = 0;
        for cue_path in &cue_files {
            let basename = cue_basename(cue_path);
            match process_cue(cue_path, &basename, &args, total_start) {
                Ok(()) => {}
                Err(e) => {
                    eprintln!("[ERROR]\t{}: {e}", cue_path.display());
                    errors += 1;
                }
            }
        }

        if errors > 0 {
            eprintln!("[ERROR]\t{errors} file(s) failed");
            std::process::exit(1);
        }
    } else {
        let cue_path = Path::new(&args.cuefile)
            .canonicalize()
            .unwrap_or_else(|_| PathBuf::from(&args.cuefile));

        let basename = match &args.basename { Some(b) => b.clone(), None => cue_basename(&cue_path) };
        process_cue(&cue_path, &basename, &args, total_start)?;
    }

    Ok(())
}
