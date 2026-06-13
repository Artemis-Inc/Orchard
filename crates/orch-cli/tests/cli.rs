//! CLI integration tests — invoke the built `orch` binary.

use std::process::Command;

fn orch() -> Command {
    Command::new(env!("CARGO_BIN_EXE_orch"))
}

fn demo() -> String {
    // the demo-offline fixture lives under the orchard crate's tests
    format!(
        "{}/../orchard/tests/scripts/demo-offline.orch",
        env!("CARGO_MANIFEST_DIR")
    )
}

#[test]
fn check_valid_agent_exits_zero() {
    let out = orch().args(["check", &demo()]).output().unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn check_invalid_agent_exits_two() {
    let dir = std::env::temp_dir().join(format!("orch_cli_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let bad = dir.join("bad.orch");
    std::fs::write(
        &bad,
        "agent A { model { provider: bogusprov, name: \"m\" } }",
    )
    .unwrap();
    let out = orch()
        .args(["check", bad.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn run_task_offline() {
    let out = orch()
        .args(["run", &demo(), "-t", "demo"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Ran fully offline"), "stdout: {stdout}");
}

#[test]
fn compile_emits_ir() {
    let out = orch().args(["compile", &demo()]).output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("\"orchard\""));
    assert!(stdout.contains("\"manifest\""));
}

#[test]
fn new_scaffolds_checkable_agent() {
    let dir = std::env::temp_dir().join(format!("orch_new_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out = orch()
        .current_dir(&dir)
        .args(["new", "my-bot"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let path = dir.join("my-bot.orch");
    assert!(path.exists());
    let src = std::fs::read_to_string(&path).unwrap();
    assert!(src.contains("#!orchard 3.0"));
    assert!(src.contains("agent MyBot"));
    // and it checks clean
    let chk = orch()
        .args(["check", path.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(chk.status.success());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn info_lists_handlers() {
    let out = orch().args(["info", &demo()]).output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("agent: Demo"));
    assert!(stdout.contains("mock"));
}
