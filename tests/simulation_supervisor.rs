#![cfg(unix)]

mod common;

use common::executable as script;
use std::{
    fs,
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

/// Minimal VCD with one 1-bit signal toggling at #10 (1ns timescale).
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

/// Spawns the supervisor the way `SimulationBackend::run` does:
/// `--internal-simulation-supervisor <elf> <simulator> <working_dir> <vcd> <secs> <nanos> <args...>`
/// with `{elf}`/`{vcd}` already expanded in the simulator arguments.
fn spawn_supervisor(
    dir: &Path,
    elf: &Path,
    simulator: &Path,
    vcd: &Path,
    args: &[&str],
) -> std::process::Child {
    let mut command = Command::new(env!("CARGO_BIN_EXE_libafl_fuzzer"));
    command
        .arg("--internal-simulation-supervisor")
        .arg(elf)
        .arg(simulator)
        .arg(dir)
        .arg(vcd)
        .arg("30")
        .arg("0");
    for arg in args {
        command.arg(
            arg.replace("{elf}", &elf.to_string_lossy())
                .replace("{vcd}", &vcd.to_string_lossy()),
        );
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap()
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
    let elf = dir.join("program.elf");
    let vcd = dir.join("trace.vcd");
    let mut supervisor = spawn_supervisor(&dir, &elf, &dir.join("simulator"), &vcd, &["{elf}", "{vcd}"]);
    drop(supervisor.stdout.take());
    drop(supervisor.stderr.take());
    let _liveness = supervisor.stdin.take().unwrap();
    fs::write(dir.join("supervisor-pid"), supervisor.id().to_string()).unwrap();
    supervisor.wait().unwrap();
}

#[test]
fn supervisor_runs_simulator_and_leaves_the_trace_for_in_process_analysis() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path();
    let elf = path.join("program.elf");
    let vcd = path.join("trace.vcd");
    fs::write(&elf, b"ELF").unwrap();
    fs::write(path.join("trace.vcd.src"), TRACE_VCD).unwrap();
    script(&path.join("simulator"), "cp trace.vcd.src \"$2\"");
    let mut child = spawn_supervisor(path, &elf, &path.join("simulator"), &vcd, &["{elf}", "{vcd}"]);
    let _liveness = child.stdin.take().unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    // The supervisor only runs the simulator; the parent analyzes the VCD in-process.
    assert!(vcd.is_file());
    assert_eq!(fs::read_to_string(&vcd).unwrap(), TRACE_VCD);
}

#[test]
fn supervisor_reports_missing_trace_as_measurement_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path();
    let elf = path.join("program.elf");
    let vcd = path.join("trace.vcd");
    fs::write(&elf, b"ELF").unwrap();
    script(&path.join("simulator"), "exit 0");
    let mut child = spawn_supervisor(path, &elf, &path.join("simulator"), &vcd, &["{elf}", "{vcd}"]);
    let _liveness = child.stdin.take().unwrap();
    let result = child.wait_with_output().unwrap();
    assert_eq!(result.status.code(), Some(11));
    assert!(String::from_utf8_lossy(&result.stderr).contains("did not produce VCD"));
}

#[test]
fn supervisor_runs_real_chipyard_cycle_limit_when_configured() {
    let Some(simulator) = std::env::var_os("CHIPYARD_TEST_SIMULATOR") else {
        return;
    };
    let elf = std::env::var_os("CHIPYARD_TEST_ELF").expect("CHIPYARD_TEST_ELF must be set");
    let ini =
        std::env::var("CHIPYARD_TEST_DRAMSIM_INI").expect("CHIPYARD_TEST_DRAMSIM_INI must be set");
    let dir = tempfile::tempdir().unwrap();
    let elf_path = PathBuf::from(&elf);
    let vcd = dir.path().join("trace.vcd");
    let mut child = spawn_supervisor(
        dir.path(),
        &elf_path,
        &PathBuf::from(&simulator),
        &vcd,
        &[
            "+permissive",
            "+dramsim",
            &format!("+dramsim_ini_dir={ini}"),
            "+max-cycles=1000",
            "+loadmem={elf}",
            "+vcdfile={vcd}",
            "+permissive-off",
            "{elf}",
        ],
    );
    let _liveness = child.stdin.take().unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "supervisor failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    // The cycle-limit VCD is left in place for the parent's in-process analysis.
    assert!(vcd.is_file() && vcd.metadata().unwrap().len() > 0);
    assert!(!dir.path().join("core").exists());
}

#[test]
fn supervisor_cleans_up_after_parent_is_killed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path();
    let elf = path.join("program.elf");
    let vcd = path.join("trace.vcd");
    fs::write(&elf, b"ELF").unwrap();
    fs::write(path.join("trace.vcd.src"), TRACE_VCD).unwrap();
    // The simulator writes the trace, then leaks a background child and waits for it.
    script(
        &path.join("simulator"),
        "cp trace.vcd.src \"$2\"; (sleep 1; echo leaked > marker) & wait",
    );
    let mut parent = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "parent_process_helper"])
        .env("SIMULATION_PARENT_HELPER", path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_for(|| vcd.is_file());
    parent.kill().unwrap();
    parent.wait().unwrap();
    thread::sleep(Duration::from_millis(1200));
    assert!(
        !path.join("marker").exists(),
        "simulation child survived parent death"
    );
}
