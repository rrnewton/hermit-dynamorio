/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! The backend-owned public library launch path. The shared core retains
//! configuration normalization and generic launch admission.

use std::os::fd::AsFd;
use std::path::PathBuf;

use crate::physical::{declare_dynamorio_launcher, record_dynamorio_launched_image};
use anyhow::anyhow;
use hermit::{Command, DetConfig, Error, ExitStatus, Output};

/// Run a command with the full DynamoRIO Detcore runtime.
///
/// As on the original library route, the summary JSON path is not consumed by
/// this physical runtime. The core's shared application owns its JSON reports.
pub fn run(
    command: Command,
    config: DetConfig,
    print_summary: bool,
    _print_summary_to_json_file: &Option<PathBuf>,
) -> Result<ExitStatus, Error> {
    Ok(run_inner(command, prepare_config(config), print_summary, false)?.status)
}

/// Run a command with the full DynamoRIO Detcore runtime and capture its output.
pub fn run_with_output(
    command: Command,
    config: DetConfig,
    print_summary: bool,
    _print_summary_to_json_file: &Option<PathBuf>,
) -> Result<Output, Error> {
    run_inner(command, prepare_config(config), print_summary, true)
}

// Preserve the original public-wrapper timing: normalize before Tokio is
// constructed; the actual generic admission checks remain inside the dispatcher.
fn prepare_config(config: DetConfig) -> DetConfig {
    hermit::prepare_backend_config_with_capabilities(
        config,
        reverie_dynamorio::DynamoRioRunner::capabilities(),
    )
}

#[tokio::main(flavor = "current_thread")]
async fn run_inner(
    command: Command,
    config: DetConfig,
    print_summary: bool,
    capture_output: bool,
) -> Result<Output, Error> {
    let mut config = config;
    hermit::cpu_profile_admission::refuse_unadmitted_launch(&config, false)?;
    hermit::preemption_admission::refuse_unqualified_annotated_launch(&config)?;
    config.launch_image = hermit::launched_image(&command);
    run_dynamorio(command, config, print_summary, capture_output).await
}

fn dynamorio_client_thread_start_failed(status: &std::process::ExitStatus) -> bool {
    status.code() == Some(reverie_dynamorio::CLIENT_THREAD_START_FAILURE_EXIT_CODE)
}

async fn run_dynamorio(
    command: Command,
    config: DetConfig,
    print_summary: bool,
    capture_output: bool,
) -> Result<Output, Error> {
    if !config.sequentialize_threads {
        return Err(anyhow!(
            "the dynamorio backend requires sequentialized threads; remove \
             --no-sequentialize-threads (or --strace-only) to run under hermit-dynamorio"
        ));
    }
    // Strict schedule boundaries have never run on this backend: the
    // configuration form that once carried Detcore's configuration to the DYNAMORIO
    // runtime could not express them, which refused such a run. They stay
    // refused, by name.
    if config.strict_schedule_boundaries {
        return Err(anyhow!(
            "the dynamorio backend does not run --strict-schedule-boundaries"
        ));
    }

    let artifacts = crate::engine_artifact::NativeArtifacts::from_required_producer_input()?;
    artifacts.recheck()?;
    let result =
        run_dynamorio_with_artifacts(command, config, print_summary, capture_output, &artifacts)
            .await;
    artifacts.complete(result)
}

async fn run_dynamorio_with_artifacts(
    command: Command,
    config: DetConfig,
    print_summary: bool,
    capture_output: bool,
    artifacts: &crate::engine_artifact::NativeArtifacts,
) -> Result<Output, Error> {
    // The same Detcore config/fingerprint reaches the one core engine through
    // the coordinator frame and actual native bootstrap, never guest state.
    let drrun = artifacts.drrun().to_owned();
    let client = artifacts.client().to_owned();
    let mut config = config;
    declare_dynamorio_launcher(&mut config, &drrun)?;
    record_dynamorio_launched_image(
        &mut config,
        std::path::Path::new(command.get_program()),
        command.get_current_dir(),
        command
            .get_captured_envs()
            .get(std::ffi::OsStr::new("PATH"))
            .map(std::ffi::OsString::as_os_str),
    );
    // The coordinator's first RPC frame carries this configuration to the
    // runtime. One that cannot be serialized (a path option that is not
    // UTF-8, say) is refused here, before any guest runs.
    serde_json::to_vec(&config).map_err(|error| {
        anyhow!("failed to serialize the Detcore config for DYNAMORIO: {error}")
    })?;
    let panic_on_unsupported_syscalls = config.panic_on_unsupported_syscalls;
    // Bind native diagnostics to the controller before guest output capture.
    let diagnostics = std::fs::File::from(
        std::io::stderr()
            .as_fd()
            .try_clone_to_owned()
            .map_err(|error| anyhow!("failed to retain DYNAMORIO diagnostic stderr: {error}"))?,
    );
    let mut runner = reverie_dynamorio::DynamoRioRunner::new(&drrun, &client)
        .map_err(|error| {
            anyhow!(
                "failed to configure the DynamoRIO DYNAMORIO runner (drrun={}, client={}): {error}",
                drrun.display(),
                client.display()
            )
        })?
        .private_environment_prefix(crate::native_contract::PRIVATE_ENV_PREFIX)?
        .private_placement(crate::native_contract::PRIVATE_PLACEMENT)
        .summary(print_summary)
        .isolated_process_group(panic_on_unsupported_syscalls)
        .tool_answers_brk(true)
        .diagnostic_file(&diagnostics)
        .map_err(|error| anyhow!("failed to configure DYNAMORIO diagnostic stderr: {error}"))?;
    drop(diagnostics);
    if panic_on_unsupported_syscalls {
        runner = runner.client_argument("-panic-on-unsupported-syscalls");
    }

    let program = command.get_program().to_owned();
    let mut environment = command.get_captured_envs();
    // The runtime checks this against its own build before it reads the
    // configuration frame (`crate::native_contract::CONFIG_FINGERPRINT_ENV`); the
    // runner's private prefix keeps it from the guest.
    environment.insert(
        crate::native_contract::CONFIG_FINGERPRINT_ENV.into(),
        detcore::config_wire_fingerprint().into(),
    );
    let guest = command.into_std_lossy();
    tracing::info!(
        target: "hermit::dynamorio",
        program = ?program,
        drrun = %drrun.display(),
        client = %client.display(),
        "launching guest through reverie-dynamorio with the shared core Detcore engine",
    );

    artifacts.recheck()?;
    let (status, stdout, stderr, mut global) = if capture_output {
        let launch = || {
            runner.output_with_environment_and_global::<detcore::GlobalState>(
                &guest,
                &environment,
                config.clone(),
            )
        };
        let (mut output, mut global) = launch()
            .await
            .map_err(|error| anyhow!("failed to launch drrun ({}): {error}", drrun.display()))?;
        if dynamorio_client_thread_start_failed(&output.status) {
            tracing::warn!(
                target: "hermit::dynamorio",
                "DynamoRIO client thread failed before guest start; retrying once",
            );
            global.force_shutdown_with_error();
            global.cancel_internal_scheduler().await;
            global.clean_up(false, &None).await.map_err(|error| {
                error.context(format!(
                    "DYNAMORIO cleanup after observed guest status {:?}",
                    output.status
                ))
            })?;
            artifacts.recheck()?;
            (output, global) = launch().await.map_err(|error| {
                anyhow!("failed to launch drrun ({}): {error}", drrun.display())
            })?;
        }
        (output.status, output.stdout, output.stderr, global)
    } else {
        let launch = || {
            runner.status_with_environment_and_global::<detcore::GlobalState>(
                &guest,
                &environment,
                config.clone(),
            )
        };
        let (mut status, mut global) = launch()
            .await
            .map_err(|error| anyhow!("failed to launch drrun ({}): {error}", drrun.display()))?;
        if dynamorio_client_thread_start_failed(&status) {
            tracing::warn!(
                target: "hermit::dynamorio",
                "DynamoRIO client thread failed before guest start; retrying once",
            );
            global.force_shutdown_with_error();
            global.cancel_internal_scheduler().await;
            global.clean_up(false, &None).await.map_err(|error| {
                error.context(format!(
                    "DYNAMORIO cleanup after observed guest status {status:?}"
                ))
            })?;
            artifacts.recheck()?;
            (status, global) = launch().await.map_err(|error| {
                anyhow!("failed to launch drrun ({}): {error}", drrun.display())
            })?;
        }
        (status, Vec::new(), Vec::new(), global)
    };

    if !status.success() {
        global.force_shutdown_with_error();
        global.cancel_internal_scheduler().await;
    }
    global
        .clean_up(print_summary, &None)
        .await
        .map_err(|error| {
            error.context(format!(
                "DYNAMORIO cleanup after observed guest status {status:?}"
            ))
        })?;
    Ok(Output {
        status: status.into(),
        stdout,
        stderr,
    })
}

#[cfg(test)]
#[path = "placement_probe.rs"]
mod placement_probe;

#[cfg(test)]
#[path = "library_tests.rs"]
mod tests;
