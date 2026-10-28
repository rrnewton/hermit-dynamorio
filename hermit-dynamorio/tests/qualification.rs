/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Maintained full Detcore qualification consumers, retargeted to the extension.
//! Original test IDs and every post-launch assertion remain unchanged.
//! This target has no unavailable-success branch. Missing artifacts fail it.

#[path = "support/hermit_binary.rs"]
mod hermit_test;
mod inherited_terminal_read;
mod native_parity;
#[path = "../src/placement_probe.rs"]
mod placement_probe;
mod support;

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;

use support::append_hermit_args_using_outer_mount;
use support::assert_success;
use support::hermit;
use support::hermit_command;
use support::hermit_run_guard;
use support::hermit_with_stdin;
use support::stderr;
use support::strip_ansi_sgr;

const DYNAMORIO_IO_BUFFER_MUTATOR_SOURCE: &str = r#"
#define _XOPEN_SOURCE 700
#include <fcntl.h>
#include <unistd.h>

int main(int argc, char **argv) {
  static const char replacement[16] = {
      66, 66, 66, 66, 66, 66, 66, 66,
      66, 66, 66, 66, 66, 66, 66, 66,
  };
  char observed[16];
  if (argc != 2) return 64;
  int fd = open(argv[1], O_RDWR | O_CLOEXEC);
  if (fd < 0) return 65;
  if (pread(fd, observed, sizeof(observed), 0) != (ssize_t)sizeof(observed)) return 66;
  if (pwrite(fd, replacement, sizeof(replacement), 0) != (ssize_t)sizeof(replacement)) return 67;
  if (fsync(fd) != 0 || close(fd) != 0) return 68;
  return 0;
}
"#;

#[test]
fn run_dynamorio_executes_integrated_backend() {
    let args = ["run", "--", "/bin/true"];
    let output = hermit(&args);
    assert_success(&output, &args);
}

#[test]
fn run_dynamorio_verify_replays_stdin_bytes_and_eof() {
    let _guard = hermit_run_guard();
    let root = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let report_path = root.path().join("verdict.json");
    let args = [
        "run",
        "--strict",
        "--verify",
        "--verify-strict",
        "--verify-json",
        report_path.to_str().unwrap(),
        "--",
        "/bin/cat",
    ];
    // The existing pipe helper closes its writer after these bytes. Cat must
    // read that EOF in both executions; output must be emitted only once.
    let input = b"first\0line\nlast-without-newline\xff";
    let output = hermit_with_stdin(&args, input);
    assert_success(&output, &args);
    assert_eq!(output.stdout, input);
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(report_path).unwrap()).unwrap();
    assert_eq!(report["verdict"], "matched", "{report}");
    assert_eq!(report["bitwise_parity"], true, "{report}");
    assert_eq!(report["comparison"]["strictness"], "canonical");
    assert_eq!(report["comparison"]["compare_io_buffers"], true);
    for side in ["left", "right"] {
        assert_eq!(report["compared_outputs"][side]["exit_code"], 0, "{report}");
        assert_eq!(
            report["compared_outputs"][side]["stdout_bytes"],
            input.len(),
            "{report}"
        );
        assert_eq!(
            report["compared_outputs"][side]["stdout_sha256"],
            detcore::Digest::new(input).to_string(),
            "{report}"
        );
        assert!(
            report["compared_log_messages"][side].as_u64().unwrap() > 0,
            "{report}"
        );
    }
}

#[test]
fn dynamorio_verify_without_json_rejects_io_buffer_content_divergence() {
    let _guard = hermit_run_guard();
    let build_root = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .expect("failed to create DYNAMORIO io-buffer test directory");
    let source = build_root.path().join("io_buffer_mutator.c");
    let guest = build_root.path().join("io_buffer_mutator");
    fs::write(&source, DYNAMORIO_IO_BUFFER_MUTATOR_SOURCE)
        .expect("failed to write DYNAMORIO io-buffer mutator source");
    let compile = Command::new("cc")
        .args(["-std=c11", "-O2", "-Wall", "-Wextra", "-Werror"])
        .arg(&source)
        .arg("-o")
        .arg(&guest)
        .output()
        .expect("failed to compile DYNAMORIO io-buffer mutator");
    assert!(
        compile.status.success(),
        "failed to compile DYNAMORIO io-buffer mutator:\n{}",
        String::from_utf8_lossy(&compile.stderr)
    );

    // Outside /tmp: both runs must reach the same host file through the
    // container's private /tmp, as they would under ptrace.
    let state = build_root.path().join("state");
    fs::write(&state, b"AAAAAAAAAAAAAAAA").expect("failed to seed DYNAMORIO io-buffer state");
    let log_dir = build_root.path().join("verify-logs");
    fs::create_dir(&log_dir).expect("failed to create DYNAMORIO verification log directory");
    let mut command = Command::new(env!("CARGO_BIN_EXE_hermit-dynamorio"));
    command
        .args([
            "--log",
            "info",
            "run",
            "--strict",
            "--verify",
            "--keep-logs",
            "--verify-log-dir",
        ])
        .arg(&log_dir);
    append_hermit_args_using_outer_mount(&mut command, &["--"]);
    let output =
        hermit_test::hermit_output(&format!("{command:?}"), command.arg(&guest).arg(&state));
    let _ = fs::remove_file(&state);
    let stderr = strip_ansi_sgr(&stderr(&output));

    assert!(
        !output.status.success(),
        "DYNAMORIO verification without --verify-json accepted an A-to-B syscall output-buffer \
         mutation:\n{stderr}"
    );
    assert!(
        !stderr.contains("Success: deterministic. Determinism verified."),
        "DYNAMORIO announced deterministic success after an output-buffer divergence:\n{stderr}"
    );
    assert!(
        stderr.contains("Failure: nondeterministic."),
        "DYNAMORIO rejected the mutation for a reason other than the two-run comparison:\n{stderr}"
    );
    for marker in [
        "[iobuf]",
        "pread64",
        "= Ok(16)",
        "991204fba2b6216d476282d375ab88d20e6108d109aecded97ef424ddd114706",
        "900dfeb7f1b5e344209e2abce56c333dafe606fb3bf59f68ab2b0e2ef8a0662b",
    ] {
        assert!(
            stderr.contains(marker),
            "DYNAMORIO's no-JSON verification failure did not name {marker:?}; it must fail on the \
             syscall output-buffer evidence itself:\n{stderr}"
        );
    }

    // A divergence keeps both logs under `--keep-logs`, each under the name the
    // harness scans for. This deterministic divergence is what pins run 2's
    // name: after a match only run 1's golden log is retained.
    let captures = |prefix: &str| -> Vec<PathBuf> {
        fs::read_dir(&log_dir)
            .expect("failed to read the retained DYNAMORIO verify-log directory")
            .map(|entry| entry.expect("failed to read a retained log entry").path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(prefix))
            })
            .collect()
    };
    for prefix in ["run1_log_", "run2_log_"] {
        let found = captures(prefix);
        assert_eq!(
            found.len(),
            1,
            "a diverged DYNAMORIO verification must retain exactly one {prefix} capture: {found:?}"
        );
        let size = fs::metadata(&found[0])
            .expect("failed to stat a retained DYNAMORIO capture")
            .len();
        assert!(
            size > 0,
            "retained DYNAMORIO capture {} is empty",
            found[0].display()
        );
    }
    for stale in ["dynamorio-run1_log_", "dynamorio-run2_log_"] {
        assert!(
            captures(stale).is_empty(),
            "DYNAMORIO retained a {stale} capture instead of the name the harness scans for"
        );
    }
}

#[test]
fn run_dynamorio_private_placement_preserves_cli_echo() {
    assert!(
        hermit_dynamorio::test_support::LaunchArtifacts::required().is_ok(),
        "DynamoRIO placement qualification requires its actual shared-engine native cohort"
    );
    let _guard = hermit_run_guard();
    let probe = placement_probe::Probe::new(true);
    let (install, bind) = probe.launch();
    let bind = bind.expect("CLI observation needs its explicit writable bind");
    let args = ["run", bind.as_str(), "--", "/bin/echo", "hello"];
    let mut command = hermit_command(&args);
    command
        .env("DYNAMORIO_TEST_LAUNCH_OBSERVATION", install)
        .env("HERMIT_REQUIRE_DYNAMORIO", "1");
    let output = hermit_test::hermit_output(&format!("hermit {args:?}"), &mut command);
    assert_success(&output, &args);
    assert_eq!(output.stdout, b"hello\n");
    probe.assert_zero(1, false);
}

#[test]
fn run_dynamorio_private_placement_preserves_stdin_verification() {
    assert!(
        hermit_dynamorio::test_support::LaunchArtifacts::required().is_ok(),
        "DynamoRIO placement qualification requires its actual shared-engine native cohort"
    );
    let _guard = hermit_run_guard();
    let probe = placement_probe::Probe::new(true);
    let (install, bind) = probe.launch();
    let bind = bind.expect("CLI observation needs its explicit writable bind");
    let root = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let report_path = root.path().join("verdict.json");
    let args = [
        "run",
        bind.as_str(),
        "--strict",
        "--verify",
        "--verify-strict",
        "--verify-json",
        report_path.to_str().unwrap(),
        "--",
        "/bin/cat",
    ];
    // The existing pipe helper closes its writer after these bytes. Cat must
    // read that EOF in both executions; output must be emitted only once.
    let input = b"first\0line\nlast-without-newline\xff";
    // Same pipe/EOF ownership as hermit_with_stdin, with only child-local staging.
    let output = hermit_test::hermit_output_with(&format!("hermit {args:?}"), || {
        let mut command = hermit_command(&args);
        let mut child = command
            .env("DYNAMORIO_TEST_LAUNCH_OBSERVATION", install)
            .env("HERMIT_REQUIRE_DYNAMORIO", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|error| panic!("failed to run hermit with {args:?}: {error}"));
        child
            .stdin
            .take()
            .expect("hermit stdin should be piped")
            .write_all(input)
            .expect("failed to write hermit stdin");
        let output = child
            .wait_with_output()
            .unwrap_or_else(|error| panic!("failed to wait for hermit with {args:?}: {error}"));
        (output, ())
    })
    .0;
    assert_success(&output, &args);
    assert_eq!(output.stdout, input);
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(report_path).unwrap()).unwrap();
    assert_eq!(report["verdict"], "matched", "{report}");
    assert_eq!(report["bitwise_parity"], true, "{report}");
    assert_eq!(report["comparison"]["strictness"], "canonical");
    assert_eq!(report["comparison"]["compare_io_buffers"], true);
    for side in ["left", "right"] {
        assert_eq!(report["compared_outputs"][side]["exit_code"], 0, "{report}");
        assert_eq!(
            report["compared_outputs"][side]["stdout_bytes"],
            input.len(),
            "{report}"
        );
        assert_eq!(
            report["compared_outputs"][side]["stdout_sha256"],
            detcore::Digest::new(input).to_string(),
            "{report}"
        );
        assert!(
            report["compared_log_messages"][side].as_u64().unwrap() > 0,
            "{report}"
        );
    }
    probe.assert_zero(2, true);
}
