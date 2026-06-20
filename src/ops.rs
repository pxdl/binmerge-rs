use std::fs::File;
use std::io::{self, Read, Seek, Write};
use std::path::{Path, PathBuf};

use crate::types::*;

/// Concatenate bin files into a single output file.
pub fn merge_files(merged_path: &Path, files: &[BinFile], cue_dir: &Path, progress: bool, overwrite: bool) -> io::Result<()> { let mut open_opts = std::fs::OpenOptions::new();
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
Ok(()) }

/// Split a merged bin file into per-track bin files.
pub fn split_files(merged_file: &BinFile,
cue_dir: &Path,
outdir: &Path,
basename: &str,
blocksize: u32,
progress: bool,
overwrite: bool,) -> io::Result<()> { let merged_path = cue_dir.join(&merged_file.filename);
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
    .map(|t| t.sectors.unwrap().to_bytes(blocksize))
    .sum();
let mut done: u64 = 0;
let mut last_pct: u64 = 0;

let mut buf = vec![0u8; BUF_SIZE];
for track in &merged_file.tracks {
    let first_index = track.indexes.first().unwrap().file_offset;
    let sectors = track.sectors.unwrap();
    let offset_bytes = first_index.to_bytes(blocksize);
    let length_bytes = sectors.to_bytes(blocksize);

    let out_name = track_filename(basename, track.number, track_count);
    let out_path = outdir.join(&out_name);

    let mut open_opts = std::fs::OpenOptions::new();
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
Ok(()) }

pub fn write_cue_file(outdir: &Path, basename: &str, cuesheet: &str, overwrite: bool) -> io::Result<PathBuf> { let path = outdir.join(format!("{basename}.cue"));
let mut open_opts = std::fs::OpenOptions::new();
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
Ok(path) }
