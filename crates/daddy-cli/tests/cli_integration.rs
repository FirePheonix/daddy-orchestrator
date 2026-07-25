use std::path::{Path, PathBuf};
use std::process::Command;

// Return the compiled `daddy` binary path for integration tests.
fn daddy_binary() -> &'static str {
    env!("CARGO_BIN_EXE_daddy")
}

// Create a mock codex executable in the supplied directory for CLI integration tests.
fn write_mock_codex(dir: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let ps1 = dir.join("codex.ps1");
        std::fs::write(
            &ps1,
            r#"$outfile = $null
for ($i = 0; $i -lt $args.Length; $i++) {
  if ($args[$i] -eq '--output-last-message' -and ($i + 1) -lt $args.Length) {
    $outfile = $args[$i + 1]
  }
}
if (-not $outfile) { exit 1 }
Set-Content -LiteralPath $outfile -Value 'Mock answer'
Write-Output '{"type":"thread.started","thread_id":"thread-123"}'
Write-Output '{"type":"turn.completed","usage":{"input_tokens":12,"output_tokens":4},"cost_usd":0.02}'"#,
        )
        .unwrap();
        let path = dir.join("codex.cmd");
        std::fs::write(
            &path,
            r#"@echo off
powershell -ExecutionPolicy Bypass -File "%~dp0codex.ps1" %*
exit /b %ERRORLEVEL%
"#,
        )
        .unwrap();
        path
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("codex");
        std::fs::write(
            &path,
            r#"#!/usr/bin/env sh
outfile=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--output-last-message" ]; then
    outfile="$2"
    shift
  fi
  shift
done
printf '%s\n' 'Mock answer' > "$outfile"
printf '%s\n' '{"type":"thread.started","thread_id":"thread-123"}'
printf '%s\n' '{"type":"turn.completed","usage":{"input_tokens":12,"output_tokens":4},"cost_usd":0.02}'
"#,
        )
        .unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        path
    }
}

// Build a PATH string that puts the mock-provider directory ahead of the existing PATH.
fn prefixed_path(dir: &Path) -> String {
    let current = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![dir.to_path_buf()];
    paths.extend(std::env::split_paths(&current));
    std::env::join_paths(paths)
        .unwrap()
        .to_string_lossy()
        .to_string()
}

// Initialize a temporary Git repository so orchestration tests can create isolated worktrees.
fn init_git_repo(dir: &Path) {
    run_git(dir, &["init"]);
    run_git(dir, &["config", "user.email", "daddy@example.com"]);
    run_git(dir, &["config", "user.name", "Daddy"]);
    std::fs::write(dir.join("README.md"), "seed\n").unwrap();
    run_git(dir, &["add", "README.md"]);
    run_git(dir, &["commit", "-m", "seed"]);
}

// Run one Git command inside the test repository and fail loudly if it does not succeed.
fn run_git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {:?} failed\nstdout: {}\nstderr: {}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
// Execute the compiled CLI against a mocked codex binary and verify the final answer.
fn one_shot_completion_uses_mocked_codex_binary() {
    let dir = tempfile::tempdir().unwrap();
    write_mock_codex(dir.path());
    let output = Command::new(daddy_binary())
        .current_dir(dir.path())
        .env("PATH", prefixed_path(dir.path()))
        .arg("--provider")
        .arg("codex")
        .arg("hello")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Mock answer"));
}

#[test]
// Run the doctor command against a mocked codex binary and verify installed status reporting.
fn doctor_reports_mocked_codex_installation() {
    let dir = tempfile::tempdir().unwrap();
    write_mock_codex(dir.path());
    let output = Command::new(daddy_binary())
        .current_dir(dir.path())
        .env("PATH", prefixed_path(dir.path()))
        .arg("doctor")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("codex"));
    assert!(stdout.contains("installed=true"));
}

#[test]
// Execute an orchestrated run with prepared worktrees and verify the JSON output includes task results.
fn orchestrated_run_executes_tasks_in_prepared_worktrees() {
    let dir = tempfile::tempdir().unwrap();
    write_mock_codex(dir.path());
    init_git_repo(dir.path());
    let output = Command::new(daddy_binary())
        .current_dir(dir.path())
        .env("PATH", prefixed_path(dir.path()))
        .arg("--provider")
        .arg("codex")
        .arg("run")
        .arg("--json")
        .arg("--prepare-worktrees")
        .arg("--execute")
        .arg("Fix")
        .arg("failing")
        .arg("auth")
        .arg("tests")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let worktrees = value
        .get("prepared_workspaces")
        .and_then(|prepared| prepared.get("worktrees"))
        .and_then(serde_json::Value::as_array)
        .unwrap();
    let executed = value
        .get("executed_tasks")
        .and_then(serde_json::Value::as_array)
        .unwrap();
    assert!(!worktrees.is_empty());
    assert!(!executed.is_empty());
    assert!(executed.iter().all(|task| task.get("result").is_some()));
    assert!(executed.iter().all(|task| task.get("eviction").is_some()));
    assert!(executed.iter().all(|task| task.get("handoff").is_some()));
}
