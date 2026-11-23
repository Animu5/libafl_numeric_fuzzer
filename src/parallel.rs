//! Worker workspace setup for parallel fuzzing. The actual process spawning,
//! CPU pinning, and crash restart are handled by LibAFL's `Launcher` with LLMP.

use std::{
    fs, io,
    path::Path,
};

/// Creates a fresh worker workspace: copies the assembly template into each
/// `worker-NNNN/build` and creates an empty `worker-NNNN/sim` directory.
/// The workspace itself must not exist yet.
pub fn prepare_workspace(
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
