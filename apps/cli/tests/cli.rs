// These synchronous subprocess tests need direct control over held-open OS pipes and process exit.
#![allow(clippy::disallowed_types, clippy::disallowed_methods)]

use std::{
    fs,
    io::Write,
    path::PathBuf,
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde_json::Value;

struct TempFile(PathBuf);

impl TempFile {
    fn new(contents: &[u8]) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("jev-cli-test-{}-{nanos}.input", std::process::id()));
        fs::write(&path, contents).unwrap();
        Self(path)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn binary() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_jev"));
    command.env_remove("OPENROUTER_API_KEY");
    command.stdin(Stdio::null());
    command
}

fn run(args: &[&str]) -> Output {
    binary().args(args).output().unwrap()
}

fn parse_output(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn wait_within(mut child: Child, deadline: Duration) -> Output {
    let start = Instant::now();
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if start.elapsed() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("jev did not exit within {deadline:?}");
        }
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn test_should_build_same_request_from_inline_file_and_stdin_state() {
    let state = r#"{"ticket":"Payment failed","attempt":2}"#;
    let inline = parse_output(&run(&[
        "--dry-run",
        "--state-format",
        "json",
        "--state",
        state,
        "--id",
        "triage",
        "noul",
        "Is this a billing bug?",
    ]));

    let file = TempFile::new(state.as_bytes());
    let from_file = parse_output(&run(&[
        "--dry-run",
        "--state-format",
        "json",
        "--state-file",
        file.path().to_str().unwrap(),
        "--id",
        "triage",
        "noul",
        "Is this a billing bug?",
    ]));

    let mut child = binary()
        .args([
            "--dry-run",
            "--state-format",
            "json",
            "--state-file",
            "-",
            "--id",
            "triage",
            "noul",
            "Is this a billing bug?",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(state.as_bytes())
        .unwrap();
    let from_stdin = parse_output(&child.wait_with_output().unwrap());

    assert_eq!(inline, from_file);
    assert_eq!(inline, from_stdin);
}

#[test]
fn test_should_fail_invalid_question_and_missing_credentials_without_reading_stdin() {
    let invalid_question = binary()
        .args([
            "--dry-run",
            "choice",
            "Which category?",
            "--option",
            "only=One option",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let invalid_output = wait_within(invalid_question, Duration::from_secs(2));
    assert_eq!(invalid_output.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&invalid_output.stderr)
            .contains("Choice requires between 2 and 64")
    );
    assert!(invalid_output.stdout.is_empty());

    let missing_key = binary()
        .args(["--input-timeout", "60", "noul", "Is this safe?"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let missing_key_output = wait_within(missing_key, Duration::from_secs(2));
    assert_eq!(missing_key_output.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&missing_key_output.stderr)
            .contains("OPENROUTER_API_KEY is not set")
    );
}

#[test]
fn test_should_exit_after_stalled_stdin_and_file_fifo_deadlines() {
    let stdin_child = binary()
        .args(["--dry-run", "--input-timeout", "1", "noul", "Question?"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    let output = wait_within(stdin_child, Duration::from_secs(4));
    assert_eq!(output.status.code(), Some(2));
    assert!(start.elapsed() < Duration::from_secs(4));
    assert!(String::from_utf8_lossy(&output.stderr).contains("input exceeded"));

    #[cfg(unix)]
    {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let fifo =
            std::env::temp_dir().join(format!("jev-cli-test-{}-{nanos}.fifo", std::process::id()));
        let status = Command::new("mkfifo").arg(&fifo).status().unwrap();
        assert!(status.success());
        let fifo_child = binary()
            .args([
                "--dry-run",
                "--input-timeout",
                "1",
                "--state-file",
                fifo.to_str().unwrap(),
                "noul",
                "Question?",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let output = wait_within(fifo_child, Duration::from_secs(4));
        let _ = fs::remove_file(fifo);
        assert_eq!(output.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&output.stderr).contains("input exceeded"));
    }
}

#[test]
fn test_should_reject_bad_state_json_utf8_and_oversized_files() {
    let malformed = run(&[
        "--dry-run",
        "--state-format",
        "json",
        "--state",
        "{truncated",
        "noul",
        "Question?",
    ]);
    assert_eq!(malformed.status.code(), Some(2));
    assert!(malformed.stdout.is_empty());

    let invalid_utf8 = TempFile::new(&[0xff, 0xfe]);
    let invalid = run(&[
        "--dry-run",
        "--state-file",
        invalid_utf8.path().to_str().unwrap(),
        "noul",
        "Question?",
    ]);
    assert_eq!(invalid.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("not valid UTF-8"));
    assert!(invalid.stdout.is_empty());

    let oversized = TempFile::new(&vec![b'x'; 1_048_577]);
    let output = run(&[
        "--dry-run",
        "--state-file",
        oversized.path().to_str().unwrap(),
        "noul",
        "Question?",
    ]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("1048576-byte limit"));
    assert!(output.stdout.is_empty());
}

#[test]
fn test_should_report_help_and_timeout_ranges_and_quietly_handle_broken_pipe() {
    let help = run(&["--help"]);
    assert!(help.status.success());
    let help = String::from_utf8_lossy(&help.stdout);
    assert!(help.contains("compact one-line or indented output"));
    assert!(help.contains("1 MiB"));
    assert!(help.contains("dry-run"));
    let choice_help = run(&["choice", "--help"]);
    assert!(choice_help.status.success());
    assert!(String::from_utf8_lossy(&choice_help.stdout).contains("2 to 64 options"));
    let score_help = run(&["score", "--help"]);
    assert!(score_help.status.success());
    assert!(String::from_utf8_lossy(&score_help.stdout).contains("2 to 32 times"));

    let bad_timeout = run(&["--input-timeout", "0", "--dry-run", "noul", "Question?"]);
    assert_eq!(bad_timeout.status.code(), Some(2));

    let large_state = TempFile::new(&vec![b'x'; 900_000]);
    let mut child = binary()
        .args([
            "--dry-run",
            "--state-format",
            "text",
            "--state-file",
            large_state.path().to_str().unwrap(),
            "noul",
            "Question?",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
}
