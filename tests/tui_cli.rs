#![cfg(feature = "tui")]

use std::process::{Command, Output, Stdio};

fn tui(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_fin-tui"))
        .args(args)
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .stdin(Stdio::null())
        .output()
        .expect("run terminal executable")
}

#[test]
fn help_version_and_completions_work_without_a_terminal_or_display() {
    let help = tui(&["--help"]);
    assert!(help.status.success());
    let text = String::from_utf8(help.stdout).expect("UTF-8 help");
    assert!(text.contains("--snapshot"));
    assert!(text.contains("--completions"));
    assert!(!text.contains('\u{1b}'));
    let version = tui(&["--version"]);
    assert!(version.status.success());
    assert!(String::from_utf8_lossy(&version.stdout).contains(env!("CARGO_PKG_VERSION")));
    for shell in ["bash", "zsh", "fish"] {
        let completion = tui(&["--completions", shell]);
        assert!(completion.status.success(), "{shell}");
        assert!(String::from_utf8_lossy(&completion.stdout).contains("fin-tui"));
    }
}

#[test]
fn snapshot_opens_shared_mermaid_files_and_prints_plain_text() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("terminal example.mmd");
    let source = "flowchart LR\nA[Terminal] --> B[Desktop]\n";
    std::fs::write(&path, source).expect("source file");
    let result = tui(&["--snapshot", "120x36", path.to_str().expect("UTF-8 path")]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let text = String::from_utf8(result.stdout).expect("UTF-8 snapshot");
    assert!(text.contains("Terminal"));
    assert!(text.contains("Desktop"));
    assert!(
        !text.contains('\u{1b}'),
        "snapshot must not emit terminal control codes"
    );
    assert_eq!(text.lines().count(), 36);
    assert_eq!(
        std::fs::read_to_string(path).expect("source preserved"),
        source
    );
}

#[test]
fn invalid_invocations_fail_without_entering_terminal_mode() {
    for args in [vec![], vec!["--snapshot", "0x0"], vec!["--unknown-option"]] {
        let result = tui(&args);
        assert!(!result.status.success(), "{args:?}");
        assert!(result.stdout.is_empty());
        assert!(!result.stderr.contains(&0x1b));
    }
    let directory = tempfile::tempdir().expect("directory");
    let missing = directory.path().join("missing.mmd");
    let result = tui(&["--snapshot", "80x24", missing.to_str().expect("path")]);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("Could not open"));
}

#[cfg(feature = "desktop")]
#[test]
fn desktop_launcher_dispatches_to_tui_without_initializing_slint() {
    let output = Command::new(env!("CARGO_BIN_EXE_fin"))
        .args(["--tui", "--version"])
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .stdin(Stdio::null())
        .output()
        .expect("desktop launcher");
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("fin-tui"));
}
