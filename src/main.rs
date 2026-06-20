use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

use clap::Parser;

use binmerge_rs::*;

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

// ──────────────────────── Logging ─────────────────────────────────────

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

// ──────────────────────── Orchestration ───────────────────────────────

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
    let mut sheet = CueSheet::default();
    sheet.files = parse_cue(cue_path, &mut sheet.disc, &mut sheet.preamble)?;
    let parse_ms = parse_start.elapsed().as_millis();
    debug!(args.verbose, "Parsed {} file(s) in {parse_ms}ms", sheet.files.len());

    // --- Determine blocksize ---
    let blocksize = sheet
        .files
        .iter()
        .find_map(|f| f.tracks.first())
        .and_then(|t| blocksize_for_type(&t.track_type))
        .unwrap_or(2352);
    debug!(args.verbose, "Blocksize: {blocksize} bytes/sector");
    // --- Single-file: compute track sectors ---
    calc_track_sectors(&mut sheet.files, blocksize)?;

    let track_count: usize = sheet.files.iter().map(|f| f.tracks.len()).sum();
    let total_bytes: u64 = sheet.files.iter().map(|f| f.size).sum();

    // --- Dry-run: print summary and exit ---
    if args.dry_run {
        info(&format!(
            "DRY-RUN: {} file(s), {} track(s), {:.2} MiB total, blocksize {}",
            sheet.files.len(),
            track_count,
            total_bytes as f64 / (1024.0 * 1024.0),
            blocksize,
        ));
        if let Some(cat) = &sheet.disc.catalog {
            info(&format!("  CATALOG: {cat}"));
        }
        if let Some(cdt) = &sheet.disc.cdtextfile {
            info(&format!("  CDTEXTFILE: {cdt}"));
        }
        if let Some(t) = &sheet.disc.title {
            info(&format!("  TITLE: {t}"));
        }
        for f in &sheet.files {
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
        if sheet.files.len() != 1 {
            fatal!("Split mode requires a cue with exactly one FILE.");
        }

        let merged = &sheet.files[0];
        info(&format!("Splitting {} tracks...", track_count));

        split_files(merged, cue_dir, &outdir, basename, blocksize, args.progress, args.overwrite)?;

        let cuesheet = gen_split_cuesheet(basename, merged, &sheet);
        let cue_out = write_cue_file(&outdir, basename, &cuesheet, args.overwrite)?;
        info(&format!("Wrote {} tracks and cue: {}", track_count, cue_out.display()));
    } else {
        info(&format!("Merging {} tracks...", track_count));

        let merged_path = outdir.join(format!("{basename}.bin"));
        merge_files(&merged_path, &sheet.files, cue_dir, args.progress, args.overwrite)?;
        info(&format!("Wrote {}", merged_path.display()));

        let cuesheet = gen_merged_cuesheet(basename, &sheet, blocksize);
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
