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
