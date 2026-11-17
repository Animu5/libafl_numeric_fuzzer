use crate::evaluation::{Evaluation, Metric};
use libafl::inputs::BytesInput;
use std::{
    collections::HashSet,
    fs::{self, OpenOptions},
    io::{self, Write},
    os::fd::AsRawFd,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

pub fn prepare(solutions: &Path, workers: usize) -> io::Result<()> {
    fs::create_dir_all(solutions)?;
    let root = solutions.join("parallel-exchange");
    fs::create_dir(&root).map_err(|e| {
        io::Error::new(
            e.kind(),
            format!("Cannot create fresh exchange {}: {e}", root.display()),
        )
    })?;
    for index in 0..workers {
        fs::create_dir(root.join(format!("worker-{index:04}")))?;
    }
    Ok(())
}

/// Idempotently creates the exchange root and all per-worker directories. Used by
/// workers running as their own processes (e.g. SLURM array tasks); in launcher mode
/// the strict `prepare` already created them.
pub fn ensure(root: &Path, workers: usize) -> io::Result<()> {
    fs::create_dir_all(root)?;
    for index in 0..workers {
        fs::create_dir_all(root.join(format!("worker-{index:04}")))?;
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub struct Exchange {
    root: PathBuf,
    index: usize,
    workers: usize,
    seen: HashSet<PathBuf>,
    seen_inputs: HashSet<Vec<u8>>,
    last_scan: Option<Instant>,
}

impl Exchange {
    pub fn new(root: PathBuf, index: usize, workers: usize) -> Self {
        Self {
            root,
            index,
            workers,
            seen: HashSet::new(),
            seen_inputs: HashSet::new(),
            last_scan: None,
        }
    }

    pub fn remember_input(&mut self, input: &[u8]) {
        self.seen_inputs.insert(input.to_vec());
    }

    pub fn publish(&self, input: &[u8], assembly: &str, evaluation: Evaluation) -> io::Result<()> {
        if evaluation.metric() != Metric::ToggleCoverage {
            return Err(io::Error::other(
                "Parallel exchange only supports toggle coverage",
            ));
        }

        let lock = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(self.root.join("lock"))?;
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(io::Error::last_os_error());
        }

        let worker = format!("worker-{:04}", self.index);
        let folder = self.root.join(&worker);
        let mut assembly_file = tempfile::NamedTempFile::new_in(&self.root)?;
        assembly_file.write_all(assembly.as_bytes())?;
        let assembly_path = folder.join(format!(
            "assembly-{}",
            assembly_file.path().file_name().unwrap().to_string_lossy()
        ));
        assembly_file
            .persist_noclobber(&assembly_path)
            .map_err(|e| e.error)?;
        let mut input_file = tempfile::NamedTempFile::new_in(&self.root)?;
        input_file.write_all(input)?;
        let input_path = folder.join(format!(
            "input-{}",
            input_file.path().file_name().unwrap().to_string_lossy()
        ));
        input_file
            .persist_noclobber(&input_path)
            .map_err(|e| e.error)?;
        let record = self.root.join("global-best.txt");
        let best = match fs::read_to_string(&record) {
            Ok(content) => {
                let score = content
                    .lines()
                    .next()
                    .and_then(|line| line.strip_prefix("toggle_coverage="))
                    .and_then(|value| value.parse::<f64>().ok())
                    .filter(|value| value.is_finite() && (0.0..=1.0).contains(value))
                    .ok_or_else(|| io::Error::other("Invalid global-best record"))?;
                Some(score)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        if best.is_none_or(|score| evaluation.score() > score) {
            let mut temp = tempfile::NamedTempFile::new_in(&self.root)?;
            writeln!(temp, "toggle_coverage={}", evaluation.score())?;
            writeln!(
                temp,
                "toggle_activity={}",
                evaluation
                    .toggle_activity_per_second()
                    .expect("coverage has activity")
            )?;
            writeln!(temp, "worker={worker}")?;
            writeln!(
                temp,
                "input={worker}/{}",
                input_path.file_name().unwrap().to_string_lossy()
            )?;
            writeln!(
                temp,
                "assembly={worker}/{}",
                assembly_path.file_name().unwrap().to_string_lossy()
            )?;
            temp.persist(&record).map_err(|e| e.error)?;
            eprintln!(
                "New global best toggle coverage: {} ({worker})",
                evaluation.score()
            );
        }
        drop(lock);
        Ok(())
    }

    pub fn pending_inputs(&mut self) -> io::Result<Vec<BytesInput>> {
        if self
            .last_scan
            .is_some_and(|last| last.elapsed() < Duration::from_secs(1))
        {
            return Ok(Vec::new());
        }
        self.last_scan = Some(Instant::now());
        let mut inputs = Vec::new();
        for index in 0..self.workers {
            if index == self.index {
                continue;
            }
            let dir = self.root.join(format!("worker-{index:04}"));
            for entry in fs::read_dir(dir)? {
                let entry = entry?;
                let name = entry.file_name();
                if !name.to_string_lossy().starts_with("input-") || !entry.file_type()?.is_file() {
                    continue;
                }
                let path = entry.path();
                if !self.seen.contains(&path) {
                    let input = fs::read(&path)?;
                    if self.seen_inputs.insert(input.clone()) {
                        inputs.push(BytesInput::new(input));
                    }
                    self.seen.insert(path);
                }
            }
        }
        Ok(inputs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_publications_preserve_full_precision_best_and_exchange_inputs() {
        let temp = tempfile::tempdir().unwrap();
        prepare(temp.path(), 2).unwrap();
        let root = temp.path().join("parallel-exchange");
        let mut threads = Vec::new();
        for index in 0..2 {
            let root = root.clone();
            threads.push(std::thread::spawn(move || {
                let exchange = Exchange::new(root, index, 2);
                let score = if index == 0 { 0.50000001 } else { 0.50000002 };
                exchange
                    .publish(
                        &[index as u8],
                        &format!("assembly-{index}"),
                        Evaluation::toggle_coverage(score, 10.0).unwrap(),
                    )
                    .unwrap();
            }));
        }
        for thread in threads {
            thread.join().unwrap();
        }
        let text = fs::read_to_string(root.join("global-best.txt")).unwrap();
        assert!(text.contains("toggle_coverage=0.50000002\n"), "{text}");
        assert!(text.contains("worker=worker-0001\n"), "{text}");
        let assembly = text
            .lines()
            .find_map(|s| s.strip_prefix("assembly="))
            .unwrap();
        let input = text.lines().find_map(|s| s.strip_prefix("input=")).unwrap();
        assert_eq!(
            fs::read_to_string(root.join(assembly)).unwrap(),
            "assembly-1"
        );
        assert_eq!(fs::read(root.join(input)).unwrap(), [1]);
        let mut receiver = Exchange::new(root, 0, 2);
        let inputs = receiver.pending_inputs().unwrap();
        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs[0].as_ref(), &[1]);
        assert!(receiver.pending_inputs().unwrap().is_empty());
        assert!(prepare(temp.path(), 2).is_err());
    }

    #[test]
    fn ensure_creates_the_exchange_idempotently() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("parallel-exchange");
        ensure(&root, 3).unwrap();
        assert!(root.join("worker-0000").is_dir());
        assert!(root.join("worker-0002").is_dir());
        ensure(&root, 3).unwrap();
    }
}
