/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

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

fn run_dynamorio_strace(extra: &[&str], guest: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_dynamorio-strace"));
    command.args(extra).arg("--").args(guest);
    output_with_timeout(&mut command)
}

#[test]
fn dynamorio_exact_strace_tool_observes_filtered_write() {
    let output = run_dynamorio_strace(&["--trace", "write"], &["/bin/echo", "hello"]);

    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"hello\n");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("write(1,"), "{stderr}");
    assert!(stderr.contains(" = 6"), "{stderr}");
}

#[test]
fn dynamorio_strace_reports_exit_after_echo_closes_stdio() {
    // GNU echo closes stdout and stderr before exit. Keep the original write
    // oracle above unchanged, and also require the consuming Tool diagnostics
    // to survive those application descriptor closes through controller RPC.
    let output = run_dynamorio_strace(&["--trace", "write"], &["/bin/echo", "hello"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"hello\n");
    let stderr = String::from_utf8(output.stderr).unwrap();
    let lines: Vec<_> = stderr.lines().collect();
    let write = lines
        .iter()
        .position(|line| line.contains("write(1,") && line.ends_with(" = 6"))
        .expect("missing completed write diagnostic");
    let threads: Vec<_> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.starts_with("Thread "))
        .collect();
    let processes: Vec<_> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.starts_with("Process "))
        .collect();
    assert_eq!(threads.len(), 1, "{stderr}");
    assert_eq!(processes.len(), 1, "{stderr}");
    assert!(
        threads[0].1.ends_with(" exited with status Exited(0)"),
        "{stderr}"
    );
    assert!(
        processes[0].1.ends_with(" exited with status Exited(0)"),
        "{stderr}"
    );
    assert!(
        write < threads[0].0 && threads[0].0 < processes[0].0,
        "{stderr}"
    );
}

#[test]
fn dynamorio_exact_strace_tool_observes_thread_and_process_exit() {
    let output = run_dynamorio_strace(&["--trace", "write"], &["/bin/true"]);

    assert!(output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("Thread "), "{stderr}");
    assert!(stderr.contains("Process "), "{stderr}");
    assert_eq!(stderr.matches("exited with status Exited(0)").count(), 2);
}

#[test]
fn dynamorio_strace_bad_override_never_uses_the_valid_default_pair() {
    // Require the real production prerequisite: a failed fallback to another
    // absent artifact would not establish this negative control.
    let default = Path::new(env!("CARGO_BIN_EXE_dynamorio-strace"))
        .parent()
        .unwrap()
        .join("reverie-dynamorio-callers/strace/manifest.json");
    reverie_dynamorio::CallerArtifact::load(&default).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let malformed = directory.path().join("malformed.json");
    std::fs::write(&malformed, b"{}").unwrap();
    let mut valid: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&default).unwrap()).unwrap();
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let inputs = valid["inputs"].as_array().unwrap();
    for relative in [
        "Cargo.toml",
        "Cargo.lock",
        "rust-toolchain.toml",
        "reverie-dynamorio-examples/strace-dynamorio-runtime/Cargo.toml",
        "reverie-dynamorio-examples/strace-dynamorio-runtime/Cargo.lock",
        "reverie-dynamorio-examples/strace-dynamorio-runtime/build.rs",
        "reverie-dynamorio-examples/strace-dynamorio-runtime/src/lib.rs",
        "reverie-dynamorio/build.rs",
        "reverie-dynamorio/native/client.c",
        "reverie-dynamorio/native/CMakeLists.txt",
        "reverie-dynamorio/scripts/build-client.sh",
        "reverie-dynamorio/scripts/build-caller.rs",
    ] {
        let required = workspace.join(relative).canonicalize().unwrap();
        assert!(
            inputs
                .iter()
                .any(|input| { Path::new(input["path"].as_str().unwrap()) == required }),
            "missing declared input {relative}"
        );
    }
    // These exact production Tool files now come from the resolved pinned
    // core package. They must still be measured, not copied or omitted.
    for suffix in [
        "strace/config.rs",
        "strace/filter.rs",
        "strace/global_state.rs",
        "strace/tool.rs",
    ] {
        let matches = inputs
            .iter()
            .filter(|input| Path::new(input["path"].as_str().unwrap()).ends_with(suffix))
            .collect::<Vec<_>>();
        assert_eq!(
            matches.len(),
            1,
            "missing or ambiguous shared Tool input {suffix}"
        );
        assert!(Path::new(matches[0]["path"].as_str().unwrap()).is_file());
    }
    for marker in [
        ".reverie-dynamorio-install.provenance",
        ".reverie-dynamorio-install.sha256",
    ] {
        assert!(
            inputs
                .iter()
                .any(|input| { Path::new(input["path"].as_str().unwrap()).ends_with(marker) }),
            "missing SDK input {marker}"
        );
    }
    for image in ["runtime", "client"] {
        valid[image]["path"] = serde_json::json!(
            default
                .parent()
                .unwrap()
                .join(valid[image]["path"].as_str().unwrap())
        );
    }
    let copied = directory.path().join("valid.json");
    std::fs::write(&copied, serde_json::to_vec(&valid).unwrap()).unwrap();
    reverie_dynamorio::CallerArtifact::load(&copied).unwrap();
    let mut bad_paths = vec![directory.path().join("absent.json"), malformed];
    for (label, pointer) in [
        ("stale-runtime", "/runtime/sha256"),
        ("stale-client", "/client/sha256"),
        ("stale-source", "/inputs/0/sha256"),
    ] {
        let mut stale = valid.clone();
        let digest = stale.pointer_mut(pointer).unwrap();
        let mut changed = digest.as_str().unwrap().as_bytes().to_vec();
        changed[0] = if changed[0] == b'0' { b'1' } else { b'0' };
        *digest = serde_json::Value::String(String::from_utf8(changed).unwrap());
        let path = directory.path().join(format!("{label}.json"));
        std::fs::write(&path, serde_json::to_vec(&stale).unwrap()).unwrap();
        bad_paths.push(path);
    }
    for path in bad_paths {
        for via_environment in [false, true] {
            let mut command = Command::new(env!("CARGO_BIN_EXE_dynamorio-strace"));
            if via_environment {
                command.env("REVERIE_DYNAMORIO_CALLER_ARTIFACT", &path);
            } else {
                command.arg("--dynamorio-artifact").arg(&path);
            }
            command.args(["--trace", "write", "--", "/bin/echo", "hello"]);
            let output = output_with_timeout(&mut command);
            assert!(!output.status.success(), "{output:?}");
            assert!(output.stdout.is_empty(), "{output:?}");
            assert!(!output.stderr.is_empty(), "{output:?}");
        }
    }
}
