use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write, BufRead};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use cue::cd::{CD, DiscMode};
use cue::track::{TrackMode, TrackSubMode};

use regex::Regex;

struct Index {
    id: u32,
    stamp: String,
    file_offset: u32, // Assuming cuestamp_to_sectors returns an i32
}

impl Index {
    fn new(id: u32, stamp: String, file_offset: u32) -> Index {
        Index {
            id,
            stamp,
            file_offset,
        }
    }
}

struct Track {
    num: u32,
    indexes: Vec<Index>,
    track_type: String,
    sectors: Option<u32>,
    file_offset: Option<u32>,
}

impl Track {
    fn new(num: u32, track_type: String) -> Track {
        Track {
            num,
            indexes: Vec::new(),
            track_type,
            sectors: None,
            file_offset: None,
        }
    }
}

struct BinFile {
    filename: String,
    tracks: Vec<Track>,
    size: Option<u64>,
}

impl BinFile {
    fn new(filepath: PathBuf) -> io::Result<BinFile> {
        let size = fs::metadata(&filepath)?.len(); // Performance hit

        Ok(BinFile {
            filename: filepath.to_str().unwrap().to_string(),
            tracks: Vec::new(),
            size: Some(size),
        })
    }
}

fn cuestamp_to_sectors(timestamp: &str) -> Result<u32, &'static str> {
    let re = Regex::new(r"(\d+):(\d+):(\d+)").map_err(|_| "Regex compilation failed")?;
    if let Some(caps) = re.captures(timestamp) {
        let minutes = caps.get(1).ok_or("Invalid timestamp")?.as_str().parse::<u32>().map_err(|_| "Invalid minutes")?;
        let seconds = caps.get(2).ok_or("Invalid timestamp")?.as_str().parse::<u32>().map_err(|_| "Invalid seconds")?;
        let frames = caps.get(3).ok_or("Invalid timestamp")?.as_str().parse::<u32>().map_err(|_| "Invalid frames")?;

        Ok(frames + (seconds * 75) + (minutes * 60 * 75))
    } else {
        Err("Timestamp does not match pattern")
    }
}

fn print_bin_files(bin_files: &Vec<BinFile>) {
    for bin_file in bin_files{
        println!("-- File --");
        println!("Filename: {}", bin_file.filename);
        println!("Size: {} bytes", bin_file.size.unwrap_or(0));
        println!("Tracks: {}", bin_file.tracks.len());

        for track in &bin_file.tracks {
            println!("-- Track --");
            println!("Track number: {}", track.num);
            println!("Track type: {}", track.track_type);
            println!("Track indexes: {}", track.indexes.len());

            for index in &track.indexes {
                println!("-- Index --");
                println!("Index id: {}", index.id);
                println!("Index stamp: {}", index.stamp);
                println!("Index file offset: {}", index.file_offset);
            }
        }
    }
}

fn get_bin_from_cue(cue_path : &str) -> io::Result<Vec<BinFile>> {
    let mut bin_files: Vec<BinFile> = Vec::new();

    let mut missing_bin_file = false;

    let file_pattern = Regex::new(r#"FILE "(.*?)" BINARY"#).unwrap();
    let track_pattern = Regex::new(r#"TRACK (\d+) ([^\s]*)"#).unwrap();
    let index_pattern = Regex::new(r#"INDEX (\d+) (\d+:\d+:\d+)"#).unwrap();

    let cue_file = File::open(cue_path)?;
    let reader = io::BufReader::new(cue_file);

    let start = Instant::now();
    for line in reader.lines() {
        let line = line?;

        // Process file lines
        if let Some(caps) = file_pattern.captures(&line) {
            let start_bin_file = Instant::now();
            if let Some(bin) = caps.get(1) {
                let bin_file_path = Path::new(cue_path).parent().unwrap().join(bin.as_str());
                //let bin_file = File::open(bin_file_path);
                //println!("Bin file: {}", bin_file_path.to_str().unwrap());
                let current_bin_file = BinFile::new(bin_file_path).unwrap();
                bin_files.push(current_bin_file);
                let duration_bin_file = start_bin_file.elapsed();
                println!("Time elapsed in BinFile::new() is: {:?}", duration_bin_file);

                continue;
            }
        }
        // Process track lines
        if let Some(caps) = track_pattern.captures(&line) {
            let start_track = Instant::now();
            if let (Some(track_number_match), Some(track_type_match)) = (caps.get(1), caps.get(2)) {
                let track_number = track_number_match.as_str().parse::<u32>().unwrap();
                let track_type = track_type_match.as_str().to_string();

                if let Some(last_file) = bin_files.last_mut() {
                    let current_track = Track::new(track_number, track_type);
                    last_file.tracks.push(current_track);
                }
                let duration_tracks = start_track.elapsed();
                println!("Time elapsed in Track::new() is: {:?}", duration_tracks);
                continue;
            }
        }
        // Process index lines
        if let Some(caps) = index_pattern.captures(&line) {
            let start_index = Instant::now();
            if let (Some(index_number_match), Some(timestamp_match)) = (caps.get(1), caps.get(2)) {
                let index_number = index_number_match.as_str().parse::<u32>().unwrap();
                let timestamp = timestamp_match.as_str().to_string();
                let file_offset = cuestamp_to_sectors(&timestamp).unwrap(); // Convert timestamp to sectors
                
                if let Some(last_file) = bin_files.last_mut() {
                    if let Some(last_track) = last_file.tracks.last_mut() {
                        let current_index = Index::new(index_number, timestamp, file_offset);
                        last_track.indexes.push(current_index); // Modify the last Track in the last BinFile
                    }
                }
                let duration_index = start_index.elapsed();
                println!("Time elapsed in Index::new() is: {:?}", duration_index);

                continue;
            }
        }
    }
    let duration = start.elapsed();
    println!("Time elapsed in get_bin_from_cue() is: {:?}", duration);

    // Check if bin file is missing
    // if missing_bin_file {
    //     eprintln!("Bin file is missing!");
    //     return Ok(bin_files);
    // }

    Ok(bin_files)
}

fn get_cd_from_cue(cue_path : &str) -> io::Result<CD> {
    println!("Cue path: {}", cue_path);
    match Path::new(cue_path).exists() {
        true => println!("Cue file exists!"),
        false => {
            eprintln!("Cue file does not exist!");
            return Ok(CD::parse("".to_string()).unwrap());
        }
        
    }
    let cue_file = File::open(cue_path)?;
    // Read cue file and store it in a single string variable
    let mut cue_contents = String::new();
    let mut reader = io::BufReader::new(cue_file);
    reader.read_to_string(&mut cue_contents)?;

    let cd = CD::parse(cue_contents.to_string()).unwrap();

    println!("Number of tracks: {}", cd.get_track_count());
    let mode = match cd.get_mode() {
        DiscMode::CD_DA => "CD-DA",
        DiscMode::CD_ROM => "CD-ROM",
        DiscMode::CD_ROM_XA => "CD-ROM XA",
    };
    println!("Mode: {}", mode);
    println!("");

    for (index, track) in cd.tracks().iter().enumerate() {
        println!("Track {}", index + 1);
        println!("Filename: {}", track.get_filename());
        println!("Start: {}", track.get_start());
        println!("Length: {:?}", track.get_length());
        println!("Pregap: {:?}", track.get_zero_pre());
        println!("Postgap: {:?}", track.get_zero_post());
        println!("");
    }

    Ok(cd)
}

fn merge_files(merged_filename: &str, files: Vec<&str>) -> io::Result<bool> {
    if Path::new(merged_filename).exists() {
        eprintln!("Target merged bin path already exists: {}", merged_filename);
        return Ok(false);
    }

    let mut outfile = OpenOptions::new().write(true).create_new(true).open(merged_filename)?;

    let chunksize = 1024 * 1024;
    for file in files {
        let mut infile = File::open(file)?;
        let mut buffer = vec![0; chunksize];
        while let Ok(bytes_read) = infile.read(&mut buffer) {
            if bytes_read == 0 {
                break;
            }
            outfile.write_all(&buffer[..bytes_read])?;
        }
    }
    Ok(true)
}

fn read_directory(file_list: &mut Vec<String>, dir: &Path) -> io::Result<bool> {
    match fs::read_dir(dir) {
        Err(e) => println!("There was an error reading the directory: {}", e),
        Ok(paths) => {
            for path in paths {
                match path {
                    Err(e) => println!("There was an error with one of the entries: {}", e),
                    Ok(p) => if p.path().is_file() {
                        let file_name = p.file_name().into_string().unwrap();
                        file_list.push(file_name);
                    }
                }
            }
        },
    }
    Ok(true)
}

fn files(dir: &Path) -> Result<Vec<PathBuf>, io::Error> {
    Ok(fs::read_dir(dir)?
        .into_iter()
        .filter(|r| r.is_ok()) // Get rid of Err variants for Result<DirEntry>
        .map(|r| r.unwrap().path()) // This is safe, since we only have the Ok variants
        .filter(|r| r.is_file()) // Filter out non-files
        .collect())
}

fn main() {
    // ---- Read Cue File tests ----
    let path = Path::new("/mnt/d/Downloads/binmergetests/Mortal Kombat 3 (USA)");
    // Find cue file by its extension
    let start = Instant::now();
    let cue_path = path.join(path.file_name().unwrap()).with_extension("cue");
    let bin_files = get_bin_from_cue(cue_path.to_str().unwrap());
    //let _ = get_cd_from_cue(cue_path.to_str().unwrap());
    let duration = start.elapsed();

    // Print bin files
    // match bin_files {
    //     Ok(bin_files) => print_bin_files(&bin_files),
    //     Err(e) => println!("Error: {}", e),
    // }

    println!("Time elapsed in files() is: {:?}", duration);

    // ---- Read Cue File tests ----


    // ---- Merge Files tests ----
    // Example usage
    //let result = merge_files("output_file.bin", vec!["file1.bin", "file2.bin"]);
    // ---- Merge Files tests ----
    
    
    // ---- Directory Reading tests ----
    // let start = Instant::now();
    // let path = Path::new("D:\\Downloads\\GB");
    // let result = files(path);
    // println!("{} files added successfully!", result.unwrap().len());
    // let duration = start.elapsed();
    // println!("Time elapsed in files() is: {:?}", duration);


    // let start = Instant::now();
    // let mut file_list: Vec<String> = Vec::new();

    // let result = read_directory(&mut file_list, path);

    // match result {
    //     Ok(_) => {
    //         println!("{} files added successfully!", file_list.len());
    //         let duration = start.elapsed();
    //         println!("Time elapsed in read_directory() is: {:?}", duration);
    //     }
    //     Err(e) => println!("Error listing files: {}", e),
    // }
    // ---- Directory Reading tests ----
    
}
