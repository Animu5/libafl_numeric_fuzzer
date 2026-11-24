#![cfg(unix)]

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[test]
fn selected_simulation_builds_and_evaluates_a_seed_without_hardware() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let build = root.join("build");
    fs::create_dir_all(build.join("src")).unwrap();
    fs::write(
        build.join("Makefile"),
        "snippet.elf: src/snippet.S\n\tcp $< $@\n",
    )
    .unwrap();
    let simulator = root.join("simulator");
    fs::write(
        &simulator,
        format!(
            "#!/bin/sh\ntest -f \"$1\" || exit 1\ncp \"{}/trace.vcd\" \"$2\"\n",
            root.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&simulator, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(root.join("trace.vcd"), TRACE_VCD).unwrap();
    let solutions = root.join("solutions");
    let log = root.join("fuzzer.log");
    let mut child = Command::new(env!("CARGO_BIN_EXE_libafl_fuzzer"))
        .args(["--backend", "simulation", "--assembly-environment"])
        .arg(&build)
        .args([
            "--atndecode-path",
            "/bin/cat",
            "--riscv-toolpath",
            "/opt/riscv",
            "--solutions-path",
        ])
        .arg(&solutions)
        .arg("--log-file")
        .arg(&log)
        .arg("--simulator-path")
        .arg(&simulator)
        .args([
            "--target-scope",
            "TOP.dut",
            "--simulator-arg={elf}",
            "--simulator-arg={vcd}",
        ])
        .env("TERM", "xterm")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    let best = solutions.join("best.S");
    while !best.exists() && start.elapsed() < Duration::from_secs(10) {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    let success = best.exists();
    if child.try_wait().unwrap().is_none() {
        child.kill().unwrap();
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        success,
        "fuzzer exited before producing a score: {}; stderr: {}; log: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
        fs::read_to_string(log).unwrap_or_default()
    );
}

/// A minimal VCD with one 1-bit signal toggling at #10 (1ns timescale), so the
/// in-process analysis yields activity 1e8 toggles/s and coverage 1.0.
const TRACE_VCD: &str = "\
$timescale 1ns $end
$scope module TOP $end
$scope module dut $end
$var wire 1 ! value $end
$upscope $end
$upscope $end
$enddefinitions $end
#0
0!
#10
1!
";

fn simulation_fixture(root: &std::path::Path) -> std::path::PathBuf {
    let build = root.join("build");
    fs::create_dir_all(build.join("src")).unwrap();
    fs::write(
        build.join("Makefile"),
        "snippet.elf: src/snippet.S\n\tcp $< $@\n",
    )
    .unwrap();
    let simulator = root.join("simulator");
    fs::write(
        &simulator,
        format!(
            "#!/bin/sh\ntest -f \"$1\" || exit 1\ncp \"{}/trace.vcd\" \"$2\"\n",
            root.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&simulator, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(root.join("trace.vcd"), TRACE_VCD).unwrap();
    simulator
}

#[test]
fn log_level_and_log_monitor_write_progress_to_the_log_file() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let simulator = simulation_fixture(root);
    let solutions = root.join("solutions");
    let log = root.join("fuzzer.log");
    let mut child = Command::new(env!("CARGO_BIN_EXE_libafl_fuzzer"))
        .args(["--backend", "simulation", "--assembly-environment"])
        .arg(root.join("build"))
        .args([
            "--atndecode-path",
            "/bin/cat",
            "--riscv-toolpath",
            "/opt/riscv",
            "--solutions-path",
        ])
        .arg(&solutions)
        .arg("--log-file")
        .arg(&log)
        .arg("--log-level")
        .arg("debug")
        .arg("--monitor")
        .arg("log")
        .arg("--simulator-path")
        .arg(&simulator)
        .args([
            "--target-scope",
            "TOP.dut",
            "--simulator-arg={elf}",
            "--simulator-arg={vcd}",
        ])
        .env("TERM", "xterm")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    let best = solutions.join("best.S");
    while !best.exists() && start.elapsed() < Duration::from_secs(10) {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    let success = best.exists();
    if child.try_wait().unwrap().is_none() {
        child.kill().unwrap();
    }
    let output = child.wait_with_output().unwrap();
    let log = fs::read_to_string(&log).unwrap_or_default();
    assert!(
        success,
        "fuzzer exited before producing a score: {}; stderr: {}; log: {log}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    // A successful simulation and its analyzed metrics are visible at info level.
    assert!(log.contains("Current toggle activity"), "log: {log}");
    // --log-level debug turns on the per-run simulation and analysis detail.
    assert!(log.contains("Running simulator"), "log: {log}");
    assert!(log.contains("VCD analysis"), "log: {log}");
    // LibAFL's own `log` records are bridged into the same file.
    assert!(
        log.lines().any(|line| line.contains("libafl")),
        "no bridged libafl log lines; log: {log}"
    );
    // The headless `log` monitor writes periodic stats lines.
    assert!(log.contains("execs="), "log: {log}");
}
