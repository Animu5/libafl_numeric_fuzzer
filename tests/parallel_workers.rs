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

#[test]
fn generated_workspaces_exchange_inputs_and_report_global_best() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let template = root.join("template");
    fs::create_dir_all(template.join("src")).unwrap();
    fs::write(
        template.join("Makefile"),
        "snippet.elf: src/snippet.S\n\tcp $< $@\n",
    )
    .unwrap();
    let simulator = root.join("simulator");
    let analyzer = root.join("analyzer");
    executable(&simulator, "echo wave > \"$2\"");
    executable(
        &analyzer,
        r#"
case "$PWD" in
  */worker-0000/sim) coverage=0.25 ;;
  *)
    if [ ! -f original-assembly ]; then cp ../build/src/snippet.S original-assembly; fi
    if cmp -s ../build/src/snippet.S original-assembly; then coverage=0.5; else coverage=0.75; fi
    ;;
esac
printf 'toggle_activity=12\ntoggle_coverage=%s\n' "$coverage"
"#,
    );
    let workspace = root.join("workspace");
    let solutions = root.join("solutions");
    let mut child = Command::new(env!("CARGO_BIN_EXE_libafl_fuzzer"))
        .args([
            "--backend",
            "simulation",
            "--workers",
            "2",
            "--worker-workspace",
        ])
        .arg(&workspace)
        .arg("--assembly-environment")
        .arg(&template)
        .args([
            "--atndecode-path",
            "/bin/cat",
            "--riscv-toolpath",
            "/opt/riscv",
            "--solutions-path",
        ])
        .arg(&solutions)
        .arg("--log-file")
        .arg(root.join("fuzzer.log"))
        .arg("--simulator-path")
        .arg(simulator)
        .arg("--analyzer-path")
        .arg(analyzer)
        .args([
            "--target-scope",
            "TOP.dut",
            "--simulator-arg={elf}",
            "--simulator-arg={vcd}",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    let global = solutions.join("parallel-exchange/global-best.txt");
    let imported = solutions.join("worker-0000/import-0002");
    while !(imported.exists()
        && fs::read_to_string(&global).is_ok_and(|s| s.contains("toggle_coverage=0.75\n")))
        && start.elapsed() < Duration::from_secs(20)
    {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    let success = imported.exists()
        && fs::read_to_string(&global).is_ok_and(|s| s.contains("toggle_coverage=0.75\n"));
    if child.try_wait().unwrap().is_none() {
        child.kill().unwrap();
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        success,
        "workers did not exchange inputs: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(workspace.join("worker-0000/build/snippet.elf").is_file());
    assert!(workspace.join("worker-0001/build/snippet.elf").is_file());
    assert!(!template.join("src/snippet.S").exists());
    let record = fs::read_to_string(global).unwrap();
    let input = record
        .lines()
        .find_map(|s| s.strip_prefix("input="))
        .unwrap();
    assert_eq!(
        fs::read(solutions.join("parallel-exchange").join(input)).unwrap(),
        fs::read(imported).unwrap()
    );
}

#[test]
fn independent_workers_execute_in_parallel_with_isolated_outputs() {
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
            "--backend",
            "simulation",
            "--workers",
            "2",
            "--assembly-environment",
        ])
        .arg(root.join("build-0"))
        .args([
            "--atndecode-path",
            "/bin/cat",
            "--riscv-toolpath",
            "/opt/riscv",
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
        .arg(sim)
        .arg("--analyzer-path")
        .arg(analyzer)
        .args([
            "--target-scope",
            "TOP.dut",
            "--simulator-arg={elf}",
            "--simulator-arg={vcd}",
        ])
        .arg(format!("--simulator-arg={}", root.display()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    let first = solutions.join("worker-0000/best.S");
    let second = solutions.join("worker-0001/best.S");
    while !(first.exists() && second.exists()) && start.elapsed() < Duration::from_secs(10) {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    let success = first.exists() && second.exists();
    if child.try_wait().unwrap().is_none() {
        child.kill().unwrap();
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        success,
        "workers did not finish seeds: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    for i in 0..2 {
        let path = fs::read_to_string(root.join(format!("sim-{i}/elf-path"))).unwrap();
        assert_eq!(
            Path::new(&path),
            root.join(format!("build-{i}/snippet.elf"))
        );
        assert!(root.join(format!("fuzzer.log.worker-{i:04}")).is_file());
    }
}

#[test]
fn launcher_death_stops_workers_and_their_build_processes() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    for i in 0..2 {
        let build = root.join(format!("build-{i}"));
        fs::create_dir_all(build.join("src")).unwrap();
        fs::write(
            build.join("Makefile"),
            "snippet.elf:\n\t@echo ready > started; (sleep 1; echo leaked > leaked) & wait\n",
        )
        .unwrap();
        fs::create_dir(root.join(format!("sim-{i}"))).unwrap();
    }
    let simulator = root.join("simulator");
    let analyzer = root.join("analyzer");
    executable(&simulator, "exit 0");
    executable(&analyzer, "exit 0");
    let mut child = Command::new(env!("CARGO_BIN_EXE_libafl_fuzzer"))
        .args([
            "--backend",
            "simulation",
            "--workers",
            "2",
            "--assembly-environment",
        ])
        .arg(root.join("build-0"))
        .args([
            "--atndecode-path",
            "/bin/cat",
            "--riscv-toolpath",
            "/opt/riscv",
            "--solutions-path",
        ])
        .arg(root.join("solutions"))
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
        .arg(simulator)
        .arg("--analyzer-path")
        .arg(analyzer)
        .args([
            "--target-scope",
            "TOP.dut",
            "--simulator-arg={elf}",
            "--simulator-arg={vcd}",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    while !(root.join("build-0/started").exists() && root.join("build-1/started").exists())
        && start.elapsed() < Duration::from_secs(5)
    {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    let started = root.join("build-0/started").exists() && root.join("build-1/started").exists();
    if child.try_wait().unwrap().is_none() {
        child.kill().unwrap();
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        started,
        "workers did not start: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    thread::sleep(Duration::from_millis(1200));
    assert!(!root.join("build-0/leaked").exists());
    assert!(!root.join("build-1/leaked").exists());
}
