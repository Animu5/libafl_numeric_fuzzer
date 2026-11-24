use std::fs::File;
use std::io;
use std::io::{BufRead, BufReader, ErrorKind};
use std::path::Path;
use std::collections::HashSet;

pub type Excluded = HashSet<String>;

pub fn load(exclude_signal_file: Option<&Path>) -> io::Result<Excluded> {
    match exclude_signal_file {
        None => Ok(HashSet::new()),
        Some(file) => read_exclude_signals(file),
    }
}

fn read_exclude_signals(exclude_signal_file: &Path) -> io::Result<HashSet<String>> {
    if !exclude_signal_file.exists() {
        return Err(io::Error::from(ErrorKind::NotFound));
    }
    let file = File::open(exclude_signal_file)?;
    let reader = BufReader::new(file);
    let mut excluded = HashSet::new();
    for line in reader.lines() {
        let line = line.map_err(|e| {
            io::Error::new(e.kind(), "Unable to read line from exclude signal file")
        })?;
        let line = line.trim();
        if !line.is_empty() {
            excluded.insert(line.to_string());
        }
    }
    Ok(excluded)
}
