/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Only the maintained subprocess/test-fixture helpers needed by these cases.
//! No application, verification comparator, record writer, or argument shim.

use std::ffi::OsStr;
use std::ffi::OsString;
use std::io::Write;
use std::process::Command;
use std::process::Output;
use std::process::Stdio;
use std::sync::Mutex;
use std::sync::MutexGuard;

use crate::hermit_test;

static HERMIT_RUN_LOCK: Mutex<()> = Mutex::new(());
const ISOLATED_WORKDIR_ENV: &str = "HERMIT_E2E_EMPTY_WORKDIR";
const HERMETIC_TEST_WORKDIR: &str = "/test";

pub(super) fn hermit_run_guard() -> MutexGuard<'static, ()> {
    HERMIT_RUN_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub(super) fn execution_root_args(requested: Option<&OsStr>) -> Result<Vec<OsString>, String> {
    match requested {
        None => Ok(Vec::new()),
        Some(value) if value == OsStr::new(HERMETIC_TEST_WORKDIR) => Ok(vec![
            "--mount=type=tmpfs,target=/test".into(),
            "--workdir=/test".into(),
        ]),
        Some(value) => Err(format!(
            "{ISOLATED_WORKDIR_ENV} must be {HERMETIC_TEST_WORKDIR}, got {value:?}"
        )),
    }
}

pub(super) fn append_hermit_args_with_execution_root_mode(
    command: &mut Command,
    args: &[&str],
    requested_workdir: Option<&OsStr>,
    private_mount: bool,
) -> Result<(), String> {
    let mut execution_root = execution_root_args(requested_workdir)?;
    if execution_root.is_empty() {
        command.args(args);
        return Ok(());
    }
    // These are Hermit options, so they must precede the delimiter rather than
    // becoming arguments to the guest program.
    let Some(guest_separator) = args.iter().position(|arg| *arg == "--") else {
        // Argument-parsing and non-guest subcommand tests have no guest whose
        // working directory can be changed. Preserve their argv so they keep
        // exercising Hermit's parser. The requested value was validated above,
        // so an unsupported marker still fails before Hermit can run.
        command.args(args);
        return Ok(());
    };
    // Several tests append the guest only after adding a Path argument to the
    // command. Classify the complete Hermit prefix, not just that final slice.
    let command_args = command
        .get_args()
        .chain(args[..guest_separator].iter().map(OsStr::new))
        .collect::<Vec<_>>();
    let starts_guest = command_args.contains(&OsStr::new("run"))
        || command_args
            .iter()
            .position(|arg| *arg == OsStr::new("record"))
            .is_some_and(|record| {
                !command_args[record + 1..].iter().any(|arg| {
                    matches!(
                        arg.to_str(),
                        Some("list" | "ls" | "rm" | "remove" | "clean")
                    )
                })
            });
    if !starts_guest {
        command.args(args);
        return Ok(());
    }
    let has_explicit_base_env = command_args.iter().any(|arg| {
        *arg == OsStr::new("--base-env")
            || arg
                .to_str()
                .is_some_and(|arg| arg.starts_with("--base-env="))
    });
    if !has_explicit_base_env {
        execution_root.insert(0, "--base-env=minimal".into());
    }
    if !private_mount || command_args.contains(&OsStr::new("--mount=type=tmpfs,target=/test")) {
        execution_root.retain(|arg| arg != OsStr::new("--mount=type=tmpfs,target=/test"));
    }
    if command_args.contains(&OsStr::new("--workdir=/test"))
        || command_args
            .windows(2)
            .any(|pair| pair == [OsStr::new("--workdir"), OsStr::new("/test")])
    {
        execution_root.retain(|arg| arg != OsStr::new("--workdir=/test"));
    }
    command
        .args(&args[..guest_separator])
        .args(execution_root)
        .args(&args[guest_separator..]);
    Ok(())
}

pub(super) fn append_hermit_args_using_outer_mount(command: &mut Command, args: &[&str]) {
    hermit_test::pin_canonical_epoch(command);
    let requested = std::env::var_os(ISOLATED_WORKDIR_ENV);
    append_hermit_args_with_execution_root_mode(command, args, requested.as_deref(), false)
        .unwrap_or_else(|error| panic!("PATH-CONTRACT: {error}"));
}

pub(super) fn hermit_command(args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_hermit-dynamorio"));
    append_hermit_args_using_outer_mount(&mut command, args);
    command
}

pub(super) fn hermit(args: &[&str]) -> Output {
    hermit_test::hermit_output(&format!("hermit {args:?}"), &mut hermit_command(args))
}

pub(super) fn hermit_with_stdin(args: &[&str], input: &[u8]) -> Output {
    hermit_test::hermit_output_with(&format!("hermit {args:?}"), || {
        let mut command = Command::new(env!("CARGO_BIN_EXE_hermit-dynamorio"));
        append_hermit_args_using_outer_mount(&mut command, args);
        let mut child = command
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
    .0
}

pub(super) fn assert_success(output: &Output, args: &[&str]) {
    assert!(
        output.status.success(),
        "hermit {args:?} failed with {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

pub(super) fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("hermit stderr should be UTF-8")
}

pub(super) fn strip_ansi_sgr(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut remaining = input;
    while let Some(start) = remaining.find("\x1b[") {
        output.push_str(&remaining[..start]);
        let escape = &remaining[start + 2..];
        let Some(end) = escape.find('m') else {
            output.push_str(&remaining[start..]);
            return output;
        };
        remaining = &escape[end + 1..];
    }
    output.push_str(remaining);
    output
}
