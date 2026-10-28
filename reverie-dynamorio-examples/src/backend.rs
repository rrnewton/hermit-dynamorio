/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! The existing DynamoRIO physical Tool host; generic results stay in core.

use reverie::{ExitStatus, GlobalTool, Tool};
use reverie_examples::host::{Run, RunError, exit_code};
use reverie_util::CommonToolArguments;

pub async fn run<T>(
    args: &CommonToolArguments,
    config: <T::GlobalState as GlobalTool>::Config,
    tool: &str,
) -> Result<Run<T::GlobalState>, RunError>
where
    T: Tool + 'static,
    T::GlobalState: 'static,
{
    let client =
        crate::client::find_client(std::env::current_exe().ok().as_deref()).ok_or_else(|| {
            RunError::Refused(
            "the DYNAMORIO client was not found: set REVERIE_DYNAMORIO_CLIENT, or build it with \
             reverie-dynamorio/scripts/build-client.sh"
                .to_owned(),
        )
        })?;
    let runner =
        reverie_dynamorio::DynamoRioRunner::new(reverie_dynamorio::bundled_drrun_path(), client)?;
    let mut command = std::process::Command::new(&args.program);
    command.args(&args.program_args);
    if args.no_host_envs {
        command.env_clear();
        command.env("PATH", "/bin/:/usr/bin");
    }
    command.envs(args.envs.iter().cloned());
    command.env(reverie_dynamorio::TOOL_ENV, tool);
    let (output, global_state) = runner
        .output_with_global::<T::GlobalState>(&command, config)
        .await?;
    use std::os::unix::process::ExitStatusExt as _;
    let status = ExitStatus::from_raw(output.status.into_raw());
    Ok(Run {
        status,
        exit_code: exit_code(status),
        global_state,
        captured: Some((output.stdout, output.stderr)),
    })
}

pub async fn run_caller<T>(
    args: &CommonToolArguments,
    config: <T::GlobalState as GlobalTool>::Config,
    artifact: &reverie_dynamorio::CallerArtifact,
) -> Result<Run<T::GlobalState>, RunError>
where
    T: Tool + 'static,
    T::GlobalState: 'static,
{
    let runner = reverie_dynamorio::DynamoRioRunner::new(
        reverie_dynamorio::bundled_drrun_path(),
        artifact.native_client(),
    )?;
    let mut command = std::process::Command::new(&args.program);
    command.args(&args.program_args);
    if args.no_host_envs {
        command.env_clear();
        command.env("PATH", "/bin/:/usr/bin");
    }
    command.envs(args.envs.iter().cloned());
    let (output, global_state) = runner
        .output_with_caller_tool::<T::GlobalState>(&command, config)
        .await
        .map_err(|error| {
            if let Some(output) = reverie_dynamorio::failed_run_output(&error) {
                use std::io::Write as _;
                // These are the already captured launch streams. A separately
                // configured diagnostic file remains separate; no guest fd is
                // changed and the failed launch remains an error.
                let stdout = std::io::stdout().write_all(&output.stdout);
                let stderr = std::io::stderr().write_all(&output.stderr);
                if let Err(report_error) = stdout.and(stderr) {
                    return anyhow::Error::new(error).context(format!(
                        "could not forward captured caller output: {report_error}"
                    ));
                }
            }
            anyhow::Error::new(error)
        })?;
    use std::os::unix::process::ExitStatusExt as _;
    let status = ExitStatus::from_raw(output.status.into_raw());
    Ok(Run {
        status,
        exit_code: exit_code(status),
        global_state,
        captured: Some((output.stdout, output.stderr)),
    })
}
