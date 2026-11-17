use std::{
    fs,
    io::{self, Read},
    os::unix::process::CommandExt,
    path::Path,
    process::{Child, ChildStdin, Command, Stdio},
    thread,
    time::Duration,
};

/// The launcher holds this pipe open for the worker's lifetime.
pub fn watch_parent() {
    thread::spawn(|| {
        let mut input = io::stdin().lock();
        let mut buffer = [0; 128];
        loop {
            match input.read(&mut buffer) {
                Ok(0) | Err(_) => {
                    // Workers lead their own group, including decoder and make children.
                    unsafe {
                        if libc::getpgrp() == libc::getpid() {
                            libc::kill(-libc::getpid(), libc::SIGKILL);
                        }
                        libc::_exit(1)
                    }
                }
                Ok(_) => {}
            }
        }
    });
}

struct Worker {
    child: Child,
    _liveness: ChildStdin,
}

pub fn launch(
    dirs: &[crate::cli::WorkerDirs],
    workspace: Option<&Path>,
    assembly_template: &Path,
    solutions: &Path,
) -> io::Result<()> {
    let exchange = solutions.join("parallel-exchange");
    if exchange.try_exists()? {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("Exchange already exists: {}", exchange.display()),
        ));
    }
    if let Some(workspace) = workspace {
        if workspace.try_exists()? {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("Worker workspace already exists: {}", workspace.display()),
            ));
        }
        prepare_workspace(workspace, assembly_template, dirs)?;
    }
    let count = dirs.len();
    crate::exchange::prepare(solutions, count)?;
    let executable = std::env::current_exe()?;
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let mut workers = Vec::with_capacity(count);
    for index in 0..count {
        let result = Command::new(&executable)
            .args(&args)
            .arg("--internal-worker-index")
            .arg(index.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .process_group(0)
            .spawn();
        match result {
            Ok(mut child) => {
                let liveness = child.stdin.take().expect("piped worker stdin");
                workers.push(Worker {
                    child,
                    _liveness: liveness,
                });
            }
            Err(error) => {
                stop_workers(&mut workers);
                return Err(io::Error::other(format!(
                    "Cannot start worker {index}: {error}"
                )));
            }
        }
    }
    eprintln!("Started {count} independent simulation workers; see worker-specific logs and solution directories.");
    loop {
        for index in 0..workers.len() {
            match workers[index].child.try_wait() {
                Ok(Some(status)) => {
                    stop_workers(&mut workers);
                    return Err(io::Error::other(format!(
                        "Worker {index} exited unexpectedly: {status}"
                    )));
                }
                Ok(None) => {}
                Err(error) => {
                    stop_workers(&mut workers);
                    return Err(io::Error::other(format!(
                        "Cannot monitor worker {index}: {error}"
                    )));
                }
            }
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn prepare_workspace(
    workspace: &Path,
    template: &Path,
    dirs: &[crate::cli::WorkerDirs],
) -> io::Result<()> {
    fs::create_dir(workspace).map_err(|e| {
        io::Error::new(
            e.kind(),
            format!(
                "Cannot create fresh worker workspace {}: {e}",
                workspace.display()
            ),
        )
    })?;
    for worker in dirs {
        let parent = worker
            .assembly
            .parent()
            .ok_or_else(|| io::Error::other("Invalid worker path"))?;
        fs::create_dir(parent)?;
        copy_directory(template, &worker.assembly)?;
        fs::create_dir(&worker.simulator)?;
    }
    Ok(())
}

fn copy_directory(source: &Path, destination: &Path) -> io::Result<()> {
    fs::create_dir(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let dest = destination.join(entry.file_name());
        if ty.is_dir() {
            copy_directory(&entry.path(), &dest)?;
        } else if ty.is_file() {
            fs::copy(entry.path(), dest)?;
        } else {
            return Err(io::Error::other(format!(
                "Unsupported symlink or special file in assembly template: {}",
                entry.path().display()
            )));
        }
    }
    fs::set_permissions(destination, fs::metadata(source)?.permissions())?;
    Ok(())
}

fn stop_workers(workers: &mut [Worker]) {
    for worker in workers.iter_mut() {
        let pid = worker.child.id() as libc::pid_t;
        if unsafe { libc::kill(-pid, libc::SIGKILL) } != 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                eprintln!("Could not stop worker group {pid}: {error}");
            }
        }
    }
    for worker in workers {
        if let Err(e) = worker.child.wait() {
            eprintln!("Could not reap worker {}: {e}", worker.child.id());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn dirs(root: &Path, count: usize) -> Vec<crate::cli::WorkerDirs> {
        (0..count)
            .map(|index| {
                let worker = root.join(format!("worker-{index:04}"));
                crate::cli::WorkerDirs {
                    assembly: worker.join("build"),
                    simulator: worker.join("sim"),
                }
            })
            .collect()
    }

    #[test]
    fn creates_two_hundred_isolated_worker_directories() {
        let temp = tempfile::tempdir().unwrap();
        let template = temp.path().join("template");
        fs::create_dir(&template).unwrap();
        fs::write(template.join("file"), b"template").unwrap();
        let root = temp.path().join("workspace");
        let workers = dirs(&root, 200);
        prepare_workspace(&root, &template, &workers).unwrap();
        for worker in &workers {
            assert_eq!(fs::read(worker.assembly.join("file")).unwrap(), b"template");
            assert!(worker.simulator.is_dir());
        }
        fs::write(workers[0].assembly.join("file"), b"changed").unwrap();
        assert_eq!(fs::read(template.join("file")).unwrap(), b"template");
        assert_eq!(
            fs::read(workers[1].assembly.join("file")).unwrap(),
            b"template"
        );
        assert!(prepare_workspace(&root, &template, &workers).is_err());
        assert_eq!(
            fs::read(workers[0].assembly.join("file")).unwrap(),
            b"changed"
        );
    }

    #[test]
    fn workspace_rejects_symlinked_template_contents() {
        let temp = tempfile::tempdir().unwrap();
        let template = temp.path().join("template");
        fs::create_dir(&template).unwrap();
        symlink("/tmp", template.join("shared")).unwrap();
        let root = temp.path().join("workspace");
        assert!(prepare_workspace(&root, &template, &dirs(&root, 1))
            .unwrap_err()
            .to_string()
            .contains("symlink"));
    }
}
