//! binmerge-rs — Parse, merge, and split multi-bin CUE sheets.
//!
//! This crate provides a library for working with CUE sheets and their
//! associated binary (`.bin`) files. The primary operations are:
//!
//! - **Parse** a CUE sheet into a structured [`CueSheet`]
//! - **Merge** multi-file BIN dumps into a single `.bin` + `.cue` pair
//! - **Split** a merged `.bin` back into per-track `.bin` files with a new CUE
//!
//! # Example
//!
//! ```no_run
//! use binmerge_rs::{parse_cue, CueSheet, merge_files, gen_merged_cuesheet, write_cue_file};
//!
//! let mut sheet = CueSheet::default();
//! sheet.files = parse_cue("game.cue".as_ref(), &mut sheet.disc, &mut sheet.preamble)?;
//!
//! let blocksize = binmerge_rs::blocksize_for_type(
//!     &sheet.files[0].tracks[0].track_type
//! ).unwrap_or(2352);
//!
//! merge_files("game-merged.bin".as_ref(), &sheet.files, ".".as_ref(), false, false)?;
//!
//! let cuesheet = gen_merged_cuesheet("game-merged", &sheet, blocksize);
//! write_cue_file(".".as_ref(), "game-merged", &cuesheet, false)?;
//! # Ok::<(), std::io::Error>(())
//! ```

pub mod types;
pub mod parse;
pub mod gen;
pub mod ops;

// Re-export the most commonly used items at the crate root.
pub use types::{
    blocksize_for_type, cue_basename, cuestamp_to_sectors, track_filename, BinFile, CueSheet,
    DiscMeta, Index, Sectors, Track,
};
pub use parse::{calc_track_sectors, parse_cue, parse_cue_from_reader};
pub use gen::{gen_merged_cuesheet, gen_split_cuesheet};
pub use ops::{merge_files, split_files, write_cue_file};
