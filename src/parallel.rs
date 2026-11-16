use std::{
    io::{self, Read},
    os::unix::process::CommandExt,
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

pub fn launch(count: usize) -> io::Result<()> {
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
