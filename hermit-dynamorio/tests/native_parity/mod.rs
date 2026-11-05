/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Maintained DynamoRIO/native/ptrace parity and copied-child refusal cases.
//! The core reference is an explicit actual artifact, with no lookup fallback.
//! These are the original case bodies; unavailable inputs are failures.

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Output;
use std::process::Stdio;
use std::sync::OnceLock;
use std::thread;
use std::time::Duration;
use std::time::Instant;

mod process_tree;
use process_tree::process_tree_snapshot;

use detcore_model::HERMIT_POLICY_REFUSAL_EXIT;

use super::hermit_test;
use super::support::append_hermit_args_using_outer_mount;
use super::support::assert_success;
use super::support::hermit_run_guard;
use super::support::stderr;

static CPUID_TABLE_DUMP_GUEST: OnceLock<PathBuf> = OnceLock::new();
static CLOCK_RLIMIT_PARITY_GUEST: OnceLock<PathBuf> = OnceLock::new();
static COPIED_SCHEDULER_QUERY_GUEST: OnceLock<PathBuf> = OnceLock::new();
static COPIED_PRIORITY_QUERY_GUEST: OnceLock<PathBuf> = OnceLock::new();
static CPUINFO_MATCHES_CPUID_GUEST: OnceLock<PathBuf> = OnceLock::new();
static DIRECTORY_SIZE_GUEST: OnceLock<PathBuf> = OnceLock::new();

/// Supplied by the bounded qualification recorder, which binds this core ELF
/// separately from both the extension and the compiled test harness.
fn core_binary() -> PathBuf {
    let requested = std::env::var_os("HERMIT_DYNAMORIO_CORE_BINARY")
        .expect("HERMIT_DYNAMORIO_CORE_BINARY must name the explicit core ptrace ELF");
    let requested = PathBuf::from(requested);
    assert!(
        requested.is_absolute(),
        "core ptrace ELF must be an absolute path"
    );
    let binary = fs::canonicalize(&requested).expect("resolve the explicit core ptrace ELF");
    let metadata = fs::metadata(&binary).expect("stat the explicit core ptrace ELF");
    assert!(metadata.is_file(), "core ptrace ELF must be a regular file");
    assert_ne!(
        metadata.permissions().mode() & 0o111,
        0,
        "core ptrace ELF must be executable"
    );
    let extension = fs::metadata(env!("CARGO_BIN_EXE_hermit-dynamorio"))
        .expect("stat the compiled extension ELF");
    assert_ne!(
        (metadata.dev(), metadata.ino()),
        (extension.dev(), extension.ino()),
        "the core reference must not be the extension ELF or its hardlink"
    );
    binary
}

/// Explicit executable selection, not an argv rewriting shim. Every retained
/// case constructs its actual run arguments without the removed DBT selector.
fn parity_command(backend: &str, args: &[&str]) -> Command {
    let mut command = match backend {
        "ptrace" => {
            let mut command = Command::new(core_binary());
            command.args(["--backend", "ptrace"]);
            command
        }
        "dynamorio" => Command::new(env!("CARGO_BIN_EXE_hermit-dynamorio")),
        other => panic!("unknown parity test backend: {other}"),
    };
    append_hermit_args_using_outer_mount(&mut command, args);
    command
}

fn parity_output(backend: &str, args: &[&str]) -> Output {
    hermit_test::hermit_output(
        &format!("{backend} {args:?}"),
        &mut parity_command(backend, args),
    )
}

fn process_build_root(name: &str) -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(name)
        .join("build")
        .join(std::process::id().to_string())
}

fn futex_c_guest(cell: &'static OnceLock<PathBuf>, name: &str) -> &'static Path {
    cell.get_or_init(|| {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/native_parity/c");
        let build_root = process_build_root(&name.replace('_', "-"));
        fs::create_dir_all(&build_root).expect("failed to create the futex guest directory");
        let guest = build_root.join(name);
        let output = Command::new("cc")
            .args(["-O0", "-g", "-Wall", "-Wextra", "-Werror", "-pthread"])
            .arg(repository.join(format!("{name}.c")))
            .arg("-o")
            .arg(&guest)
            .output()
            .expect("failed to compile the futex guest");
        assert!(
            output.status.success(),
            "{name} guest compilation failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        guest
    })
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("hermit stdout should be UTF-8")
}

fn verify_log_of_true(backend: &str, logs: &Path) -> PathBuf {
    let log_dir = logs.to_str().expect("UTF-8 temporary path");
    // `--log info`, as the e2e harness runs every verify cell: at the default
    // verification level a ptrace record carries its tracing span.
    let args = [
        "--log",
        "info",
        "run",
        "--max-timeslice=disabled",
        "--epoch=2026-10-05T09:20:32+00:00",
        "--verify",
        "--keep-logs",
        "--verify-log-dir",
        log_dir,
        "--",
        "/bin/true",
    ];
    let output = hermit_test::hermit_output(
        "hermit run",
        parity_command(backend, &args)
            .env_remove("RUST_LOG")
            .env_remove("HERMIT_LOG")
            .env_remove("HERMIT_LOG_FILE")
            .stdin(Stdio::null()),
    );
    assert_success(&output, &args);
    let golden: Vec<_> = fs::read_dir(logs)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("run1_log_"))
        })
        .collect();
    let [golden] = &golden[..] else {
        panic!("expected one retained run 1 log in {log_dir}, found {golden:?}");
    };
    golden.clone()
}
#[test]
fn dynamorio_rdtsc_is_answered_by_detcore_as_under_ptrace() {
    let _lock = hermit_run_guard();
    let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let (ptrace_dir, dynamorio_dir) = (dir.path().join("ptrace"), dir.path().join("dynamorio"));
    fs::create_dir_all(&ptrace_dir).unwrap();
    fs::create_dir_all(&dynamorio_dir).unwrap();
    let ptrace = fs::read_to_string(verify_log_of_true("ptrace", &ptrace_dir)).unwrap();
    let dynamorio = fs::read_to_string(verify_log_of_true("dynamorio", &dynamorio_dir)).unwrap();
    let reads = ptrace.matches("inbound rdtsc").count();
    assert!(reads > 0, "ptrace logged no rdtsc:\n{ptrace}");
    assert_eq!(
        dynamorio.matches("inbound rdtsc").count(),
        reads,
        "ptrace:\n{ptrace}\ndynamorio:\n{dynamorio}"
    );
}
#[test]
fn dynamorio_cpuid_rows_match_ptrace() {
    let _lock = hermit_run_guard();
    let guest = futex_c_guest(&CPUID_TABLE_DUMP_GUEST, "cpuid_table_dump")
        .to_str()
        .unwrap()
        .to_owned();
    let run = |backend: &str| {
        let args = [
            "run",
            "--strict",
            "--base-env=minimal",
            "--",
            guest.as_str(),
        ];
        let output = parity_output(backend, &args);
        assert_success(&output, &args);
        stdout(&output)
    };
    let ptrace = run("ptrace");
    assert!(
        ptrace.contains("00000007.0: 00000000 00180fb9 00000000 00000000\n"),
        "{ptrace}"
    );
    assert!(!ptrace.contains("MISMATCH"), "{ptrace}");
    assert_eq!(run("dynamorio"), ptrace);
}
#[test]
fn dynamorio_rlimit_and_clock_answers_match_ptrace() {
    let _lock = hermit_run_guard();
    let guest = futex_c_guest(&CLOCK_RLIMIT_PARITY_GUEST, "clock_rlimit_parity")
        .to_str()
        .unwrap()
        .to_owned();
    let run = |backend: &str| {
        let args = [
            "run",
            "--strict",
            "--base-env=minimal",
            "--",
            guest.as_str(),
            "limits",
        ];
        let output = parity_output(backend, &args);
        assert_success(&output, &args);
        stdout(&output)
    };
    let ptrace = run("ptrace");
    assert!(ptrace.contains("rlimit lowered: 0 40 "), "{ptrace}");
    assert!(
        ptrace.contains("clock_gettime invalid: -1 EINVAL\n"),
        "{ptrace}"
    );
    assert_eq!(run("dynamorio"), ptrace);
}
#[test]
fn dynamorio_refuses_a_vfork_childs_clock_read_by_name() {
    let _lock = hermit_run_guard();
    let guest = futex_c_guest(&CLOCK_RLIMIT_PARITY_GUEST, "clock_rlimit_parity")
        .to_str()
        .unwrap()
        .to_owned();
    let args = || {
        [
            "run",
            "--strict",
            "--base-env=minimal",
            "--",
            guest.as_str(),
            "vfork-clock",
        ]
        .map(str::to_owned)
    };
    let ptrace_args = args();
    let ptrace_args: Vec<&str> = ptrace_args.iter().map(String::as_str).collect();
    let ptrace = parity_output("ptrace", &ptrace_args);
    assert_success(&ptrace, &ptrace_args);
    assert_eq!(stdout(&ptrace), "vfork child clock_gettime: ok, exit 0\n");

    let dynamorio_args = args();
    let dynamorio_args: Vec<&str> = dynamorio_args.iter().map(String::as_str).collect();
    let dynamorio = parity_output("dynamorio", &dynamorio_args);
    assert!(!dynamorio.status.success(), "{}", stderr(&dynamorio));
    assert!(
        stderr(&dynamorio).contains(&format!(
            "detcore-dynamorio: unsupported syscall {} in copied child",
            libc::SYS_clock_gettime
        )),
        "{}",
        stderr(&dynamorio)
    );
}
#[test]
fn dynamorio_refuses_a_vfork_childs_scheduler_policy_query_by_name() {
    let _lock = hermit_run_guard();
    let guest = futex_c_guest(&COPIED_SCHEDULER_QUERY_GUEST, "copied_scheduler_query")
        .to_str()
        .unwrap();
    let batch = |mut command: Command| {
        // SAFETY: the raw syscall only changes this child before exec; it does
        // not allocate, acquire locks, or alter the test process's policy.
        unsafe {
            command.pre_exec(|| {
                let parameter = libc::sched_param { sched_priority: 0 };
                if libc::syscall(
                    libc::SYS_sched_setscheduler,
                    0,
                    libc::SCHED_BATCH,
                    &parameter,
                ) == -1
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        command
    };
    let mut native = Command::new(guest);
    native.arg("vfork");
    let native = batch(native)
        .output()
        .expect("native SCHED_BATCH vfork control");
    assert!(native.status.success(), "{native:?}");
    assert_eq!(
        stdout(&native),
        "after sched_getscheduler\nvfork sched_getscheduler: policy=3 errno=0 exit=0\n"
    );
    let run = |backend, strictness, mode| {
        let args = ["run", strictness, "--base-env=minimal", "--", guest, mode];
        hermit_test::hermit_output(
            "scheduler policy query",
            &mut batch(parity_command(backend, &args)),
        )
    };
    // The ordinary shared Tool retains its existing SCHED_OTHER policy even
    // though the launcher and the native control inherit SCHED_BATCH.
    for backend in ["dynamorio", "ptrace"] {
        let output = run(backend, "--strict", "ordinary");
        assert!(output.status.success(), "{backend}: {output:?}");
        assert_eq!(
            stdout(&output),
            "ordinary sched_getscheduler: policy=0 errno=0\n"
        );
    }
    let ptrace = run("ptrace", "--strict", "vfork");
    assert!(ptrace.status.success(), "{ptrace:?}");
    assert_eq!(
        stdout(&ptrace),
        "after sched_getscheduler\nvfork sched_getscheduler: policy=0 errno=0 exit=0\n"
    );
    // This full transcript proves actual native policy inheritance; do not
    // assume that the guest kept the launcher's policy or accept a fixed zero.
    let passthrough = run("dynamorio", "--allow-unsupported-syscalls", "vfork");
    assert!(passthrough.status.success(), "{passthrough:?}");
    assert_eq!(passthrough.stdout, native.stdout);

    let refused = run("dynamorio", "--strict", "vfork");
    assert_eq!(refused.status.code(), Some(101), "{refused:?}");
    assert!(
        stderr(&refused).contains("unsupported syscall sched_getscheduler (145) in copied child"),
        "{refused:?}"
    );
    assert!(
        stderr(&refused).contains(&format!(
            "detcore-dynamorio: unsupported syscall {} in copied child",
            libc::SYS_sched_getscheduler
        )),
        "{refused:?}"
    );
    assert!(
        !stdout(&refused).contains("after sched_getscheduler"),
        "{refused:?}"
    );
}
#[test]
fn dynamorio_refuses_a_vfork_childs_scheduler_parameter_query_by_name() {
    let _lock = hermit_run_guard();
    let guest = futex_c_guest(&COPIED_SCHEDULER_QUERY_GUEST, "copied_scheduler_query")
        .to_str()
        .unwrap();
    let native = Command::new(guest)
        .arg("param-vfork")
        .output()
        .expect("native sched_getparam vfork control");
    assert!(native.status.success(), "{native:?}");
    let vfork_output = "after sched_getparam\n\
        vfork sched_getparam: result=0 errno=0 priority=0 canaries=1 exit=0\n";
    assert_eq!(stdout(&native), vfork_output);
    let run = |backend, strictness, mode| {
        let args = ["run", strictness, "--base-env=minimal", "--", guest, mode];
        hermit_test::hermit_output(
            "scheduler parameter query",
            &mut parity_command(backend, &args),
        )
    };
    // A zero priority alone does not prove Tool mediation. These complete
    // target/null transcripts characterize the shared handler's existing
    // PID-existence and null-pointer limitations, not native Linux semantics.
    for backend in ["dynamorio", "ptrace"] {
        let output = run(backend, "--strict", "param-ordinary");
        assert!(output.status.success(), "{backend}: {output:?}");
        assert_eq!(
            stdout(&output),
            "ordinary sched_getparam: result=0 errno=0 priority=0 canaries=1\n"
        );
        let targets = run(backend, "--strict", "param-targets");
        assert!(targets.status.success(), "{backend}: {targets:?}");
        assert_eq!(
            stdout(&targets),
            "parameter identities: child=1 parent=1\n\
             sched_getparam zero: result=0 errno=0 priority=0 canaries=1\n\
             sched_getparam self: result=0 errno=0 priority=0 canaries=1\n\
             sched_getparam live: result=0 errno=0 priority=0 canaries=1\n\
             sched_getparam absent: result=0 errno=0 priority=0 canaries=1\n\
             sched_getparam negative: result=0 errno=0 priority=0 canaries=1\n\
             sched_getparam null: result=0 errno=0 priority=324508639 canaries=1\n\
             sched_getparam bad: result=-1 errno=14 priority=324508639 canaries=1\n\
             parameter wait: exit=37\nsched-getparam-targets=ok\n"
        );
    }
    let ptrace = run("ptrace", "--strict", "param-vfork");
    assert!(ptrace.status.success(), "{ptrace:?}");
    assert_eq!(stdout(&ptrace), vfork_output);
    let passthrough = run("dynamorio", "--allow-unsupported-syscalls", "param-vfork");
    assert!(passthrough.status.success(), "{passthrough:?}");
    assert_eq!(passthrough.stdout, native.stdout);

    let refused = run("dynamorio", "--strict", "param-vfork");
    assert_eq!(refused.status.code(), Some(101), "{refused:?}");
    assert!(
        stderr(&refused).contains("unsupported syscall sched_getparam (143) in copied child"),
        "{refused:?}"
    );
    assert!(
        stderr(&refused).contains(&format!(
            "detcore-dynamorio: unsupported syscall {} in copied child",
            libc::SYS_sched_getparam
        )),
        "{refused:?}"
    );
    assert!(
        !stdout(&refused).contains("after sched_getparam"),
        "{refused:?}"
    );
}
#[test]
fn dynamorio_refuses_a_vfork_childs_affinity_query_by_name() {
    let _lock = hermit_run_guard();
    let guest = futex_c_guest(&COPIED_SCHEDULER_QUERY_GUEST, "copied_scheduler_query")
        .to_str()
        .unwrap();
    use nix::sched::CpuSet;
    use nix::sched::sched_getaffinity;
    use nix::sched::sched_setaffinity;
    use nix::unistd::Pid;

    // Hermit chooses one CPU from its inherited mask. Give both command
    // children the same measured singleton so the full native mask is an
    // independent oracle under identical launch affinity; the test parent
    // and all guest syscall policies remain unchanged.
    let inherited = sched_getaffinity(Pid::from_raw(0)).expect("read allowed CPU mask");
    let cpu = (0..CpuSet::count())
        .find(|&cpu| inherited.is_set(cpu).unwrap_or(false))
        .expect("nonempty allowed CPU mask");
    let mut singleton = CpuSet::new();
    singleton.set(cpu).expect("allowed CPU fits CpuSet");
    let same_cpu = |mut command: Command| {
        // SAFETY: one async-signal-safe syscall with a mask built before fork;
        // no allocation or mutation of the parent process.
        unsafe {
            command.pre_exec(move || {
                sched_setaffinity(Pid::from_raw(0), &singleton).map_err(std::io::Error::from)
            });
        }
        command
    };
    let mut native_command = Command::new(guest);
    native_command.arg("affinity-vfork");
    let native = same_cpu(native_command)
        .output()
        .expect("native sched_getaffinity vfork control");
    assert!(native.status.success(), "{native:?}");
    assert!(
        stdout(&native).starts_with("after sched_getaffinity\nvfork sched_getaffinity: result="),
        "{native:?}"
    );
    let run = |backend, strictness, mode| {
        let args = ["run", strictness, "--base-env=minimal", "--", guest, mode];
        hermit_test::hermit_output(
            "scheduler affinity query",
            &mut same_cpu(parity_command(backend, &args)),
        )
    };
    // This is the existing shared CPU0 policy, not the native Linux mask.
    // Its ignored target-existence semantics are unchanged by this guard cut.
    let mask = format!("01{}{}", "00".repeat(15), "7f".repeat(112));
    let ordinary_output =
        format!("ordinary sched_getaffinity: result=16 errno=0 mask={mask} canaries=1\n");
    for backend in ["dynamorio", "ptrace"] {
        let output = run(backend, "--strict", "affinity-ordinary");
        assert!(output.status.success(), "{backend}: {output:?}");
        assert_eq!(stdout(&output), ordinary_output);
    }
    let ptrace = run("ptrace", "--strict", "affinity-vfork");
    assert!(ptrace.status.success(), "{ptrace:?}");
    assert_eq!(
        stdout(&ptrace),
        format!(
            "after sched_getaffinity\nvfork sched_getaffinity: result=16 errno=0 mask={mask} canaries=1\n"
        )
    );
    // The complete measured native transcript includes the raw positive byte
    // count, all 128 mask/suffix bytes and both canaries; do not hardcode CPU0.
    let passthrough = run(
        "dynamorio",
        "--allow-unsupported-syscalls",
        "affinity-vfork",
    );
    assert!(passthrough.status.success(), "{passthrough:?}");
    assert_eq!(passthrough.stdout, native.stdout);

    let refused = run("dynamorio", "--strict", "affinity-vfork");
    assert_eq!(refused.status.code(), Some(101), "{refused:?}");
    assert!(
        stderr(&refused).contains("unsupported syscall sched_getaffinity (204) in copied child"),
        "{refused:?}"
    );
    assert!(
        stderr(&refused).contains(&format!(
            "detcore-dynamorio: unsupported syscall {} in copied child",
            libc::SYS_sched_getaffinity
        )),
        "{refused:?}"
    );
    assert!(
        !stdout(&refused).contains("after sched_getaffinity"),
        "{refused:?}"
    );
}
#[test]
fn dynamorio_refuses_a_vfork_childs_priority_query_by_name() {
    let _lock = hermit_run_guard();
    let guest = futex_c_guest(&COPIED_PRIORITY_QUERY_GUEST, "copied_priority_query")
        .to_str()
        .unwrap();
    let nice19 = |mut command: Command| {
        // SAFETY: the raw syscall only changes this child before exec; it does
        // not allocate, acquire locks, or alter the test process's policy.
        unsafe {
            command.pre_exec(|| {
                if libc::syscall(libc::SYS_setpriority, libc::PRIO_PROCESS, 0, 19) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        command
    };
    let mut native = Command::new(guest);
    native.arg("vfork");
    let native = nice19(native)
        .output()
        .expect("native nice19 vfork control");
    assert!(native.status.success(), "{native:?}");
    assert_eq!(
        stdout(&native),
        "after getpriority\nvfork getpriority: raw=1 errno=0 exit=0\n"
    );
    let run = |backend, strictness, mode| {
        let args = ["run", strictness, "--base-env=minimal", "--", guest, mode];
        hermit_test::hermit_output(
            "priority query",
            &mut nice19(parity_command(backend, &args)),
        )
    };
    // The ordinary shared Tool retains raw20 even when native nice19 returns1.
    for backend in ["dynamorio", "ptrace"] {
        let output = run(backend, "--strict", "ordinary");
        assert!(output.status.success(), "{backend}: {output:?}");
        assert_eq!(stdout(&output), "ordinary getpriority: raw=20 errno=0\n");
        let targets = run(backend, "--strict", "ordinary-targets");
        assert!(targets.status.success(), "{backend}: {targets:?}");
        assert_eq!(
            stdout(&targets),
            "targets identities: child=1 parent=1\n\
             getpriority zero: raw=20 errno=0\n\
             getpriority self: raw=20 errno=0\n\
             getpriority live: raw=20 errno=0\n\
             getpriority absent: raw=20 errno=0\n\
             getpriority negative: raw=20 errno=0\n\
             getpriority invalid-negative: raw=-1 errno=22\n\
             getpriority invalid-live: raw=-1 errno=22\n\
             getpriority invalid-absent: raw=-1 errno=22\n\
             targets wait: exit=37\ngetpriority-targets=ok\n"
        );
    }
    let ptrace = run("ptrace", "--strict", "vfork");
    assert!(ptrace.status.success(), "{ptrace:?}");
    assert_eq!(
        stdout(&ptrace),
        "after getpriority\nvfork getpriority: raw=20 errno=0 exit=0\n"
    );
    // This full transcript proves the copied child actually inherited nice19;
    // do not assume that launch configuration alone establishes its raw value.
    let passthrough = run("dynamorio", "--allow-unsupported-syscalls", "vfork");
    assert!(passthrough.status.success(), "{passthrough:?}");
    assert_eq!(passthrough.stdout, native.stdout);

    let refused = run("dynamorio", "--strict", "vfork");
    assert_eq!(refused.status.code(), Some(101), "{refused:?}");
    assert!(
        stderr(&refused).contains("unsupported syscall getpriority (140) in copied child"),
        "{refused:?}"
    );
    assert!(
        stderr(&refused).contains(&format!(
            "detcore-dynamorio: unsupported syscall {} in copied child",
            libc::SYS_getpriority
        )),
        "{refused:?}"
    );
    assert!(
        !stdout(&refused).contains("after getpriority"),
        "{refused:?}"
    );
}
#[test]
fn run_dynamorio_cpuinfo_matches_cpuid() {
    assert_cpuinfo_matches_cpuid(&[]);
}
#[test]
fn dynamorio_a_procfs_directory_keeps_its_size() {
    let _guard = hermit_run_guard();
    assert_procfs_directory_keeps_its_size("dynamorio");
}
fn run_with_deadline(
    mut command: Command,
    directory: &Path,
    limit: Duration,
    open_stdin: bool,
) -> (Option<std::process::ExitStatus>, String) {
    let label = format!("{command:?}");
    hermit_test::hermit_output_with(&label, || {
        let (status, stderr) = run_with_deadline_once(&mut command, directory, limit, open_stdin);
        (
            output_of(status, Vec::new(), stderr.clone().into_bytes()),
            (status, stderr),
        )
    })
    .1
}

fn run_with_deadline_once(
    command: &mut Command,
    directory: &Path,
    limit: Duration,
    open_stdin: bool,
) -> (Option<std::process::ExitStatus>, String) {
    let stdout_path = directory.join("deadline-run.stdout");
    let stderr_path = directory.join("deadline-run.stderr");
    command
        .stdout(fs::File::create(&stdout_path).expect("failed to create the stdout file"))
        .stderr(fs::File::create(&stderr_path).expect("failed to create the stderr file"));
    if open_stdin {
        command.stdin(Stdio::piped());
    }
    let mut child = command.spawn().expect("failed to spawn hermit");
    let stdin_writer = child.stdin.take();
    let deadline = Instant::now() + limit;
    let mut snapshot = None;
    let status = loop {
        match child.try_wait().expect("failed to poll hermit") {
            Some(status) => break Some(status),
            None if Instant::now() >= deadline => {
                snapshot = Some(process_tree_snapshot(child.id()));
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            None => thread::sleep(Duration::from_millis(50)),
        }
    };
    drop(stdin_writer);
    let mut stderr = fs::read_to_string(&stderr_path).expect("failed to read the stderr file");
    if let Some(snapshot) = snapshot {
        stderr.push_str(&format!(
            "\n--- killed at the {limit:?} deadline; the process tree before the kill ---\n{snapshot}"
        ));
    }
    (status, stderr)
}

fn output_of(status: Option<std::process::ExitStatus>, stdout: Vec<u8>, stderr: Vec<u8>) -> Output {
    use std::os::unix::process::ExitStatusExt;
    Output {
        status: status.unwrap_or_else(|| std::process::ExitStatus::from_raw(libc::SIGKILL)),
        stdout,
        stderr,
    }
}

fn assert_cpuinfo_matches_cpuid(backend: &[&str]) {
    let (status, stdout, log, report) = cpuinfo_matches_cpuid_run(&[]);
    assert_eq!(
        status,
        Some(0),
        "{backend:?}\nstdout:\n{stdout}\nstderr:\n{log}"
    );
    assert_cpuinfo_matches_cpuid_output(backend, &stdout, &log, &report);
}

fn cpuinfo_matches_cpuid_run(run_args: &[&str]) -> (Option<i32>, String, String, Vec<u8>) {
    let guest = futex_c_guest(&CPUINFO_MATCHES_CPUID_GUEST, "cpuinfo_matches_cpuid")
        .to_str()
        .unwrap()
        .to_owned();
    let directory = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let verdict = directory.path().join("verify.json");
    let mut args = vec!["--log=info"];
    args.extend_from_slice(&[
        "run",
        "--strict",
        "--verify",
        "--verify-strict",
        "--verify-json",
        verdict.to_str().unwrap(),
    ]);
    args.extend_from_slice(run_args);
    // The original DBT selection did not append the ptrace-only --timeout.
    // Keep its unchanged 180-second run_with_deadline bound below.
    args.extend_from_slice(&["--", guest.as_str()]);
    let (status, log) = run_with_deadline(
        parity_command("dynamorio", &args),
        directory.path(),
        Duration::from_secs(180),
        false,
    );
    let stdout = fs::read_to_string(directory.path().join("deadline-run.stdout"))
        .expect("failed to read the guest's stdout");
    let report = fs::read(&verdict).unwrap_or_default();
    (status.and_then(|status| status.code()), stdout, log, report)
}

fn assert_cpuinfo_matches_cpuid_output(backend: &[&str], stdout: &str, log: &str, report: &[u8]) {
    let mut expected = String::from(
        "vendor_id: match\n\
         cpu family: match\n\
         model: match\n\
         stepping: match\n\
         model name: match\n",
    );
    for flag in [
        "sse",
        "sse2",
        "pni",
        "pclmulqdq",
        "ssse3",
        "fma",
        "sse4_1",
        "sse4_2",
        "movbe",
        "popcnt",
        "aes",
        "avx",
        "f16c",
        "bmi1",
        "avx2",
        "bmi2",
        "avx512f",
        "adx",
        "sha_ni",
        "avx512bw",
        "vaes",
        "vpclmulqdq",
    ] {
        expected += &format!("flag {flag}: match\n");
    }
    assert_eq!(stdout, expected, "{backend:?}\nstderr:\n{log}");
    let report: serde_json::Value =
        serde_json::from_slice(report).expect("the verdict must be JSON");
    assert_eq!(report["bitwise_parity"], true, "{backend:?}: {report}");
}

fn assert_procfs_directory_keeps_its_size(backend: &str) {
    let guest = futex_c_guest(&DIRECTORY_SIZE_GUEST, "directory_size")
        .to_str()
        .unwrap()
        .to_owned();
    let directory = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let (status, log) = run_with_deadline(
        parity_command(
            "dynamorio",
            &[
                "run",
                "--strict",
                "--tmp=/tmp",
                "--",
                guest.as_str(),
                "proc",
            ],
        ),
        directory.path(),
        Duration::from_secs(60),
        false,
    );
    let stdout = fs::read_to_string(directory.path().join("deadline-run.stdout"))
        .expect("failed to read the guest's stdout");
    assert_eq!(
        status.and_then(|status| status.code()),
        Some(0),
        "{backend}: the run must exit 0:\nstdout:\n{stdout}\nstderr:\n{log}"
    );
    assert_eq!(stdout, "proc 0 0; seek end: 0\n", "{backend}");
}

#[test]
fn dynamorio_refuses_cpu_profile_v3_by_name() {
    let _lock = hermit_run_guard();
    let args = ["run", "--cpu-profile", "x86-64-v3", "--", "/bin/true"];
    let output = parity_output("dynamorio", &args);
    assert_eq!(
        output.status.code(),
        Some(HERMIT_POLICY_REFUSAL_EXIT),
        "{}",
        stderr(&output)
    );
    assert!(
        stderr(&output).contains(
            "--cpu-profile x86-64-v3 refused: --backend=dynamorio: Detcore answers DynamoRIO's CPUID, but \
             the DynamoRIO runtime's configuration carries no CPU profile"
        ),
        "{}",
        stderr(&output)
    );
}
