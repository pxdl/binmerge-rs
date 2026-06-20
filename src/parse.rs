use std::fs::{self, File};
use std::io::{self, BufRead};
use std::path::Path;

use crate::types::*;

/// Parse a .cue file into a list of BinFile structs.
///
/// Each FILE directive starts a new bin; TRACK and INDEX directives
/// are associated with the most recent FILE.
pub fn parse_cue(cue_path: &Path, disc: &mut DiscMeta, preamble: &mut Vec<String>) -> io::Result<Vec<BinFile>> { let cue_dir = cue_path.parent().unwrap_or(Path::new("."));
let file = File::open(cue_path)?;
let mut reader = io::BufReader::new(file);
parse_cue_from_reader(&mut reader, cue_dir, disc, preamble) }

/// Parse CUE content from a buffered reader.
pub fn parse_cue_from_reader(reader: &mut dyn BufRead, cue_dir: &Path, disc: &mut DiscMeta, preamble: &mut Vec<String>) -> io::Result<Vec<BinFile>> { let mut bin_files: Vec<BinFile> = Vec::new();

for line in reader.lines() {
    let line_owned = line?;
    let line = line_owned.trim();

    if let Some(caps) = FILE_RE.captures(line) {
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

    if let Some(caps) = TRACK_RE.captures(line) {
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
    if let Some(caps) = INDEX_RE.captures(line) {
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
    if let Some(caps) = PREGAP_RE.captures(line) {
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

    if let Some(caps) = POSTGAP_RE.captures(line) {
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

    if let Some(caps) = FLAGS_RE.captures(line) {
        let Some(current) = bin_files.last_mut() else {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "FLAGS directive before any FILE"));
        };
        let Some(track) = current.tracks.last_mut() else {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "FLAGS directive before any TRACK"));
        };
        track.flags.push(caps[1].to_string());
        continue;
    }

    if let Some(caps) = ISRC_RE.captures(line) {
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
    if let Some(caps) = TITLE_RE.captures(line) {
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

    if let Some(caps) = PERFORMER_RE.captures(line) {
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

    if let Some(caps) = SONGWRITER_RE.captures(line) {
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
    if let Some(caps) = CATALOG_RE.captures(line) {
        disc.catalog = Some(caps[1].to_string());
        continue;
    }

    // CDTEXTFILE — disc-level only
    if let Some(caps) = CDTEXTFILE_RE.captures(line) {
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
        continue;
    }

    // Capture unknown directives for lossless roundtrip.
    if !line.is_empty() {
        disc.unknown.push(line_owned);
    }
}

if bin_files.is_empty() {
    return Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "CUE file has no FILE directives",
    ));
}

Ok(bin_files) }

/// For a single-bin cue (split scenario), calculate each track's length
/// in sectors by working backwards from the end of the file.
pub fn calc_track_sectors(files: &mut [BinFile], blocksize: u32) -> io::Result<()> { if files.len() != 1 {
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
let total_sectors = Sectors::from((file.size / bs) as u32);
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
Ok(()) }
