/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Launcher for the shared counter1 tool on the DynamoRIO backend.

use reverie_examples::tools::counter1 as counter1_tool;

use reverie_dynamorio_examples::backend;

use clap::Parser;
use counter1_tool::CounterLocal;
use reverie_util::CommonToolArguments;

/// Run PROGRAM under the counter1 tool on DynamoRIO.
#[derive(Debug, Parser)]
struct Args {
    /// Verified Counter1 caller manifest; defaults to the profile-specific pair.
    #[clap(long, env = "REVERIE_DYNAMORIO_CALLER_ARTIFACT")]
    dynamorio_artifact: Option<std::path::PathBuf>,

    #[clap(flatten)]
    common: CommonToolArguments,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let path = match &args.dynamorio_artifact {
        Some(path) => path.clone(),
        None => std::env::current_exe()?
            .parent()
            .ok_or_else(|| anyhow::anyhow!("counter1 executable has no profile directory"))?
            .join("reverie-dynamorio-callers/counter1/manifest.json"),
    };
    let caller = reverie_dynamorio::CallerArtifact::load(&path).map_err(|error| {
        anyhow::anyhow!(
            "cannot load DynamoRIO caller artifact {}: {error}; run reverie-dynamorio/scripts/build-client.sh --caller-manifest reverie-dynamorio-examples/counter1-dynamorio-runtime/Cargo.toml --profile <this binary's Cargo profile>",
            path.display()
        )
    })?;
    let log_guard = args.common.init_tracing();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let run = runtime
        .block_on(backend::run_caller::<CounterLocal>(
            &args.common,
            (),
            &caller,
        ))
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    if let Some((stdout, stderr)) = &run.captured {
        use std::io::Write as _;
        std::io::stdout().write_all(stdout)?;
        std::io::stderr().write_all(stderr)?;
    }
    eprintln!("counter1-global syscalls={}", run.global_state.total());
    drop(log_guard);
    run.status.raise_or_exit()
}
