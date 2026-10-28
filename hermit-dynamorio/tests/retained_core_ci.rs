/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 * This source code is licensed under the BSD-style license found in LICENSE.
 */

//! Retained physical core-CI consumers, now owned by hermit-dynamorio.
//! Original test IDs, ignore reasons, guest bytes, thresholds and assertions
//! are preserved. Selection uses the dedicated binary; absent artifacts fail
//! through its actual adapter. There is no unavailable-success branch.

#[path = "support/hermit_binary.rs"]
mod hermit_test;
#[path = "support/retained_run2_log.rs"]
mod run2_log;
mod support;

use std::fs;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::OnceLock;
use std::thread;
use std::time::Duration;
use support::append_hermit_args_using_outer_mount as append_hermit_args;
use support::{assert_success, hermit, hermit_command, stderr};
const ISOLATED_WORKDIR_ENV: &str = "HERMIT_E2E_EMPTY_WORKDIR";
static DBT_UNSUPPORTED_SYSCALL_GUEST: OnceLock<PathBuf> = OnceLock::new();
static DBT_STDERR_GUEST: OnceLock<PathBuf> = OnceLock::new();
static DBT_LOG_ENV_GUEST: OnceLock<PathBuf> = OnceLock::new();
static DBT_EXEC_FAILURE_GUEST: OnceLock<PathBuf> = OnceLock::new();

fn process_build_root(name: &str) -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(name)
        .join("build")
        .join(std::process::id().to_string())
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("hermit stdout should be UTF-8")
}

fn read_terminal_dbt_verdict(path: &Path) -> serde_json::Value {
    let verdict: serde_json::Value =
        serde_json::from_slice(&fs::read(path).expect("failed to read DBT verification verdict"))
            .expect("DBT verification verdict should be JSON");
    let matched = verdict["verdict"] == "matched";
    assert!(
        matched || verdict["verdict"] == "diverged",
        "verification did not reach a terminal comparison: {verdict}"
    );
    assert_eq!(
        verdict["verified"], matched,
        "unexpected verdict: {verdict}"
    );
    assert_eq!(
        verdict["bitwise_parity"], matched,
        "unexpected verdict: {verdict}"
    );
    assert_eq!(
        verdict["comparison"]["strictness"], "canonical",
        "unexpected verdict: {verdict}"
    );
    assert_eq!(
        verdict["comparison"]["log_scope"], "info",
        "unexpected verdict: {verdict}"
    );
    // The DBT log carries every authenticated record, initialization records
    // included at their stream positions, so the verdict names all_records_v1.
    assert_eq!(
        verdict["comparison"]["record_envelope"], "all_records_v1",
        "unexpected verdict: {verdict}"
    );
    assert_eq!(
        verdict["guest_exit_code"], 0,
        "guest rejected its environment: {verdict}"
    );
    for side in ["left", "right"] {
        assert!(
            verdict["compared_log_messages"][side]
                .as_u64()
                .is_some_and(|count| count > 0),
            "empty {side} INFO population: {verdict}"
        );
    }
    verdict
}

fn output_capturing_run2_log(
    command: &mut Command,
    logs: &Path,
    capture: &Path,
) -> (Output, Option<PathBuf>) {
    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to start the verify command");
    let waiter = thread::spawn(move || child.wait_with_output());
    let mut captured = false;
    while !waiter.is_finished() {
        if !captured {
            captured = run2_log::link_run2_log(logs, capture);
        }
        thread::sleep(Duration::from_millis(10));
    }
    let output = waiter
        .join()
        .expect("the verify command's waiter panicked")
        .expect("failed to wait for the verify command");
    // The polls can miss the whole run. A run 2 log that Hermit retained, as
    // it does after a divergence, is still there to link.
    if !captured {
        captured = run2_log::link_run2_log(logs, capture);
    }
    (output, captured.then(|| capture.to_owned()))
}

fn dbt_unsupported_syscall_guest() -> &'static Path {
    DBT_UNSUPPORTED_SYSCALL_GUEST.get_or_init(|| {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("hermit-cli should be inside the repository");
        let build_root = process_build_root("dbt-unsupported-syscall");
        fs::create_dir_all(&build_root)
            .expect("failed to create DBT unsupported-syscall guest directory");
        let guest = build_root.join("dbt_unsupported_syscall");
        let output =
            Command::new("cc")
                .args(["-O0", "-g", "-Wall", "-Wextra", "-Werror"])
                .arg(repository.join(
                    "hermit-dynamorio/tests/retained-core-fixtures/dbt_unsupported_syscall.c",
                ))
                .arg("-o")
                .arg(&guest)
                .output()
                .expect("failed to compile DBT unsupported-syscall guest");
        assert!(
            output.status.success(),
            "DBT unsupported-syscall guest compilation failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        guest
    })
}

fn dbt_stderr_guest() -> &'static Path {
    DBT_STDERR_GUEST.get_or_init(|| {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("hermit-cli should be inside the repository");
        let build_root = process_build_root("dbt-stderr-nostdlib");
        fs::create_dir_all(&build_root).expect("failed to create DBT stderr guest directory");
        let guest = build_root.join("stderr_nostdlib");
        let output = Command::new("cc")
            .args([
                "-nostdlib",
                "-static",
                "-fno-pie",
                "-no-pie",
                "-Wall",
                "-Wextra",
                "-Werror",
            ])
            .arg(repository.join("hermit-dynamorio/tests/retained-core-fixtures/stderr_nostdlib.c"))
            .arg("-o")
            .arg(&guest)
            .output()
            .expect("failed to compile DBT stderr guest");
        assert!(
            output.status.success(),
            "DBT stderr guest compilation failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        guest
    })
}

fn dbt_log_env_guest() -> &'static Path {
    DBT_LOG_ENV_GUEST.get_or_init(|| {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("hermit-cli should be inside the repository");
        let build_root = process_build_root("dbt-hermit-log-env");
        fs::create_dir_all(&build_root).expect("failed to create DBT log-env guest directory");
        let guest = build_root.join("hermit_log_env");
        let output = Command::new("cc")
            .args(["-O2", "-Wall", "-Wextra", "-Werror"])
            .arg(repository.join("hermit-dynamorio/tests/retained-core-fixtures/hermit_log_env.c"))
            .arg("-o")
            .arg(&guest)
            .output()
            .expect("failed to compile DBT log-env guest");
        assert!(
            output.status.success(),
            "DBT log-env guest compilation failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        guest
    })
}

fn dbt_exec_failure_guest() -> &'static Path {
    DBT_EXEC_FAILURE_GUEST.get_or_init(|| {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("hermit-cli should be inside the repository");
        let build_root = process_build_root("dbt-exec-failure");
        fs::create_dir_all(&build_root).expect("failed to create DBT exec-failure guest directory");
        let guest = build_root.join("dbt_exec_failure");
        let output = Command::new("cc")
            .args(["-O0", "-g", "-Wall", "-Wextra", "-Werror"])
            .arg(
                repository.join("hermit-dynamorio/tests/retained-core-fixtures/dbt_exec_failure.c"),
            )
            .arg("-o")
            .arg(&guest)
            .output()
            .expect("failed to compile DBT exec-failure guest");
        assert!(
            output.status.success(),
            "DBT exec-failure guest compilation failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        guest
    })
}

#[test]
#[ignore = "requires the pinned-root isolation validation node"]
fn run_dbt_binds_in_a_user_namespace_of_its_own() {
    let directory = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    fs::write(directory.path().join("input"), "bound input\n").unwrap();
    let bind = format!("{}:/tmp/e2e/bound", directory.path().display());
    let mut command = hermit_command(&[
        "run",
        "--bind",
        &bind,
        "--",
        "/bin/sh",
        "-c",
        "cat /tmp/e2e/bound/input; cat /proc/self/uid_map",
    ]);
    // Hermit must work without CAP_SYS_ADMIN, the case its user namespace is
    // for: drop it from the bounding set, so even a root caller (the pinned
    // root) execs Hermit without it. An unprivileged caller cannot drop it and
    // never had it.
    // SAFETY: prctl is async-signal-safe and touches only this child.
    unsafe {
        command.pre_exec(|| {
            const CAP_SYS_ADMIN: libc::c_ulong = 21;
            libc::prctl(libc::PR_CAPBSET_DROP, CAP_SYS_ADMIN, 0, 0, 0);
            Ok(())
        });
    }
    let output = hermit_test::hermit_output(&format!("{command:?}"), &mut command);
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    let mut lines = text.lines();
    assert_eq!(lines.next(), Some("bound input"), "{text}");
    // The mounts --bind needs come from a user namespace of the DBT adapter's
    // own, which maps root to the caller alone (as reverie's Container::map_root
    // does for every other backend), not from a privileged host. A guest left in
    // the caller's namespace would show the caller's own map instead, whether
    // that is the host's full range or a container's map.
    let guest_map = lines.next().unwrap_or_default();
    let map = guest_map.split_whitespace().collect::<Vec<_>>();
    let euid = unsafe { libc::geteuid() }.to_string();
    assert_eq!(map, ["0", euid.as_str(), "1"], "{text}");
    let caller_map = fs::read_to_string("/proc/self/uid_map").unwrap();
    assert_ne!(
        caller_map.split_whitespace().collect::<Vec<_>>(),
        map,
        "the guest shares the caller's user namespace: {text}"
    );
    assert_eq!(lines.next(), None, "{text}");
}

#[test]
#[ignore = "requires the pinned-root isolation validation node and its /test marker"]
fn run_dbt_verifies_fresh_physical_workdirs() {
    assert_eq!(std::env::var_os(ISOLATED_WORKDIR_ENV), Some("/test".into()));
    let parent_cwd = std::env::current_dir().unwrap();
    let parent_namespace = fs::read_link("/proc/self/ns/mnt").unwrap();
    let directory = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .unwrap()
        .keep();
    println!("DBT physical-workdir evidence: {}", directory.display());
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("test-resources/exclusive_workdir.c");
    let guest = directory.join("exclusive_workdir");
    let compile = Command::new("cc")
        .args(["-O2", "-Wall", "-Wextra", "-Werror"])
        .arg(source)
        .arg("-o")
        .arg(&guest)
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    assert!(!Path::new("/test/physical-run-exclusive").exists());
    let verify = |attempt: usize| {
        let logs = directory.join(format!("verify-{attempt}"));
        fs::create_dir(&logs).unwrap();
        let verdict = directory.join(format!("verify-{attempt}.json"));
        let mut command = hermit_command(&[
            "--log",
            "info",
            "run",
            "--strict",
            "--verify",
            "--keep-logs",
            "--verify-log-dir",
            logs.to_str().unwrap(),
            "--verify-json",
            verdict.to_str().unwrap(),
            "--",
            guest.to_str().unwrap(),
        ]);
        let output = hermit_test::hermit_output(&format!("{command:?}"), &mut command);
        assert!(
            output.status.success(),
            "verify {attempt}: {}",
            stderr(&output)
        );
        assert_eq!(stdout(&output), "empty per-run workdir verified\n");
        let report = read_terminal_dbt_verdict(&verdict);
        assert_eq!(report["verdict"], "matched", "{report}");
        assert_eq!(report["comparison"]["compare_io_buffers"], true, "{report}");
        assert_eq!(report["comparison"]["compare_logs"], true, "{report}");
        assert_eq!(
            report["compared_log_messages"]["left"], report["compared_log_messages"]["right"],
            "{report}"
        );
        // After a match `--keep-logs` retains only run 1's log, the golden
        // log; run 2's log, which matched it, is deleted.
        let captures = |side: &str| {
            fs::read_dir(&logs)
                .unwrap()
                .map(Result::unwrap)
                .filter(|entry| entry.file_name().to_string_lossy().starts_with(side))
                .collect::<Vec<_>>()
        };
        let golden = captures("run1_log_");
        assert_eq!(golden.len(), 1);
        assert!(golden[0].metadata().unwrap().len() > 0);
        assert!(
            captures("run2_log_").is_empty(),
            "verify {attempt}: a matched verification must not retain run 2's log"
        );
    };
    // Each command performs the original strict two-run comparison. Reusing
    // either a physical-run directory or a sibling's directory makes O_EXCL
    // fail without changing the comparator or the guest workload.
    verify(0);
    verify(1);
    std::thread::scope(|scope| {
        let first = scope.spawn(|| verify(2));
        let second = scope.spawn(|| verify(3));
        first.join().unwrap();
        second.join().unwrap();
    });
    assert_eq!(std::env::current_dir().unwrap(), parent_cwd);
    assert_eq!(
        fs::read_link("/proc/self/ns/mnt").unwrap(),
        parent_namespace
    );
    assert!(!Path::new("/test/physical-run-exclusive").exists());
}

#[test]
fn run_dbt_fails_closed_by_default_and_opt_out_aggregates_unsupported_syscalls() {
    let program = dbt_unsupported_syscall_guest()
        .to_str()
        .expect("DBT unsupported-syscall guest path should be UTF-8");

    // Positive bracket: fail-closed changes only unsupported behavior. A supported
    // guest still succeeds through the same default DBT front door.
    let supported_args = ["run", "--", "/bin/echo", "dbt-supported-ok"];
    let supported = hermit(&supported_args);
    assert_success(&supported, &supported_args);
    assert_eq!(stdout(&supported), "dbt-supported-ok\n");

    // Negative bracket: the real unsupported restart_syscall must fail and name
    // itself before the guest can publish its former success marker.
    let default_args = ["run", "--", program];
    let default = hermit(&default_args);
    assert!(
        !default.status.success(),
        "default DBT unexpectedly allowed an unsupported syscall:\n{}",
        stderr(&default)
    );
    assert!(
        stderr(&default).contains("unsupported syscall: restart_syscall"),
        "default DBT failure omitted unsupported syscall:\n{}",
        stderr(&default)
    );
    assert_eq!(
        stdout(&default),
        "",
        "unsupported guest published its success marker"
    );

    let normal_args = [
        "run",
        "--allow-unsupported-syscalls",
        "--verify",
        "--",
        program,
    ];
    let normal = hermit(&normal_args);
    assert_success(&normal, &normal_args);
    assert_eq!(stdout(&normal), "dbt-unsupported-ok\n");
    let normal_stderr = stderr(&normal);
    let opt_out_warning = "a successful exit does not establish complete deterministic execution";
    assert_eq!(
        normal_stderr.matches(opt_out_warning).count(),
        1,
        "compatibility opt-out warning missing or duplicated:\n{normal_stderr}"
    );
    let warning = "syscalls restart_syscall used but not yet supported";
    assert_eq!(
        normal_stderr.matches(warning).count(),
        1,
        "expected one aggregate warning:\n{normal_stderr}"
    );

    let tamper_args = [
        "run",
        "--allow-unsupported-syscalls",
        "--",
        program,
        "report-tamper",
    ];
    let tamper = hermit(&tamper_args);
    assert_success(&tamper, &tamper_args);
    assert_eq!(stdout(&tamper), "dbt-unsupported-report-tamper-ok\n");
    assert_eq!(
        stderr(&tamper).matches(warning).count(),
        1,
        "report tampering suppressed the aggregate warning:\n{}",
        stderr(&tamper)
    );

    let fork_tamper_args = [
        "run",
        "--allow-unsupported-syscalls",
        "--",
        program,
        "fork-report-tamper",
    ];
    let fork_tamper = hermit(&fork_tamper_args);
    assert_success(&fork_tamper, &fork_tamper_args);
    assert_eq!(
        stdout(&fork_tamper),
        "dbt-unsupported-fork-report-tamper-ok\n"
    );
    assert_eq!(
        stderr(&fork_tamper).matches(warning).count(),
        1,
        "fork-child report tampering suppressed the aggregate warning:\n{}",
        stderr(&fork_tamper)
    );

    let strict_args = ["run", "--strict", "--", program];
    let strict = hermit(&strict_args);
    assert!(
        !strict.status.success(),
        "strict DBT unexpectedly succeeded:\n{}",
        stderr(&strict)
    );
    assert!(
        stderr(&strict).contains("unsupported syscall: restart_syscall"),
        "strict DBT failure omitted unsupported syscall:\n{}",
        stderr(&strict)
    );
    let normal_fork_args = [
        "run",
        "--allow-unsupported-syscalls",
        "--verify",
        "--",
        program,
        "fork",
    ];
    let normal_fork = hermit(&normal_fork_args);
    assert_success(&normal_fork, &normal_fork_args);
    assert_eq!(stdout(&normal_fork), "dbt-unsupported-fork-ok\n");
    assert_eq!(
        stderr(&normal_fork).matches(warning).count(),
        1,
        "fork-child warning was not aggregated exactly once:\n{}",
        stderr(&normal_fork)
    );

    let normal_fork_exec_args = [
        "run",
        "--allow-unsupported-syscalls",
        "--verify",
        "--",
        program,
        "fork-exec",
    ];
    let normal_fork_exec = hermit(&normal_fork_exec_args);
    assert_success(&normal_fork_exec, &normal_fork_exec_args);
    assert_eq!(
        stdout(&normal_fork_exec),
        "dbt-unsupported-exec-ok\ndbt-unsupported-fork-exec-parent-ok\n"
    );
    assert_eq!(
        stderr(&normal_fork_exec).matches(warning).count(),
        1,
        "fork-exec warning was not aggregated exactly once:\n{}",
        stderr(&normal_fork_exec)
    );

    for mode in ["fork", "fork-exec", "fork-setsid-exec", "exec-empty"] {
        let args = ["run", "--strict", "--", program, mode];
        let output = hermit(&args);
        assert!(
            !output.status.success(),
            "strict DBT {mode} unexpectedly succeeded:\n{}",
            stderr(&output)
        );
        assert!(
            stderr(&output).contains("unsupported syscall"),
            "strict DBT {mode} omitted unsupported-syscall diagnostic:\n{}",
            stderr(&output)
        );
    }
}

#[test]
fn run_dbt_strict_returns_with_blocked_stdin_source() {
    let program = dbt_unsupported_syscall_guest()
        .to_str()
        .expect("DBT unsupported-syscall guest path should be UTF-8");
    let mut source = Command::new("sleep")
        .arg("30")
        .stdout(Stdio::piped())
        .spawn()
        .expect("failed to start blocked DBT stdin source");
    let args = ["run", "--strict", "--", program];
    let mut command = Command::new("timeout");
    command
        .args(["--kill-after", "2s", "10s"])
        .arg(env!("CARGO_BIN_EXE_hermit-dynamorio"));
    append_hermit_args(&mut command, &args);
    let output = hermit_test::hermit_output(
        &format!("{command:?}"),
        command.stdin(source.stdout.take().expect("sleep stdout was not piped")),
    );
    let _ = source.kill();
    let _ = source.wait();
    assert_ne!(output.status.code(), Some(124), "strict DBT hung on stdin");
    assert!(
        !output.status.success(),
        "strict DBT unexpectedly succeeded"
    );
    assert!(stderr(&output).contains("unsupported syscall"));
}

#[test]
fn run_dbt_keeps_diagnostics_out_of_guest_stderr() {
    let program = dbt_stderr_guest()
        .to_str()
        .expect("DBT stderr guest path should be UTF-8");
    let script = r#"set -euo pipefail; output=$("$1" 2>&1); test "$output" = guest-stderr; printf 'isolated=%s\n' "$output""#;
    let args = [
        "--log",
        "INFO",
        "run",
        "--strict",
        "--",
        "/bin/bash",
        "-c",
        script,
        "dbt-stderr-fixture",
        program,
    ];
    let output = hermit(&args);

    assert_success(&output, &args);
    assert_eq!(stdout(&output), "isolated=guest-stderr\n");
    let stderr = stderr(&output);
    assert!(
        stderr.contains("INFO detcore") && stderr.contains("DETLOG [syscall]"),
        "DBT child diagnostics were not emitted:\n{stderr}"
    );
    assert!(
        !stderr.contains("guest-stderr"),
        "guest fd 2 leaked into controller diagnostics:\n{stderr}"
    );

    // Reported verification transports controller evidence out of band and must
    // not overwrite the guest's own HERMIT_LOG value.  The canonical comparator
    // can currently diverge on run-specific DBT process IDs, so this test owns
    // the environment/evidence contract, not that separate determinism defect.
    for (guest_value, expected) in [
        (None, "<unset>"),
        (Some("guest-sentinel"), "guest-sentinel"),
    ] {
        let directory = tempfile::Builder::new()
            .prefix("dbt-log-env-verify-")
            .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
            .expect("failed to create DBT log-env verification directory")
            .keep();
        eprintln!(
            "DBT log-env verification artifacts retained at {}",
            directory.display()
        );
        let logs = directory.join("logs");
        fs::create_dir(&logs).expect("failed to create DBT log-env verification log directory");
        let verdict = directory.join("verdict.json");
        let expected_arg = format!("expect={expected}");
        let mut command = Command::new(env!("CARGO_BIN_EXE_hermit-dynamorio"));
        command
            .args([
                "--log",
                "INFO",
                "run",
                "--strict",
                "--base-env=host",
                "--verify",
                "--verify-strict",
                "--keep-logs",
                "--verify-log-dir",
            ])
            .arg(&logs)
            .arg("--verify-json")
            .arg(&verdict);
        // Use the normal epoch/workdir setup, while preserving the host-based
        // environment that this test deliberately varies through HERMIT_LOG.
        append_hermit_args(&mut command, &["--"]);
        command
            .arg(dbt_log_env_guest())
            .arg(&expected_arg)
            .env_remove("HERMIT_LOG");
        if let Some(value) = guest_value {
            command.env("HERMIT_LOG", value);
        }
        let capture = directory.join("captured-run2.log");
        let (output, run2_log) = output_capturing_run2_log(&mut command, &logs, &capture);
        let report = read_terminal_dbt_verdict(&verdict);
        assert_eq!(
            output.status.success(),
            report["verified"] == true,
            "process status disagrees with terminal verdict: {report}"
        );
        let mut retained_logs = fs::read_dir(&logs)
            .expect("failed to read retained DBT log-env verification logs")
            .map(|entry| entry.expect("failed to read retained log entry").path())
            .collect::<Vec<_>>();
        retained_logs.sort();
        // `--keep-logs` keeps only run 1's golden log after a match, and both
        // logs after a divergence.
        let expected_sides: &[&str] = if report["verdict"] == "matched" {
            &["run1_log_"]
        } else {
            &["run1_log_", "run2_log_"]
        };
        assert_eq!(
            retained_logs.len(),
            expected_sides.len(),
            "unexpected logs for verdict {}: {retained_logs:?}",
            report["verdict"]
        );
        for (log, side) in retained_logs.iter().zip(expected_sides) {
            assert!(
                log.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(side)),
                "expected a {side} capture, found {log:?}"
            );
        }
        // The helper links run 2's log while the command runs, and once more
        // after it exits, which finds the log a divergence retains. A missing
        // capture fails here whatever the verdict.
        let run2_log = run2_log.expect("run 2's log was not captured while the command ran");
        // After a match Hermit deletes run 2's log, so the link made while the
        // command ran is checked in its place. The match compares only INFO
        // records, and leaked guest stdout could be in a record of another level.
        let mut checked_logs = retained_logs;
        if report["verdict"] == "matched" {
            checked_logs.push(run2_log.clone());
        }
        for log in checked_logs {
            let contents = fs::read_to_string(&log).expect("failed to read DBT verification log");
            assert!(contents.contains("INFO detcore"), "empty INFO log: {log:?}");
            assert!(
                !contents.contains("hermit_log="),
                "guest stdout leaked into DBT diagnostics: {log:?}"
            );
        }
        fs::remove_file(run2_log).expect("failed to remove run 2's checked log");
    }
}

#[test]
fn run_dbt_verifies_pipe_backpressure() {
    let args = [
        "run",
        "--verify",
        "--",
        "/bin/bash",
        "-c",
        r#"{ printf "%4096s" x; for _ in {1..100000}; do :; done; printf "%1371s" y; } | wc -c"#,
    ];
    let output = hermit(&args);

    assert_success(&output, &args);
    assert_eq!(stdout(&output), "5467\n");
    assert!(
        stderr(&output).contains(":: Success: deterministic. Determinism verified."),
        "DBT determinism confirmation missing:\n{}",
        stderr(&output),
    );
}

#[test]
fn run_dbt_recovers_after_failed_exec() {
    let program = dbt_exec_failure_guest()
        .to_str()
        .expect("DBT exec-failure guest path should be UTF-8");
    let directory = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .expect("failed to create DBT exec lifecycle test directory");
    for exec_self in [false, true] {
        let verdict_path = directory.path().join(format!("exec-{exec_self}.json"));
        let mut args = vec![
            "run",
            "--strict",
            "--verify",
            "--verify-strict",
            "--verify-json",
            verdict_path.to_str().expect("verdict path should be UTF-8"),
            "--",
            program,
        ];
        if exec_self {
            args.push("--exec-self");
        }
        let output = hermit(&args);

        assert_success(&output, &args);
        let expected = if exec_self {
            "recovered after failed exec\nsuccessful exec after failed exec\n"
        } else {
            "recovered after failed exec\n"
        };
        assert_eq!(stdout(&output), expected);
        let report = read_terminal_dbt_verdict(&verdict_path);
        assert_eq!(report["verdict"], "matched", "{report}");
    }
}
