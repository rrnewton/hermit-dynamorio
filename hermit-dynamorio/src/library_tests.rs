/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

/// Pin the existing environment-backed parser inputs for source-identical tests.
fn env_free_detconfig(epoch: &str, seed: &str) -> super::DetConfig {
    use clap::Parser;
    let mut config = super::DetConfig::parse_from(["hermit", epoch, seed, "--sched-seed=0"]);
    config.sched_seed = None;
    config
}

#[test]
fn dynamorio_retries_only_the_pre_guest_bootstrap_failure() {
    use std::os::unix::process::ExitStatusExt as _;

    let failure = std::process::ExitStatus::from_raw(
        reverie_dynamorio::CLIENT_THREAD_START_FAILURE_EXIT_CODE << 8,
    );
    assert!(super::dynamorio_client_thread_start_failed(&failure));
    assert!(!super::dynamorio_client_thread_start_failed(
        &std::process::ExitStatus::from_raw(1 << 8)
    ));
}

#[test]
fn dynamorio_public_dispatch_refuses_strict_schedule_boundaries() {
    let command = super::Command::new("/bin/true");
    let config = super::DetConfig {
        sequentialize_threads: true,
        strict_schedule_boundaries: true,
        ..Default::default()
    };

    let error = super::run_with_output(command, config, false, &None)
        .expect_err("DYNAMORIO must refuse strict schedule boundaries");
    assert!(
        error
            .to_string()
            .contains("dynamorio backend does not run --strict-schedule-boundaries"),
        "unexpected error: {error}"
    );
}

#[test]
fn dynamorio_public_dispatch_requires_sequentialized_threads() {
    let command = super::Command::new("/bin/true");
    let config = super::DetConfig {
        sequentialize_threads: false,
        ..Default::default()
    };

    let error = super::run_with_output(command, config, false, &None)
        .expect_err("DYNAMORIO must reject non-sequentialized execution");
    assert!(
        error
            .to_string()
            .contains("dynamorio backend requires sequentialized threads"),
        "unexpected error: {error}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dynamorio_library_environment_privacy_is_caller_selected() {
    {
        let mut command = super::Command::new("/usr/bin/env");
        command
            .env_clear()
            .env("HERMIT_DYNAMORIO_PRIVACY_SENTINEL", "known")
            .env("HERMIT_APP_VISIBLE", "known");
        let mut config = env_free_detconfig("--epoch=2026-01-01T00:00:00Z", "--seed=0");
        config.sequentialize_threads = true;
        config.deterministic_io = true;
        config.panic_on_unsupported_syscalls = true;
        config.validate();
        let config = hermit::prepare_backend_config_with_capabilities(
            config,
            reverie_dynamorio::DynamoRioRunner::capabilities(),
        );
        let output = super::run_dynamorio(command, config, false, true)
            .await
            .expect("run /usr/bin/env through the real DYNAMORIO library constructor");
        assert_eq!(
            output.status,
            super::ExitStatus::Exited(0),
            "guest did not exit 0.\n--- guest stderr ---\n{}\n--- guest stdout ---\n{}",
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout),
        );
        let printed = String::from_utf8(output.stdout).expect("synthetic environment is UTF-8");
        assert_eq!(
            printed
                .lines()
                .filter(|line| *line == "HERMIT_APP_VISIBLE=known")
                .count(),
            1,
            "public sentinel missing or duplicated: {printed}",
        );
        assert!(
            !printed.contains("HERMIT_DYNAMORIO_PRIVACY_SENTINEL="),
            "Hermit private environment sentinel leaked to guest: {printed}",
        );
        assert_eq!(printed, "HERMIT_APP_VISIBLE=known\n");
    }
}

#[test]
fn dynamorio_private_placement_preserves_library_dispatch() {
    use clap::Parser;

    hermit_dynamorio::test_support::LaunchArtifacts::required()
        .expect("DYNAMORIO placement control requires the real shared-engine native cohort");
    const CHILD: &str = "HERMIT_TEST_DYNAMORIO_PLACEMENT_LIBRARY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let probe = super::placement_probe::Probe::new(false);
        let (install, bind) = probe.launch();
        assert!(bind.is_none());
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "library::tests::dynamorio_private_placement_preserves_library_dispatch",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD, "1")
            .env("DYNAMORIO_TEST_LAUNCH_OBSERVATION", install)
            .env("HERMIT_REQUIRE_DYNAMORIO", "1")
            .output()
            .expect("run isolated DYNAMORIO library placement and output checks");
        let diagnostics = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{output:?}");
        assert!(
            diagnostics
                .lines()
                .filter(|line| line.starts_with("reverie-dynamorio: tool=Detcore "))
                .count()
                >= 2,
            "DYNAMORIO native summaries did not prove both Detcore dispatches: {diagnostics}"
        );
        assert!(!diagnostics.contains("guest-stderr"), "{diagnostics}");
        probe.assert_zero(2, false);
        return;
    }

    let mut command = super::Command::new("/bin/echo");
    command.arg("hello");
    let mut config = super::DetConfig::parse_from(["hermit-dynamorio-test"]);
    config.sequentialize_threads = true;
    config.validate();
    let output = super::run_with_output(command, config, true, &None)
        .expect("run /bin/echo through DynamoRioGuest<Detcore>");

    // Include captured output so a failed backend launch reports its diagnostics.
    assert_eq!(
        output.status,
        super::ExitStatus::Exited(0),
        "guest did not exit 0.\n--- guest stderr ---\n{}\n--- guest stdout ---\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout),
    );
    assert_eq!(output.stdout, b"hello\n");
    assert_eq!(output.stderr, b"");

    let mut command = super::Command::new("/bin/sh");
    command.args(["-c", "printf 'guest-stderr\\n' >&2"]);
    let mut config = super::DetConfig::parse_from(["hermit-dynamorio-test"]);
    config.sequentialize_threads = true;
    config.validate();
    let output = super::run_with_output(command, config, true, &None)
        .expect("capture nonempty guest stderr through DynamoRioGuest<Detcore>");
    assert_eq!(output.status, super::ExitStatus::Exited(0), "{output:?}");
    assert_eq!(output.stdout, b"");
    assert_eq!(output.stderr, b"guest-stderr\n");
}

#[test]
fn dynamorio_public_dispatch_runs_echo_through_detcore() {
    use clap::Parser;

    hermit_dynamorio::test_support::LaunchArtifacts::required()
        .expect("DYNAMORIO library control requires the real shared-engine native cohort");

    // Isolate the controller's fd 2 in a subprocess, without redirecting a
    // descriptor shared by other tests in this process.
    const CHILD: &str = "HERMIT_TEST_DYNAMORIO_LIBRARY_DIAGNOSTICS_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "library::tests::dynamorio_public_dispatch_runs_echo_through_detcore",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .expect("run isolated DYNAMORIO library output checks");
        let diagnostics = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{output:?}");
        assert!(
            diagnostics
                .lines()
                .filter(|line| line.starts_with("reverie-dynamorio: tool=Detcore "))
                .count()
                >= 2,
            "DYNAMORIO native summaries did not prove both Detcore dispatches: {diagnostics}"
        );
        assert!(!diagnostics.contains("guest-stderr"), "{diagnostics}");
        return;
    }

    let mut command = super::Command::new("/bin/echo");
    command.arg("hello");
    let mut config = super::DetConfig::parse_from(["hermit-dynamorio-test"]);
    config.sequentialize_threads = true;
    config.validate();
    let output = super::run_with_output(command, config, true, &None)
        .expect("run /bin/echo through DynamoRioGuest<Detcore>");

    // Include captured output so a failed backend launch reports its diagnostics.
    assert_eq!(
        output.status,
        super::ExitStatus::Exited(0),
        "guest did not exit 0.\n--- guest stderr ---\n{}\n--- guest stdout ---\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout),
    );
    assert_eq!(output.stdout, b"hello\n");
    assert_eq!(output.stderr, b"");

    let mut command = super::Command::new("/bin/sh");
    command.args(["-c", "printf 'guest-stderr\\n' >&2"]);
    let mut config = super::DetConfig::parse_from(["hermit-dynamorio-test"]);
    config.sequentialize_threads = true;
    config.validate();
    let output = super::run_with_output(command, config, true, &None)
        .expect("capture nonempty guest stderr through DynamoRioGuest<Detcore>");
    assert_eq!(output.status, super::ExitStatus::Exited(0), "{output:?}");
    assert_eq!(output.stdout, b"");
    assert_eq!(output.stderr, b"guest-stderr\n");
}

#[test]
fn dynamorio_public_status_dispatch_runs_true_through_detcore() {
    use clap::Parser;

    hermit_dynamorio::test_support::LaunchArtifacts::required()
        .expect("DYNAMORIO library control requires the real shared-engine native cohort");

    let command = super::Command::new("/bin/true");
    let mut config = super::DetConfig::parse_from(["hermit-dynamorio-test"]);
    config.sequentialize_threads = true;
    config.validate();
    let status = super::run(command, config, true, &None)
        .expect("run /bin/true through DynamoRioGuest<Detcore>");

    assert_eq!(status, super::ExitStatus::Exited(0));
}
