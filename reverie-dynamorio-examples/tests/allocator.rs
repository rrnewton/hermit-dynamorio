/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! The two original DynamoRIO-owned Counter1 allocator controls.
//! The generic allocator guest remains a single core-owned compiled fixture.

use reverie_liteinst::STRADDLER_STALENESS_TICKS_ENV;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const TEST_STRADDLER_STALENESS_TICKS: &str = "20000";

fn shared_allocator_guest() -> PathBuf {
    let path = std::env::var_os("REVERIE_EXAMPLES_ENV_GUEST")
        .map(PathBuf::from)
        .expect("select the exact core-owned reverie-liteinst-env-guest artifact");
    assert!(
        path.is_file(),
        "missing shared allocator guest: {}",
        path.display()
    );
    path
}

fn output_with_timeout(command: &mut Command) -> Output {
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if Instant::now() >= deadline {
            let process_group = i32::try_from(child.id()).unwrap();
            // SAFETY: the child created its own process group before exec.
            let _ = unsafe { libc::kill(-process_group, libc::SIGKILL) };
            let _ = child.kill();
            let output = child.wait_with_output().unwrap();
            panic!("LiteInst example timed out: {output:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn assert_counter1_allocator_output(output: &Output, summary_prefix: &str) {
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"allocator-growth-ok\n");
    let stderr = std::str::from_utf8(&output.stderr).unwrap();
    let total = stderr
        .lines()
        .find_map(|line| line.strip_prefix(summary_prefix))
        .expect("counter1 summary is missing")
        .parse::<u64>()
        .unwrap();
    assert!(total > 0, "{stderr}");
}

fn assert_one_counter1_allocator_summary(output: &Output) {
    let prefix = "counter1-global syscalls=";
    assert_counter1_allocator_output(output, prefix);
    let stderr = std::str::from_utf8(&output.stderr).unwrap();
    assert_eq!(
        stderr
            .lines()
            .filter(|line| line.starts_with(prefix))
            .count(),
        1,
        "{stderr}"
    );
}

fn counter1_allocator_on_dynamorio() {
    let mut command = Command::new(env!("CARGO_BIN_EXE_dynamorio-counter1"));
    command
        .env(
            STRADDLER_STALENESS_TICKS_ENV,
            TEST_STRADDLER_STALENESS_TICKS,
        )
        .arg("--")
        .arg(shared_allocator_guest())
        .arg("exercise-allocator");
    assert_one_counter1_allocator_summary(&output_with_timeout(&mut command));
}

#[test]
fn exact_counter1_tool_does_not_reenter_the_guest_allocator_on_dynamorio() {
    counter1_allocator_on_dynamorio();
}

#[test]
fn exact_counter1_tool_reports_its_local_global_without_a_coordinator() {
    let client = reverie_dynamorio_examples::client::find_client(Some(Path::new(env!("CARGO_BIN_EXE_dynamorio-counter1")))).expect(
        "build the DYNAMORIO client with reverie-dynamorio/scripts/build-client.sh or set REVERIE_DYNAMORIO_CLIENT",
    );
    // The documented exact-mode flag selects the same Tool with a local GlobalState.
    // Use the native launch shape from reverie-dynamorio/scripts/test-example-tools.sh.
    let mut command = Command::new(reverie_dynamorio::bundled_drrun_path());
    command
        .env_remove(reverie_dynamorio::TOOL_ENV)
        .env_remove(reverie_dynamorio::sync_rpc::RPC_SOCKET_ENV)
        .env("HERMIT_DYNAMORIO_COUNTER1_EXACT", "1")
        .args(["-quiet", "-disable_rseq", "-stack_size", "2M", "-c"])
        .arg(client)
        .arg("--")
        .arg(shared_allocator_guest())
        .arg("exercise-allocator");
    assert_one_counter1_allocator_summary(&output_with_timeout(&mut command));
}

#[test]
fn dbt_counter1_explicit_caller_selection_bypasses_prototype_client() {
    let manifest = Path::new(env!("CARGO_BIN_EXE_dynamorio-counter1"))
        .parent()
        .unwrap()
        .join("reverie-dynamorio-callers/counter1/manifest.json");
    // A valid, current pair is a prerequisite; a missing caller cannot prove
    // that the CLI selected the actual shared Tool through the caller owner.
    reverie_dynamorio::CallerArtifact::load(&manifest).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let missing_prototype = directory.path().join("missing-prototype-client.so");
    assert!(!missing_prototype.exists());
    let mut command = Command::new(env!("CARGO_BIN_EXE_dynamorio-counter1"));
    command
        .env("REVERIE_DYNAMORIO_CALLER_ARTIFACT", &manifest)
        .env("REVERIE_DYNAMORIO_CLIENT", &missing_prototype)
        .env(
            STRADDLER_STALENESS_TICKS_ENV,
            TEST_STRADDLER_STALENESS_TICKS,
        )
        .arg("--")
        .arg(shared_allocator_guest())
        .arg("exercise-allocator");
    assert_one_counter1_allocator_summary(&output_with_timeout(&mut command));
}

#[test]
fn dbt_counter1_bad_override_never_uses_the_valid_default_pair() {
    let default = Path::new(env!("CARGO_BIN_EXE_dynamorio-counter1"))
        .parent()
        .unwrap()
        .join("reverie-dynamorio-callers/counter1/manifest.json");
    reverie_dynamorio::CallerArtifact::load(&default).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let malformed = directory.path().join("malformed.json");
    std::fs::write(&malformed, b"{}").unwrap();
    for path in [directory.path().join("absent.json"), malformed] {
        for via_environment in [false, true] {
            let mut command = Command::new(env!("CARGO_BIN_EXE_dynamorio-counter1"));
            command.env_remove("REVERIE_DYNAMORIO_CALLER_ARTIFACT");
            if via_environment {
                command.env("REVERIE_DYNAMORIO_CALLER_ARTIFACT", &path);
            } else {
                command.arg("--dynamorio-artifact").arg(&path);
            }
            command
                .arg("--")
                .arg(shared_allocator_guest())
                .arg("exercise-allocator");
            let output = output_with_timeout(&mut command);
            assert!(!output.status.success(), "{output:?}");
            assert!(output.stdout.is_empty(), "{output:?}");
            assert!(
                String::from_utf8_lossy(&output.stderr)
                    .contains("cannot load DynamoRIO caller artifact"),
                "{output:?}"
            );
        }
    }
}
