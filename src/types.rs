use std::fmt;
use std::ops::{Add, Sub};
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;

// ─────────────────────────── Regex patterns ───────────────────────────

pub(crate) static FILE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"FILE "?(.*?)"? BINARY"#).unwrap());
pub(crate) static TRACK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"TRACK (\d+) ([^\s]*)").unwrap());
pub(crate) static INDEX_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"INDEX (\d+) (\d+:\d+:\d+)").unwrap());
pub(crate) static CUESTAMP_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\d+):(\d+):(\d+)").unwrap());
pub(crate) static REM_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[\t ]*REM\b").unwrap());
pub(crate) static PREGAP_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[\t ]*PREGAP (\d+:\d+:\d+)").unwrap());
pub(crate) static POSTGAP_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[\t ]*POSTGAP (\d+:\d+:\d+)").unwrap());
pub(crate) static CATALOG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[\t ]*CATALOG (\S+)").unwrap());
pub(crate) static FLAGS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[\t ]*FLAGS (.+)").unwrap());
pub(crate) static ISRC_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[\t ]*ISRC (\S+)").unwrap());
pub(crate) static TITLE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^[\t ]*TITLE "?(.*?)"?$"#).unwrap());
pub(crate) static PERFORMER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^[\t ]*PERFORMER "?(.*?)"?$"#).unwrap());
pub(crate) static SONGWRITER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^[\t ]*SONGWRITER "?(.*?)"?$"#).unwrap());
pub(crate) static CDTEXTFILE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^[\t ]*CDTEXTFILE "?(.*?)"?$"#).unwrap());

pub(crate) const BUF_SIZE: usize = 1024 * 1024; // 1 MiB

// ─────────────────────────── Sectors newtype ──────────────────────────

/// CD sector count (1 sector = 1/75 second of audio, byte size depends on track mode).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Sectors(pub u32);

impl Sectors {
    /// Convert to byte offset given a track-mode blocksize.
    pub fn to_bytes(self, blocksize: u32) -> u64 {
        self.0 as u64 * blocksize as u64
    }
}

impl From<u32> for Sectors {
    fn from(n: u32) -> Self {
        Sectors(n)
    }
}

impl Add for Sectors {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        Sectors(self.0 + rhs.0)
    }
}

impl Add<u32> for Sectors {
    type Output = Self;
    fn add(self, rhs: u32) -> Self {
        Sectors(self.0 + rhs)
    }
}

impl Sub for Sectors {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        Sectors(self.0 - rhs.0)
    }
}

impl Sub<u32> for Sectors {
    type Output = Self;
    fn sub(self, rhs: u32) -> Self {
        Sectors(self.0 - rhs)
    }
}

impl fmt::Display for Sectors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let minutes = self.0 / 4500;
        let remainder = self.0 % 4500;
        let seconds = remainder / 75;
        let frames = remainder % 75;
        write!(f, "{minutes:02}:{seconds:02}:{frames:02}")
    }
}

// ─────────────────────────── Data structures ──────────────────────────

pub struct Index { pub number: u32,
/// Sector offset from the start of the bin file containing this index.
pub file_offset: Sectors, }
pub struct Track { pub number: u32,
pub track_type: String,
pub indexes: Vec<Index>,
/// Track length in sectors (populated for single-file / split mode).
pub sectors: Option<Sectors>,
/// PREGAP duration in sectors.
pub pregap: Option<Sectors>,
/// POSTGAP duration in sectors.
pub postgap: Option<Sectors>,
/// REM comment lines associated with this track.
pub remarks: Vec<String>,
/// FLAGS: e.g. "DCP", "4CH", "PRE", "SCMS"
pub flags: Vec<String>,
/// ISRC code
pub isrc: Option<String>,
/// CD-TEXT: track title
pub title: Option<String>,
/// CD-TEXT: track performer
pub performer: Option<String>,
/// CD-TEXT: track songwriter
pub songwriter: Option<String>, }
pub struct BinFile { pub filename: String,
pub tracks: Vec<Track>,
/// File size in bytes.
pub size: u64,
/// REM comment lines associated with this file (between FILE and first TRACK).
pub remarks: Vec<String>, }
/// Disc-level metadata (before any FILE directive).
#[derive(Default)]
pub struct DiscMeta { pub catalog: Option<String>,
pub title: Option<String>,
pub performer: Option<String>,
pub songwriter: Option<String>,
pub cdtextfile: Option<String>,
/// Unparsed / unknown directives (preserved for lossless roundtrip).
pub unknown: Vec<String>, }

/// Parsed CUE sheet — all data needed for merge/split operations.
#[derive(Default)]
pub struct CueSheet { pub files: Vec<BinFile>,
pub disc: DiscMeta,
pub preamble: Vec<String>, }

// ──────────────────────── Timestamp helpers ──────────────────────────

/// Convert "MM:SS:FF" → sectors (75 frames/s, 60s/min).
pub fn cuestamp_to_sectors(stamp: &str) -> Result<Sectors, &'static str> { let caps = CUESTAMP_RE.captures(stamp).ok_or("Invalid timestamp format")?;
let m: u32 = caps[1].parse().map_err(|_| "bad minutes")?;
let s: u32 = caps[2].parse().map_err(|_| "bad seconds")?;
let f: u32 = caps[3].parse().map_err(|_| "bad frames")?;
Ok(Sectors(f + s * 75 + m * 60 * 75)) }

// ──────────────────────── Blocksize ───────────────────────────────────

/// Map track type string to byte blocksize.
pub fn blocksize_for_type(track_type: &str) -> Option<u32> { match track_type {
    "AUDIO" | "MODE1/2352" | "MODE2/2352" | "CDI/2352" => Some(2352),
    "CDG" => Some(2448),
    "MODE1/2048" => Some(2048),
    "MODE2/2336" | "CDI/2336" => Some(2336),
    _ => None,
} }

// ──────────────────────── Filename helpers ────────────────────────────

/// Redump-style track filename.
pub fn track_filename(prefix: &str, track_num: u32, track_count: usize) -> String { if track_count == 1 {
    format!("{prefix}.bin")
} else if track_count > 9 {
    format!("{prefix} (Track {track_num:02}).bin")
} else {
    format!("{prefix} (Track {track_num}).bin")
} }

pub fn cue_basename(cue_path: &Path) -> String { cue_path
    .file_stem()
    .and_then(|s| s.to_str())
    .unwrap_or("output")
    .to_string() }
