use crate::types::*;

/// Emit disc-level metadata (CATALOG, CDTEXTFILE, TITLE, PERFORMER, SONGWRITER).
pub(crate) fn push_disc_meta(out: &mut String, disc: &DiscMeta) {
    if let Some(cat) = &disc.catalog {
        out.push_str(&format!("CATALOG {cat}\r\n"));
    }
    if let Some(cdt) = &disc.cdtextfile {
        out.push_str(&format!("CDTEXTFILE \"{cdt}\"\r\n"));
    }
    if let Some(t) = &disc.title {
        out.push_str(&format!("TITLE \"{t}\"\r\n"));
    }
    if let Some(p) = &disc.performer {
        out.push_str(&format!("PERFORMER \"{p}\"\r\n"));
    }
    if let Some(s) = &disc.songwriter {
        out.push_str(&format!("SONGWRITER \"{s}\"\r\n"));
    }
}

/// Emit per-track metadata (FLAGS, ISRC, TITLE, PERFORMER, SONGWRITER, REMs, PREGAP).
pub(crate) fn push_track_meta(out: &mut String, track: &Track) {
    for flag in &track.flags {
        out.push_str(&format!("    FLAGS {flag}\r\n"));
    }
    if let Some(isrc) = &track.isrc {
        out.push_str(&format!("    ISRC {isrc}\r\n"));
    }
    if let Some(t) = &track.title {
        out.push_str(&format!("    TITLE \"{t}\"\r\n"));
    }
    if let Some(p) = &track.performer {
        out.push_str(&format!("    PERFORMER \"{p}\"\r\n"));
    }
    if let Some(s) = &track.songwriter {
        out.push_str(&format!("    SONGWRITER \"{s}\"\r\n"));
    }
    for rem in &track.remarks {
        out.push_str(&format!("    {rem}\r\n"));
    }
    if let Some(pg) = track.pregap {
        out.push_str(&format!("    PREGAP {pg}\r\n"));
    }
}

/// Generate a merged cue sheet: one FILE covering all tracks.
pub fn gen_merged_cuesheet(basename: &str, sheet: &CueSheet, blocksize: u32) -> String { let mut out = String::new();

// Preamble REMs (before first FILE)
for rem in &sheet.preamble {
    out.push_str(&format!("{rem}\r\n"));
}

// Disc-level metadata
push_disc_meta(&mut out, &sheet.disc);

out.push_str(&format!("FILE \"{basename}.bin\" BINARY\r\n"));
let mut sector_pos = Sectors::default();

for file in &sheet.files {
    // File-level REMs
    for rem in &file.remarks {
        out.push_str(&format!("{rem}\r\n"));
    }
    let file_sectors = Sectors::from((file.size / blocksize as u64) as u32);
    for track in &file.tracks {
        out.push_str(&format!("  TRACK {:02} {}\r\n", track.number, track.track_type));
        push_track_meta(&mut out, track);
        for idx in &track.indexes {
            let abs_offset = sector_pos + idx.file_offset;
            out.push_str(&format!(
                "    INDEX {:02} {abs_offset}\r\n",
                idx.number,
            ));
        }
        if let Some(pg) = track.postgap {
            out.push_str(&format!("    POSTGAP {pg}\r\n"));
        }
    }
    sector_pos = sector_pos + file_sectors;
}

out }

/// Generate a split cue sheet: one FILE per track.
pub fn gen_split_cuesheet(basename: &str, file: &BinFile, sheet: &CueSheet) -> String { let mut out = String::new();
let track_count = file.tracks.len();

// Preamble REMs (before first FILE)
for rem in &sheet.preamble {
    out.push_str(&format!("{rem}\r\n"));
}

// Disc-level metadata
push_disc_meta(&mut out, &sheet.disc);

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
    let base_offset = track.indexes.first().map(|i| i.file_offset).unwrap_or(Sectors(0));
    for idx in &track.indexes {
        let rel_offset = idx.file_offset - base_offset;
        out.push_str(&format!(
            "    INDEX {:02} {rel_offset}\r\n",
            idx.number,
        ));
    }
    if let Some(pg) = track.postgap {
        out.push_str(&format!("    POSTGAP {pg}\r\n"));
    }
}

out }
