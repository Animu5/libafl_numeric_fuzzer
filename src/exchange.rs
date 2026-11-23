use crate::evaluation::{Evaluation, Metric};
use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    os::fd::AsRawFd,
    path::Path,
};

/// Creates the exchange root and all per-worker directories. Fails if the root
/// already exists (use a fresh solutions path per campaign).
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

/// Idempotently creates the exchange root and all per-worker directories.
pub fn ensure(root: &Path, workers: usize) -> io::Result<()> {
    fs::create_dir_all(root)?;
    for index in 0..workers {
        fs::create_dir_all(root.join(format!("worker-{index:04}")))?;
    }
    Ok(())
}

/// Updates the shared global-best record if `evaluation` beats the current best.
/// Saves the input and assembly under `root/worker-NNNN/` and atomically replaces
/// `root/global-best.txt` under an exclusive flock. Input exchange between workers
/// is handled by LLMP; this only persists the global best for visibility and
/// reproducibility.
pub fn update_global_best(
    root: &Path,
    worker: usize,
    input: &[u8],
    assembly: &str,
    evaluation: &Evaluation,
) -> io::Result<()> {
    if evaluation.metric() != Metric::ToggleCoverage {
        return Err(io::Error::other(
            "Global best only supports toggle coverage",
        ));
    }

    let lock = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(root.join("lock"))?;
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(io::Error::last_os_error());
    }

    let worker_name = format!("worker-{worker:04}");
    let folder = root.join(&worker_name);
    fs::create_dir_all(&folder)?;

    let mut assembly_file = tempfile::NamedTempFile::new_in(root)?;
    assembly_file.write_all(assembly.as_bytes())?;
    let assembly_path = folder.join(format!(
        "assembly-{}",
        assembly_file.path().file_name().unwrap().to_string_lossy()
    ));
    assembly_file
        .persist_noclobber(&assembly_path)
        .map_err(|e| e.error)?;

    let mut input_file = tempfile::NamedTempFile::new_in(root)?;
    input_file.write_all(input)?;
    let input_path = folder.join(format!(
        "input-{}",
        input_file.path().file_name().unwrap().to_string_lossy()
    ));
    input_file
        .persist_noclobber(&input_path)
        .map_err(|e| e.error)?;

    let record = root.join("global-best.txt");
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
        let mut temp = tempfile::NamedTempFile::new_in(root)?;
        writeln!(temp, "toggle_coverage={}", evaluation.score())?;
        writeln!(
            temp,
            "toggle_activity={}",
            evaluation
                .toggle_activity_per_second()
                .expect("coverage has activity")
        )?;
        writeln!(temp, "worker={worker_name}")?;
        writeln!(
            temp,
            "input={worker_name}/{}",
            input_path.file_name().unwrap().to_string_lossy()
        )?;
        writeln!(
            temp,
            "assembly={worker_name}/{}",
            assembly_path.file_name().unwrap().to_string_lossy()
        )?;
        temp.persist(&record).map_err(|e| e.error)?;
        eprintln!(
            "New global best toggle coverage: {} ({worker_name})",
            evaluation.score()
        );
    }
    drop(lock);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_updates_preserve_full_precision_best() {
        let temp = tempfile::tempdir().unwrap();
        prepare(temp.path(), 2).unwrap();
        let root = temp.path().join("parallel-exchange");
        let mut threads = Vec::new();
        for index in 0..2 {
            let root = root.clone();
            threads.push(std::thread::spawn(move || {
                let score = if index == 0 { 0.50000001 } else { 0.50000002 };
                update_global_best(
                    &root,
                    index,
                    &[index as u8],
                    &format!("assembly-{index}"),
                    &Evaluation::toggle_coverage(score, 10.0).unwrap(),
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
