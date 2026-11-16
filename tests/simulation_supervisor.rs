#![cfg(unix)]

use std::{
    fs,
    os::unix::{fs::PermissionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

fn script(path: &Path, contents: &str) {
    fs::write(path, format!("#!/bin/sh\n{contents}\n")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

fn wait_for(mut condition: impl FnMut() -> bool) {
    let start = Instant::now();
    while !condition() {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "timed out waiting for supervisor"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn parent_process_helper() {
    let Some(dir) = std::env::var_os("SIMULATION_PARENT_HELPER") else {
        return;
    };
    let dir = PathBuf::from(dir);
    let mut supervisor = Command::new(env!("CARGO_BIN_EXE_libafl_fuzzer"))
        .arg("--internal-simulation-supervisor")
        .arg(dir.join("program.elf"))
        .arg(dir.join("simulator"))
        .arg(&dir)
        .arg(dir.join("analyzer"))
        .arg("TOP.dut")
        .arg("")
        .args(["30", "0", "30", "0", "{elf}", "{vcd}"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .unwrap();
    let _liveness = supervisor.stdin.take().unwrap();
    fs::write(dir.join("supervisor-pid"), supervisor.id().to_string()).unwrap();
    supervisor.wait().unwrap();
}

#[test]
fn supervisor_returns_metrics_on_success() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path();
    fs::write(path.join("program.elf"), b"ELF").unwrap();
    script(&path.join("simulator"), "echo wave > \"$2\"");
    script(
        &path.join("analyzer"),
        "printf 'toggle_activity=12\\ntoggle_coverage=0.5\\n'",
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_libafl_fuzzer"))
        .arg("--internal-simulation-supervisor")
        .arg(path.join("program.elf"))
        .arg(path.join("simulator"))
        .arg(path)
        .arg(path.join("analyzer"))
        .arg("TOP.dut")
        .arg("")
        .args(["2", "0", "2", "0", "{elf}", "{vcd}"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let _liveness = child.stdin.take().unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        String::from_utf8(result.stdout).unwrap(),
        "toggle_activity=12\ntoggle_coverage=0.5\n"
    );
}

#[test]
fn supervisor_cleans_up_after_parent_is_killed() {
    for analyzer_phase in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        fs::write(path.join("program.elf"), b"ELF").unwrap();
        let simulator = if analyzer_phase {
            "printf '%s' \"$2\" > vcd-path; echo wave > \"$2\""
        } else {
            "printf '%s' \"$2\" > vcd-path; echo wave > \"$2\"; (sleep 1; echo leaked > marker) & wait"
        };
        let analyzer = "echo started > analyzer-started; (sleep 1; echo leaked > marker) & wait";
        script(&path.join("simulator"), simulator);
        script(&path.join("analyzer"), analyzer);
        let mut parent = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "parent_process_helper"])
            .env("SIMULATION_PARENT_HELPER", path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        wait_for(|| {
            fs::read_to_string(path.join("vcd-path")).is_ok_and(|content| !content.is_empty())
        });
        if analyzer_phase {
            wait_for(|| path.join("analyzer-started").exists());
        }
        let vcd = PathBuf::from(fs::read_to_string(path.join("vcd-path")).unwrap());
        wait_for(|| vcd.exists());
        parent.kill().unwrap();
        parent.wait().unwrap();
        wait_for(|| !vcd.exists());
        assert!(!vcd.parent().unwrap().exists());
        thread::sleep(Duration::from_millis(1200));
        assert!(
            !path.join("marker").exists(),
            "simulation child survived parent death"
        );
    }
}
