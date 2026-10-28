/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * This source code is licensed under the BSD-style license in LICENSE.
 */

//! Launch the shared Strace Tool through an explicitly paired native caller.

use clap::Parser;
use reverie_examples::tools::strace::{Config, Filter, Strace};
use reverie_util::CommonToolArguments;

#[derive(Debug, Parser)]
struct Options {
    /// Verified caller manifest; the profile's Strace caller is the default.
    #[clap(long, env = "REVERIE_DYNAMORIO_CALLER_ARTIFACT")]
    dynamorio_artifact: Option<std::path::PathBuf>,

    #[clap(flatten)]
    common: CommonToolArguments,

    /// Trace only this set of system calls (all calls by default).
    #[clap(long)]
    trace: Vec<Filter>,
}

fn main() -> anyhow::Result<()> {
    let options = Options::parse();
    let path = match &options.dynamorio_artifact {
        Some(path) => path.clone(),
        None => std::env::current_exe()?
            .parent()
            .ok_or_else(|| anyhow::anyhow!("strace executable has no profile directory"))?
            .join("reverie-dynamorio-callers/strace/manifest.json"),
    };
    let caller = reverie_dynamorio::CallerArtifact::load(&path).map_err(|error| {
        anyhow::anyhow!(
            "cannot load DynamoRIO caller artifact {}: {error}; run reverie-dynamorio/scripts/build-client.sh --caller-manifest reverie-dynamorio-examples/strace-dynamorio-runtime/Cargo.toml --profile <this binary's Cargo profile>",
            path.display()
        )
    })?;
    let config = Config {
        filters: options.trace,
    };
    let log_guard = options.common.init_tracing();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let run = runtime.block_on(reverie_dynamorio_examples::backend::run_caller::<Strace>(
        &options.common,
        config,
        &caller,
    ))?;
    if let Some((stdout, stderr)) = &run.captured {
        use std::io::Write as _;
        std::io::stdout().write_all(stdout)?;
        std::io::stderr().write_all(stderr)?;
    }
    let (status, _) = (run.status, run.global_state);
    drop(log_guard);
    status.raise_or_exit()
}
