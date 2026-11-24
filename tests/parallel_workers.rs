#![cfg(unix)]

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

fn executable(path: &Path, script: &str) {
    fs::write(path, format!("#!/bin/sh\n{script}\n")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

/// A small VCD with a single 1-bit signal toggling once at #10 (1ns timescale):
/// activity 1e8 toggles/s, coverage 1.0.
const LOW_VCD: &str = "\
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

/// A VCD with ten toggles by #10: activity 1e9 toggles/s, clearly above LOW_VCD.
const HIGH_VCD: &str = "\
$timescale 1ns $end
$scope module TOP $end
$scope module dut $end
$var wire 1 ! value $end
$upscope $end
$upscope $end
$enddefinitions $end
#0
0!
#1
1!
#2
0!
#3
1!
#4
0!
#5
1!
#6
0!
#7
1!
#8
0!
#9
1!
#10
0!
";

fn base_args(root: &Path, template: &Path, workspace: &Path, solutions: &Path) -> Vec<std::ffi::OsString> {
    vec![
        "--backend".into(),
        "simulation".into(),
        "--workers".into(),
        "2".into(),
        "--worker-workspace".into(),
        workspace.as_os_str().to_os_string(),
        "--assembly-environment".into(),
        template.as_os_str().to_os_string(),
        "--atndecode-path".into(),
        "/bin/cat".into(),
        "--riscv-toolpath".into(),
        "/opt/riscv".into(),
        "--solutions-path".into(),
        solutions.as_os_str().to_os_string(),
        "--log-file".into(),
        root.join("fuzzer.log").as_os_str().to_os_string(),
        "--simulator-path".into(),
        root.join("simulator").as_os_str().to_os_string(),
        "--target-scope".into(),
        "TOP.dut".into(),
        "--simulator-arg={elf}".into(),
        "--simulator-arg={vcd}".into(),
        "--monitor".into(),
        "none".into(),
    ]
}

fn make_template(root: &Path) -> std::path::PathBuf {
    let template = root.join("template");
    fs::create_dir_all(template.join("src")).unwrap();
    fs::write(
        template.join("Makefile"),
        "snippet.elf: src/snippet.S\n\tcp $< $@\n",
    )
    .unwrap();
    template
}

#[test]
fn launcher_forks_workers_with_global_best_and_central_log() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let template = make_template(root);
    // The simulator writes a low-activity trace in worker-0000 and a high-activity
    // trace everywhere else; the VCD is analyzed in-process by the fuzzer itself.
    fs::write(root.join("low.vcd"), LOW_VCD).unwrap();
    fs::write(root.join("high.vcd"), HIGH_VCD).unwrap();
    executable(
        &root.join("simulator"),
        &format!(
            r#"case "$PWD" in
  */worker-0000/sim) cp "{}/low.vcd" "$2" ;;
  *) cp "{}/high.vcd" "$2" ;;
esac"#,
            root.display(),
            root.display()
        ),
    );
    let workspace = root.join("workspace");
    let solutions = root.join("solutions");
    let log = root.join("fuzzer.log");
    let mut child = Command::new(env!("CARGO_BIN_EXE_libafl_fuzzer"))
        .args(base_args(root, &template, &workspace, &solutions))
        .env("LLMP_BROKER_PORT", format!("{}", 30000 + std::process::id() % 30000))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null()) // forked children hold the pipe; use the log file instead
        .spawn()
        .unwrap();
    let start = Instant::now();
    let global = solutions.join("parallel-exchange/global-best.txt");
    // Wait for all artifacts, not just the global best: worker-0000 may lag
    // behind worker-0001, whose high-activity trace becomes the global best.
    let ready = || {
        fs::read_to_string(&global)
            .is_ok_and(|s| s.starts_with("toggle_activity=") && s.contains("worker=worker-0001\n"))
            && workspace.join("worker-0000/build/snippet.elf").is_file()
            && workspace.join("worker-0001/build/snippet.elf").is_file()
            && solutions.join("worker-0000/best.S").is_file()
            && solutions.join("worker-0001/best.S").is_file()
    };
    while !ready() && start.elapsed() < Duration::from_secs(60) {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    let success = ready();
    if child.try_wait().unwrap().is_none() {
        child.kill().unwrap();
    }
    let _ = child.wait();
    let log_text = fs::read_to_string(&log).unwrap_or_default();
    assert!(success, "global best not reported; log:\n{log_text}");
    // Both workers built their ELF in their own workspace.
    assert!(workspace.join("worker-0000/build/snippet.elf").is_file());
    assert!(workspace.join("worker-0001/build/snippet.elf").is_file());
    // Template is untouched.
    assert!(!template.join("src/snippet.S").exists());
    // Each worker has its own best.S.
    assert!(solutions.join("worker-0000/best.S").is_file());
    assert!(solutions.join("worker-0001/best.S").is_file());
    // Central log file exists (shared by broker + workers).
    assert!(root.join("fuzzer.log").is_file());
    // global-best.txt points to a real input file.
    let record = fs::read_to_string(&global).unwrap();
    let input = record
        .lines()
        .find_map(|s| s.strip_prefix("input="))
        .unwrap();
    assert!(solutions.join("parallel-exchange").join(input).is_file());
}

#[test]
fn independent_workers_use_isolated_directories() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    for i in 0..2 {
        let build = root.join(format!("build-{i}"));
        fs::create_dir_all(build.join("src")).unwrap();
        fs::write(
            build.join("Makefile"),
            "snippet.elf: src/snippet.S\n\tcp $< $@\n",
        )
        .unwrap();
        fs::create_dir(root.join(format!("sim-{i}"))).unwrap();
    }

    let sim = root.join("simulator");
    fs::write(root.join("trace.vcd"), HIGH_VCD).unwrap();
    executable(
        &sim,
        &format!(
            r#"
printf '%s' "$1" > elf-path
echo ready > "$3/$(basename "$PWD").ready"
while [ ! -e "$3/sim-0.ready" ] || [ ! -e "$3/sim-1.ready" ]; do sleep 0.02; done
cp "{}/trace.vcd" "$2"
"#,
            root.display()
        ),
    );
    let solutions = root.join("solutions");
    let mut child = Command::new(env!("CARGO_BIN_EXE_libafl_fuzzer"))
        .args([
            "--backend", "simulation", "--workers", "2",
            "--assembly-environment",
        ])
        .arg(root.join("build-0"))
        .args([
            "--atndecode-path", "/bin/cat",
            "--riscv-toolpath", "/opt/riscv",
            "--solutions-path",
        ])
        .arg(&solutions)
        .arg("--log-file")
        .arg(root.join("fuzzer.log"))
        .arg("--worker-assembly-environment")
        .arg(root.join("build-0"))
        .arg("--worker-assembly-environment")
        .arg(root.join("build-1"))
        .arg("--worker-simulator-working-dir")
        .arg(root.join("sim-0"))
        .arg("--worker-simulator-working-dir")
        .arg(root.join("sim-1"))
        .arg("--simulator-path")
        .arg(&sim)
        .args(["--target-scope", "TOP.dut", "--simulator-arg={elf}", "--simulator-arg={vcd}"])
        .arg(format!("--simulator-arg={}", root.display()))
        .arg("--monitor")
        .arg("none")
        .env("LLMP_BROKER_PORT", format!("{}", 30001 + std::process::id() % 30000))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let start = Instant::now();
    let first = solutions.join("worker-0000/best.S");
    let second = solutions.join("worker-0001/best.S");
    while !(first.exists() && second.exists()) && start.elapsed() < Duration::from_secs(15) {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    let success = first.exists() && second.exists();
    if child.try_wait().unwrap().is_none() {
        child.kill().unwrap();
    }
    let _ = child.wait();
    let log_text = fs::read_to_string(root.join("fuzzer.log")).unwrap_or_default();
    assert!(success, "workers did not finish seeds; log:\n{log_text}");
    for i in 0..2 {
        let path = fs::read_to_string(root.join(format!("sim-{i}/elf-path"))).unwrap();
        assert_eq!(
            Path::new(&path),
            root.join(format!("build-{i}/snippet.elf"))
        );
    }
}
