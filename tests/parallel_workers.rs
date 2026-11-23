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
        "--analyzer-path".into(),
        root.join("analyzer").as_os_str().to_os_string(),
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
    executable(&root.join("simulator"), "echo wave > \"$2\"");
    // Worker 0 always scores 0.25; others score 0.75 on mutated inputs.
    executable(
        &root.join("analyzer"),
        r#"
case "$PWD" in
  */worker-0000/sim) c=0.25 ;;
  *) c=0.75 ;;
esac
printf 'toggle_activity=12\ntoggle_coverage=%s\n' "$c"
"#,
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
    while !fs::read_to_string(&global).is_ok_and(|s| s.contains("toggle_coverage=0.75\n"))
        && start.elapsed() < Duration::from_secs(20)
    {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    let success = fs::read_to_string(&global).is_ok_and(|s| s.contains("toggle_coverage=0.75\n"));
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
    let analyzer = root.join("analyzer");
    executable(
        &sim,
        r#"
printf '%s' "$1" > elf-path
echo ready > "$3/$(basename "$PWD").ready"
while [ ! -e "$3/sim-0.ready" ] || [ ! -e "$3/sim-1.ready" ]; do sleep 0.02; done
echo wave > "$2"
"#,
    );
    executable(
        &analyzer,
        "printf 'toggle_activity=12\\ntoggle_coverage=0.5\\n'",
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
        .arg("--analyzer-path")
        .arg(&analyzer)
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
