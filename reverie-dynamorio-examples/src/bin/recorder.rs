/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Run a program under the pass-through recorder (`recorder_tool.rs`) on a
//! chosen backend, or natively, and write every syscall it made with the
//! result it got, in a form that compares across backends and with native
//! Linux (`reverie-examples/e2e/compare.rs`).
//!
//! `--backend native` records the same with plain ptrace syscall stops, with
//! no Reverie at all. A backend that cannot host the recorder refuses by
//! name: the record is not written and the refusal is the last line of
//! stderr, starting `REFUSED:`.

use std::ffi::OsString;
use std::path::PathBuf;

use clap::Parser;

use reverie_dynamorio_examples::backend;
use reverie_examples::host::run_native;
use reverie_examples::tools::recorder_tool;

use recorder_tool::Recorder;
use recorder_tool::report;
use recorder_tool::reverie_events;

/// Where to run the program.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
enum Target {
    /// Natively, observed with plain ptrace syscall stops.
    Native,
    Dynamorio,
}

/// Run PROGRAM under the pass-through recorder on BACKEND and write its
/// syscalls and their results to OUTPUT.
#[derive(Debug, Parser)]
struct Opts {
    #[clap(long, value_enum)]
    backend: Target,
    /// Where to write the record (stdout is the program's).
    #[clap(long)]
    output: PathBuf,
    #[clap(value_name = "PROGRAM")]
    program: PathBuf,
    #[clap(
        value_name = "ARGS",
        trailing_var_arg = true,
        allow_hyphen_values = true
    )]
    args: Vec<OsString>,
}

fn utf8(value: &std::ffi::OsStr) -> anyhow::Result<String> {
    value
        .to_str()
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("non-UTF-8 program or argument {value:?}"))
}

fn main() -> anyhow::Result<()> {
    let opts = Opts::parse();
    let report = if opts.backend == Target::Native {
        run_native(&opts.program, &opts.args)?
    } else {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        // The host's environment, as for a native run.
        let common = reverie_util::CommonToolArguments {
            log_file: None,
            no_host_envs: false,
            envs: Vec::new(),
            trace_syscalls: false,
            program: utf8(opts.program.as_os_str())?,
            program_args: opts
                .args
                .iter()
                .map(|arg| utf8(arg))
                .collect::<anyhow::Result<_>>()?,
        };
        let run = runtime.block_on(backend::run::<Recorder>(&common, (), recorder_tool::NAME));
        match run {
            Ok(run) => {
                // The program's output, where the backend captured it, goes
                // where it would have gone.
                if let Some((stdout, stderr)) = &run.captured {
                    use std::io::Write as _;
                    std::io::stdout().write_all(stdout)?;
                    std::io::stderr().write_all(stderr)?;
                }
                let events = run.global_state.events.into_inner().unwrap();
                report(run.exit_code, reverie_events(events))
            }
            Err(error) => anyhow::bail!("{error}"),
        }
    };
    std::fs::write(&opts.output, report.text())?;
    Ok(())
}
