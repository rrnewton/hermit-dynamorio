/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! DynamoRIO's existing full Detcore physical path behind Hermit's shared app.
//!
//! This local prototype retains current `detcore-dynamorio` / `reverie-dynamorio` artifacts.
//! It neither implements a command driver nor decides verification outcomes.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::env;
use std::ffi::OsStr;
use std::ffi::OsString;
use std::fs;
use std::io::IsTerminal as _;
use std::io::Read;
use std::io::Seek as _;
use std::io::SeekFrom;
use std::io::Write;
use std::os::fd::AsFd;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::ExitStatusExt as _;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command as StdCommand;
use std::process::Output;
use std::sync::Arc;

use detcore::Config;
use detcore_model::backend_engagement::BackendEngagement;
use detcore_model::backend_engagement::BackendEngagementReport;
use hermit::Error;
use hermit::ExitStatus;
use hermit::app::backend_hook::{
    BackendAdapter, ContainerScope, CoordinatorTracing, EvidenceRequirement, Execute, Execution,
    Mode, NativeFacts, Observation, PairEvidence, PhysicalRun, Prepare, RunLatches,
    SavedNativeEvidence, SavedNativeEvidenceRef, VerificationCapture, VerificationLogTransport,
    VerificationRun,
};
use reverie::process::Bind;
use reverie_dynamorio::DynamoRioEvidence;
use reverie_dynamorio::DynamoRioEvidenceLogLevel;
use reverie_dynamorio::DynamoRioRunner;
use reverie_dynamorio::DynamoRioStatsSink;
use reverie_dynamorio::backend_stats::DynamoRioBackendStatsSnapshot;
use reverie_dynamorio::backend_stats::DynamoRioStatsCapture;
use tracing::metadata::LevelFilter;

/// One statically selected physical implementation; the shared app owns entry,
/// admission, logging policy, common comparison and report publication.
pub struct DynamoRioAdapter;

pub struct Session(SessionState);

enum SessionState {
    Ordinary {
        invocation: Option<DynamoRioInvocation>,
        tracing: CoordinatorTracing,
        summary: bool,
        engagement: Option<PathBuf>,
        logging: Option<LevelFilter>,
        log_file: Option<PathBuf>,
        _stdin: Option<std::fs::File>,
    },
    Verification {
        mode: Mode,
        verification: DynamoRioVerification,
        saved_executed: bool,
    },
}

pub struct Pending(DynamoRioRunCompletion);

impl BackendAdapter for DynamoRioAdapter {
    const NAME: &'static str = "dynamorio";
    const CAPABILITIES: reverie::BackendCapabilities = DynamoRioRunner::capabilities();
    const EVIDENCE: EvidenceRequirement = EvidenceRequirement::NativeCounters;
    const LOG_TRANSPORT: VerificationLogTransport = VerificationLogTransport::NativeEvidence;
    const NATIVE_IDENTITY_DIAGNOSTIC: Option<&'static str> =
        Some("dynamorio path confirmed: DynamoRIO client reported tool=Detcore");
    const NONDEFAULT_CPU_PROFILE_REFUSAL: &'static str = "Detcore answers DynamoRIO's CPUID, but the DynamoRIO runtime's configuration carries no CPU profile, so it answers only the x86-64-v2 table";

    type Options = ();
    type Session = Session;
    type Pending = Pending;
    type Facts = DynamoRioRunFacts;

    fn validate_mode(&self, mode: Mode) -> Result<(), Error> {
        // Declare only the modes whose existing physical path is implemented.
        // This admission hook creates no input, resource, writer or verdict.
        match mode {
            Mode::Ordinary | Mode::VerificationPair | Mode::SavedVerification => Ok(()),
        }
    }

    fn parse(&self, _: &clap::ArgMatches) -> Result<(), clap::Error> {
        Ok(())
    }

    fn prepare(&self, (): (), mut request: Prepare<'_>) -> Result<Session, Error> {
        let mode = request.mode();
        // As in the original saved-run owner, the common verification sink owns
        // requested logs in Saved mode; ordinary single runs still refuse them.
        if mode != Mode::VerificationPair
            && dynamorio_log_file_is_refused(
                request.log_file().is_some(),
                mode == Mode::SavedVerification,
            )
        {
            return Err(Error::msg(DYNAMORIO_LOG_FILE_REFUSAL));
        }
        let invocation = DynamoRioInvocation::new(
            request.program(),
            request.arguments(),
            &request.config(),
            request.environment()?,
            request.working_directory(),
            request.binds(),
            request.container(),
            request.summary_requested(),
        )?;
        if matches!(mode, Mode::VerificationPair | Mode::SavedVerification) {
            return Ok(Session(SessionState::Verification {
                mode,
                verification: invocation.verification(request.take_stdin())?,
                saved_executed: false,
            }));
        }
        Ok(Session(SessionState::Ordinary {
            invocation: Some(invocation),
            tracing: request.coordinator_tracing(),
            summary: request.summary_requested(),
            engagement: request.engagement_path().map(Path::to_owned),
            logging: request.logging(),
            log_file: request.log_file().map(Path::to_owned),
            _stdin: request.take_stdin(),
        }))
    }

    fn execute(
        &self,
        session: &mut Session,
        request: Execute<'_>,
    ) -> Result<Execution<Pending>, Error> {
        match &mut session.0 {
            SessionState::Ordinary {
                invocation,
                tracing,
                summary,
                engagement,
                logging,
                log_file,
                ..
            } => {
                anyhow::ensure!(
                    request.mode() == Mode::Ordinary,
                    "backend operation mode changed"
                );
                let invocation = invocation.take().ok_or_else(|| {
                    Error::msg("single physical backend operation was already executed")
                })?;
                // Core owns the original coordinator WARN cap and its guard.
                // The requested guest logging channel remains separate.
                let _tracing = tracing.clone().enter();
                invocation
                    .run(
                        *summary,
                        engagement.as_deref(),
                        *logging,
                        log_file.as_deref(),
                    )
                    .map(Execution::Finished)
            }
            SessionState::Verification {
                mode,
                verification,
                saved_executed,
            } => {
                anyhow::ensure!(request.mode() == *mode, "backend operation mode changed");
                if *mode == Mode::SavedVerification {
                    anyhow::ensure!(
                        !*saved_executed,
                        "single physical backend operation was already executed"
                    );
                    *saved_executed = true;
                }
                verification
                    .run(*mode, request.into_verification()?)
                    .map(Execution::Verification)
            }
        }
    }

    fn finish(
        &self,
        pending: Pending,
        observation: Observation<'_>,
    ) -> Result<DynamoRioRunFacts, Error> {
        pending.0.finish(observation.output, observation.run)
    }

    fn pair_evidence(
        &self,
        first: &DynamoRioRunFacts,
        second: &DynamoRioRunFacts,
    ) -> Result<PairEvidence, Error> {
        Ok(PairEvidence::NativeCounters {
            first: first.native_facts(),
            second: second.native_facts(),
        })
    }

    fn save_native_evidence(
        &self,
        facts: &DynamoRioRunFacts,
    ) -> Result<SavedNativeEvidence, Error> {
        Ok(SavedNativeEvidence::new(
            facts.raw.clone(),
            u64::try_from(facts.initialization_records)?,
        ))
    }

    fn restore_native_evidence(
        &self,
        evidence: SavedNativeEvidenceRef<'_>,
    ) -> Result<DynamoRioRunFacts, Error> {
        DynamoRioRunFacts::restore(evidence.raw(), evidence.initialization_records())
    }
}

/// Capture/reading/decoding belong to Reverie; this refusal policy also applies
/// to caller-retained saved runs and remains after evidence/latch checks.
fn require_dynamorio_stats(
    capture: &DynamoRioStatsCapture,
) -> Result<DynamoRioBackendStatsSnapshot, Error> {
    match capture {
        DynamoRioStatsCapture::Missing => {
            return Err(Error::msg(
                "DYNAMORIO verification did not reach a verdict: typed whole-process statistics were missing",
            ));
        }
        DynamoRioStatsCapture::Unreadable { message, .. } => {
            return Err(Error::msg(format!(
                "DYNAMORIO verification did not reach a verdict: failed to read typed whole-process statistics: {message}"
            )));
        }
        DynamoRioStatsCapture::Records(bytes) if bytes.is_empty() => {
            return Err(Error::msg(
                "DYNAMORIO verification did not reach a verdict: typed whole-process statistics were empty",
            ));
        }
        DynamoRioStatsCapture::Records(_) => {}
    }
    let snapshot = capture.snapshot().map_err(|error| Error::msg(format!(
        "DYNAMORIO verification did not reach a verdict: typed whole-process statistics were unreadable: {error}"
    )))?;
    if snapshot.process_images() == 0 {
        return Err(Error::msg(
            "DYNAMORIO verification did not reach a verdict: typed whole-process statistics contained no process records",
        ));
    }
    if snapshot.counted_branches() == 0
        || snapshot.intercepted_syscalls() == 0
        || snapshot.rewritten_syscalls() > snapshot.intercepted_syscalls()
    {
        return Err(Error::msg(
            "DYNAMORIO typed native callback counters are inconsistent",
        ));
    }
    Ok(snapshot)
}

/// Render the native DYNAMORIO counters as a labeled `--summary` block.
///
/// These are the counters the DynamoRIO client already emits at exit; hermit
/// simply surfaces them on the normal run path. Labels are deliberately honest
/// about what each number is: `branches` is Detcore's deterministic
/// counted-branch clock (cbr/ubr/call/return retired), **not** a count of
/// translated basic blocks, and `memory hash` is the client's observed
/// guest-memory digest, not a Detcore RunSummary field.
fn format_dynamorio_stats(summary: &DynamoRioBackendStatsSnapshot) -> String {
    format!(
        "=== DYNAMORIO backend stats (native DynamoRIO client) ===\n\
         counted branches (deterministic branch clock): {}\n\
         syscalls intercepted:                          {}\n\
         syscall instructions rewritten:                {}\n\
         stdin (fd 0) reads:                            {}\n\
         observed guest-memory hash:                    {}\n",
        summary.counted_branches(),
        summary.intercepted_syscalls(),
        summary.rewritten_syscalls(),
        summary.stdin_reads(),
        format_args!("{:016x}", summary.memory_hash_fold()),
    )
}

fn write_dynamorio_engagement(
    path: &Path,
    snapshot: &DynamoRioBackendStatsSnapshot,
) -> Result<(), Error> {
    let report = BackendEngagementReport::new(BackendEngagement::Dbt {
        counted_branches: snapshot.counted_branches(),
    });
    report.validate().map_err(Error::msg)?;
    let mut bytes = serde_json::to_vec(&report)?;
    bytes.push(b'\n');
    fs::write(path, bytes).map_err(|error| {
        Error::msg(format!(
            "writing DYNAMORIO engagement {}: {error}",
            path.display()
        ))
    })
}

/// The exit code with which the DYNAMORIO native client ends a process on a backend
/// or Tool failure (`exit_runtime_tree(101)` in reverie-dynamorio's client).
const DYNAMORIO_BACKEND_FAILURE_EXIT_CODE: i32 = 101;

/// Prints and records a single run's statistics as requested, and returns them
/// when they could be read. A run always captures them: they are also how a
/// backend failure in a guest child is found (see [`fail_on_dynamorio_backend_failure`]).
fn finish_single_run_dynamorio_stats(
    capture: &DynamoRioStatsCapture,
    summary: bool,
    engagement_json: Option<&Path>,
) -> Result<Option<DynamoRioBackendStatsSnapshot>, Error> {
    match require_dynamorio_stats(capture) {
        Ok(stats) => {
            if summary {
                eprint!("{}", format_dynamorio_stats(&stats));
            }
            if let Some(path) = engagement_json {
                write_dynamorio_engagement(path, &stats)?;
            }
            Ok(Some(stats))
        }
        Err(error) if engagement_json.is_some() => Err(error),
        Err(error) => {
            if summary {
                eprintln!(":: DYNAMORIO summary unavailable: {error}");
            }
            eprintln!(":: WARNING: DYNAMORIO execution statistics are unavailable: {error}");
            Ok(None)
        }
    }
}

/// `status`, or a failed status when any process image of the run ended on a
/// backend or Tool failure.
///
/// The native client ends such a process with exit code 101. When the guest
/// tree has no isolated process group (`--allow-unsupported-syscalls` without
/// `--verify`), that ends only the failing process, and a parent that ignores
/// its child's status can still finish the run with any status, 0 included.
/// The failing image flags its stats record, so the run fails here instead.
fn fail_on_dynamorio_backend_failure(
    status: ExitStatus,
    capture: Option<&DynamoRioStatsCapture>,
) -> ExitStatus {
    // A complete failure-only record remains authoritative even if the root
    // was killed before it could publish execution counters. It cannot prove
    // engagement or verification completeness; require_dynamorio_stats owns those.
    let failures = capture
        .and_then(|capture| capture.snapshot().ok())
        .map_or(0, |stats| stats.backend_failures());
    if failures == 0 || status == ExitStatus::Exited(DYNAMORIO_BACKEND_FAILURE_EXIT_CODE) {
        return status;
    }
    eprintln!(
        "hermit: [dynamorio backend] {failures} guest process(es) ended on a DYNAMORIO backend or Tool \
         failure (exit {DYNAMORIO_BACKEND_FAILURE_EXIT_CODE}), but the guest tree finished with \
         {status:?}; failing the run with exit {DYNAMORIO_BACKEND_FAILURE_EXIT_CODE}. The failing \
         process's diagnostic is above."
    );
    ExitStatus::Exited(DYNAMORIO_BACKEND_FAILURE_EXIT_CODE)
}

#[derive(Debug, Eq, PartialEq)]
struct DynamoRioGuestCommand {
    program: PathBuf,
    args: Vec<OsString>,
}

fn executable_on_path(program: &OsStr, path: &OsStr, cwd: &Path) -> Option<PathBuf> {
    executable_path_candidate(program, path, cwd).map(|candidate| cwd.join(candidate))
}

fn executable_path_candidate(program: &OsStr, path: &OsStr, cwd: &Path) -> Option<PathBuf> {
    env::split_paths(path)
        .map(|directory| directory.join(program))
        .find(|candidate| {
            cwd.join(candidate).metadata().is_ok_and(|metadata| {
                metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
            })
        })
}

/// Resolve the simple `#!/usr/bin/env PROGRAM` form before DynamoRIO starts.
///
/// DynamoRIO follows an absolute exec target correctly, but its copied exec path
/// can wait indefinitely when `env` later resolves a bare target through PATH.
/// Keep `env` in the process chain and replace only its single plain program
/// token with the equivalent absolute PATH match. More complex `env` forms are
/// left unchanged for the normal launcher rather than partially interpreting
/// options or assignments here.
fn prepare_dynamorio_guest_command(
    program: &Path,
    args: &[OsString],
    path: Option<&OsStr>,
    cwd: &Path,
) -> DynamoRioGuestCommand {
    let unchanged = || DynamoRioGuestCommand {
        program: program.to_path_buf(),
        args: args.to_vec(),
    };
    // Command resolves relative paths and PATH entries after changing cwd.
    // Inspect that same file in the physical run's already-established mount
    // namespace; the caller's /test may have been hidden by its fresh tmpfs.
    let script_path =
        if program.is_absolute() || program.as_os_str().as_encoded_bytes().contains(&b'/') {
            program.to_path_buf()
        } else if let Some(found) =
            path.and_then(|path| executable_path_candidate(program.as_os_str(), path, cwd))
        {
            found
        } else {
            return unchanged();
        };
    let Some(shebang) = hermit::Shebang::new(cwd.join(&script_path)) else {
        return unchanged();
    };
    let (interpreter, interpreter_args) = shebang.into_parts();
    if interpreter.file_name() != Some(OsStr::new("env")) || interpreter_args.len() != 1 {
        return unchanged();
    }

    let target = &interpreter_args[0];
    let target_bytes = std::os::unix::ffi::OsStrExt::as_bytes(target.as_os_str());
    if target_bytes.starts_with(b"-")
        || target_bytes.contains(&b'=')
        || target_bytes.contains(&b'/')
    {
        return unchanged();
    }
    let Some(target) = path.and_then(|path| executable_on_path(target, path, cwd)) else {
        return unchanged();
    };

    let mut resolved_args = Vec::with_capacity(args.len() + 2);
    resolved_args.push(target.into_os_string());
    // execvp supplies the selected PATH candidate to a script interpreter.
    // Explicit paths retain their spelling; a bare name must not discard the
    // directory that actually selected its script.
    resolved_args.push(script_path.into_os_string());
    resolved_args.extend_from_slice(args);
    DynamoRioGuestCommand {
        program: interpreter,
        args: resolved_args,
    }
}

// run_dynamorio supplies program/args, an exact environment represented by values
// and explicit removals, and cwd.
// Reconstruct those fields inside the physical run, before DynamoRioRunner performs
// its own command reconstruction. Stdin and evidence descriptors remain owned
// by the existing runner adapters.
fn prepare_dynamorio_physical_command(guest: &StdCommand) -> Result<StdCommand, Error> {
    let environment = guest
        .get_envs()
        .filter_map(|(key, value)| value.map(|value| (key.to_owned(), value.to_owned())))
        .collect::<BTreeMap<_, _>>();
    let args = guest.get_args().map(OsStr::to_owned).collect::<Vec<_>>();
    let cwd = env::current_dir()?;
    let cwd = guest
        .get_current_dir()
        .map_or(cwd.clone(), |workdir| cwd.join(workdir));
    let prepared = prepare_dynamorio_guest_command(
        Path::new(guest.get_program()),
        &args,
        environment.get(OsStr::new("PATH")).map(OsString::as_os_str),
        &cwd,
    );
    let mut command = StdCommand::new(prepared.program);
    command.args(prepared.args);
    // Do not call env_clear here: std::Command then erases removal entries,
    // so DynamoRioRunner cannot see them when it reconstructs from get_envs().
    // DynamoRioRunner copies explicit removals as well as values. Preserve the raw
    // command's removal entries instead of re-enumerating ambient variables.
    for (name, value) in guest.get_envs() {
        if let Some(value) = value {
            command.env(name, value);
        } else {
            command.env_remove(name);
        }
    }
    if let Some(workdir) = guest.get_current_dir() {
        command.current_dir(workdir);
    }
    Ok(command)
}

fn apply_exact_environment(command: &mut StdCommand, environment: &BTreeMap<OsString, OsString>) {
    // DynamoRioRunner reconstructs its launcher command from Command::get_envs(),
    // which cannot expose env_clear(). Make removals explicit so --base-env
    // does not accidentally inherit the Hermit launcher's environment.
    for (key, _) in env::vars_os() {
        if !environment.contains_key(&key) {
            command.env_remove(key);
        }
    }
    command.envs(environment);
}

fn apply_dynamorio_workdir(command: &mut StdCommand, workdir: Option<&Path>) {
    if let Some(workdir) = workdir {
        command.current_dir(workdir);
    }
}
// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(PR-644): Review inherited DYNAMORIO policy descriptors and bounded reports.
struct InstalledFd {
    target: i32,
    backup: Option<i32>,
    original_flags: Option<i32>,
}

impl InstalledFd {
    fn install(source: i32, target: i32) -> std::io::Result<Self> {
        // Keep the backup above the reserved transport descriptor so installing the target
        // cannot overwrite its backup.
        let backup = unsafe {
            libc::fcntl(
                target,
                libc::F_DUPFD_CLOEXEC,
                crate::native_contract::UNSUPPORTED_SYSCALL_REPORT_FD + 1,
            )
        };
        let backup = if backup == -1 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EBADF) {
                None
            } else {
                return Err(error);
            }
        } else {
            Some(backup)
        };
        let original_flags = if let Some(backup_fd) = backup {
            let flags = unsafe { libc::fcntl(target, libc::F_GETFD) };
            if flags == -1 {
                let error = std::io::Error::last_os_error();
                let _ = unsafe { libc::close(backup_fd) };
                return Err(error);
            }
            Some(flags)
        } else {
            None
        };
        let installed = Self {
            target,
            backup,
            original_flags,
        };
        if unsafe { libc::dup2(source, target) } == -1 {
            return Err(std::io::Error::last_os_error());
        }
        if unsafe { libc::fcntl(target, libc::F_SETFD, 0) } == -1 {
            let error = std::io::Error::last_os_error();
            drop(installed);
            return Err(error);
        }
        Ok(installed)
    }
}

impl Drop for InstalledFd {
    fn drop(&mut self) {
        if let Some(backup) = self.backup {
            let _ = unsafe { libc::dup2(backup, self.target) };
            if let Some(flags) = self.original_flags {
                let _ = unsafe { libc::fcntl(self.target, libc::F_SETFD, flags) };
            }
            let _ = unsafe { libc::close(backup) };
        } else {
            let _ = unsafe { libc::close(self.target) };
        }
    }
}

struct DynamoRioUnsupportedSyscallReport {
    reader: std::fs::File,
    writer: Arc<std::fs::File>,
    _report_fd: InstalledFd,
}

impl DynamoRioUnsupportedSyscallReport {
    fn new() -> std::io::Result<Self> {
        let mut descriptors = [-1; 2];
        let result =
            unsafe { libc::pipe2(descriptors.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) };
        if result == -1 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: pipe2 initialized both descriptors, transferring their ownership here.
        let reader = unsafe { std::fs::File::from_raw_fd(descriptors[0]) };
        let writer = unsafe { std::fs::File::from_raw_fd(descriptors[1]) };
        let report_fd = InstalledFd::install(
            writer.as_raw_fd(),
            crate::native_contract::UNSUPPORTED_SYSCALL_REPORT_FD,
        )?;
        Ok(Self {
            reader,
            writer: Arc::new(writer),
            _report_fd: report_fd,
        })
    }

    fn emit(&mut self) -> std::io::Result<()> {
        const MAX_REPORT_BYTES: usize = 1024 * 1024;
        let mut contents = Vec::new();
        let mut buffer = [0_u8; 4096];
        loop {
            match self.reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    if contents.len() + read > MAX_REPORT_BYTES {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "DYNAMORIO unsupported-syscall report exceeded 1 MiB",
                        ));
                    }
                    contents.extend_from_slice(&buffer[..read]);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(error),
            }
        }
        let contents = String::from_utf8_lossy(&contents);
        let syscalls = contents
            .lines()
            .filter_map(|line| {
                if let Some(raw) = line.strip_prefix("@") {
                    let sysno = raw
                        .parse::<i32>()
                        .ok()
                        .map(reverie::syscalls::Sysno::from)?;
                    detcore::is_unsupported_syscall(sysno).then(|| sysno.to_string())
                } else if !line.is_empty()
                    && line.len() <= 64
                    && line
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                {
                    Some(line.to_owned())
                } else {
                    None
                }
            })
            .take(512)
            .collect::<BTreeSet<_>>();
        if let Some(message) = detcore::format_unsupported_syscall_warning(&syscalls) {
            eprintln!("WARNING: {message}");
        }
        Ok(())
    }
}

impl Drop for DynamoRioUnsupportedSyscallReport {
    fn drop(&mut self) {
        if let Err(error) = self.emit() {
            eprintln!("WARNING: failed to read DYNAMORIO unsupported-syscall report: {error}");
        }
    }
}

struct TeeReader<R, W> {
    input: R,
    replay: W,
}

impl<R: Read, W: Write> Read for TeeReader<R, W> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let read = self.input.read(buffer)?;
        self.replay.write_all(&buffer[..read])?;
        Ok(read)
    }
}

/// The common app computes the exact canonical/full-trace evidence level.
/// This maps that immutable result to the physical collector's enum only.
fn dynamorio_evidence_log_level(level: LevelFilter) -> DynamoRioEvidenceLogLevel {
    if level >= LevelFilter::TRACE {
        DynamoRioEvidenceLogLevel::Trace
    } else if level >= LevelFilter::DEBUG {
        DynamoRioEvidenceLogLevel::Debug
    } else {
        DynamoRioEvidenceLogLevel::Info
    }
}

fn decode_dynamorio_evidence(file: &mut std::fs::File) -> Result<DynamoRioEvidence, Error> {
    file.seek(SeekFrom::Start(0))?;
    let mut encoded = Vec::new();
    file.read_to_end(&mut encoded)?;
    if encoded.is_empty() {
        return Err(Error::msg("DYNAMORIO canonical evidence was empty"));
    }
    crate::native_evidence::decode_evidence(&encoded).map_err(|error| {
        Error::msg(format!(
            "DYNAMORIO canonical evidence was malformed or truncated: {error}"
        ))
    })
}

/// The latches of one DYNAMORIO run. Detcore runs in the guest's DynamoRIO client,
/// so its determinism-loss latch is read from the run's evidence: the client
/// writes a record with target [`detcore::detlog::DETERMINISM_LOSS_RECORD_TARGET`]
/// at process exit when its latch is set. The reason is the record's text after
/// the target, escaped as every evidence record is.
fn dynamorio_run_latches(evidence: &DynamoRioEvidence) -> RunLatches {
    let marker = format!(
        " ERROR {}: ",
        detcore::detlog::DETERMINISM_LOSS_RECORD_TARGET
    );
    let determinism_loss = evidence.all_records().iter().find_map(|record| {
        let text = String::from_utf8_lossy(record);
        text.split_once(&marker)
            .map(|(_, reason)| reason.trim_end_matches('\n').to_owned())
    });
    RunLatches {
        determinism_loss,
        log_sink_failure: None,
    }
}

/// Write the comparison log for one DYNAMORIO run from its decoded evidence.
///
/// The log holds `DynamoRioEvidence::all_records()`: every authenticated record in
/// stream order, with each process-image initialization record where the
/// Reverie collector placed it, immediately before that image's first comparable
/// record (or at the end of the stream if the image wrote none). That is exactly
/// the stream the `all_records_v1` envelope
/// compares and counts. The refusal guards count `DynamoRioEvidence::records()`
/// instead, as described on `materialize_dynamorio_comparison_log`.
fn materialize_dynamorio_evidence_log(
    evidence: &DynamoRioEvidence,
    log: std::fs::File,
    path: &Path,
) -> Result<usize, Error> {
    let published = evidence.all_records();
    materialize_dynamorio_comparison_log(evidence.records(), &published, log, path)
}

/// Refuse evidence without guest records, then write `published` to the log.
///
/// `guest_records` are the decoded records other than initialization records,
/// and `published` is the complete stream that is compared. Every stream holds
/// at least one initialization record, which is an INFO record, so a guard over
/// `published` could never fire: a run whose guest produced no evidence would
/// reach the verdict with nonzero compared INFO counts. That is the gap
/// https://github.com/rrnewton/hermit/pull/2356 closed, so both guards here
/// count `guest_records`. Returns the number of INFO records in `published`.
fn materialize_dynamorio_comparison_log(
    guest_records: &[Vec<u8>],
    published: &[&[u8]],
    mut log: std::fs::File,
    path: &Path,
) -> Result<usize, Error> {
    if guest_records.is_empty() {
        return Err(Error::msg(
            "DYNAMORIO canonical evidence contained no records",
        ));
    }
    // Detcore counts INFO records by parsing a log file, so the guest records
    // are written and counted on their own first. The published stream then
    // replaces them and is counted by the same parser.
    let mut write_and_count = |records: &[&[u8]]| -> Result<usize, Error> {
        write_dynamorio_comparison_records(&mut log, path, records)?;
        detcore::logdiff::write_canonical_info(path, &mut std::io::sink()).map_err(|error| {
            Error::msg(format!(
                "DYNAMORIO canonical evidence did not contain a valid log stream: {error}"
            ))
        })
    };
    let guest_records: Vec<&[u8]> = guest_records.iter().map(Vec::as_slice).collect();
    if write_and_count(guest_records.as_slice())? == 0 {
        return Err(Error::msg(
            "DYNAMORIO canonical evidence contained no INFO records",
        ));
    }
    write_and_count(published)
}

/// Replace the contents of the DYNAMORIO comparison log with `records`, one record
/// per line, and check that none was lost or split on the way.
fn write_dynamorio_comparison_records(
    log: &mut std::fs::File,
    path: &Path,
    records: &[&[u8]],
) -> Result<(), Error> {
    log.set_len(0)?;
    log.seek(SeekFrom::Start(0))?;
    for record in records {
        let Some(payload) = record.strip_suffix(b"\n") else {
            return Err(Error::msg(
                "DYNAMORIO canonical evidence record was missing its terminal newline",
            ));
        };
        if payload.contains(&b'\n') || payload.contains(&b'\r') {
            return Err(Error::msg(
                "DYNAMORIO canonical evidence record contained an embedded line boundary",
            ));
        }
        log.write_all(record)?;
    }
    log.flush()?;

    // The verdict publishes `all_records_v1`, so nothing between Reverie's
    // authenticated decoder and this log may select records: every record,
    // initialization records included, must reach the log in stream order.
    // Filtering here would be an undisclosed second selection. Each record was
    // checked above to hold no embedded line boundary, so one record is one line
    // and this count is exact.
    let materialized = std::fs::read(path)
        .map_err(|error| {
            Error::msg(format!(
                "DYNAMORIO canonical evidence log unreadable: {error}"
            ))
        })?
        .iter()
        .filter(|byte| **byte == b'\n')
        .count();
    if materialized != records.len() {
        return Err(Error::msg(format!(
            "DYNAMORIO canonical evidence log holds {materialized} records but {} were decoded; the \
             comparison publishes the all_records_v1 envelope and must not drop any",
            records.len()
        )));
    }
    Ok(())
}

fn dynamorio_verification_output(output: Output) -> reverie::process::Output {
    reverie::process::Output {
        status: process_status(output.status),
        stdout: output.stdout,
        stderr: output.stderr,
    }
}

/// The DYNAMORIO `--log-file` refusal message, named so a test can assert the exact text
/// rather than a paraphrase of it.
const DYNAMORIO_LOG_FILE_REFUSAL: &str =
    "DYNAMORIO --log-file is unavailable on the ordinary single-run adapter";

/// Is `--log-file` refused for this DYNAMORIO invocation?
///
/// ⚠️ EXTRACTED SO THE POLICY IS DEFENDED. Before this, `git grep 'log-file is
/// unavailable'` returned exactly ONE hit tree-wide -- the error string itself --
/// with no test asserting when it fires. An undefended refusal can be reversed by
/// accident, and there is a live proposal to reverse this one: https://github.com/rrnewton/hermit/issues/1689's first
/// claim implements DYNAMORIO `--log-file`, and main adopted the opposite policy in
/// `f0584c1aac` about four minutes after that branch forked, inside a commit about
/// verification verdicts rather than as the subject of a decision.
///
/// This does NOT take a side on which policy is right -- that is an owner ruling.
/// It pins what main does TODAY, so reversing it means deleting a test, which is
/// visible in a diff, instead of editing one condition, which is not.
///
/// The rule: a requested log file is refused on the ordinary single-run adapter,
/// and permitted under `--verify`, where the verification adapter owns the sink.
fn dynamorio_log_file_is_refused(log_file_requested: bool, verify: bool) -> bool {
    log_file_requested && !verify
}

/// The prepared physical DYNAMORIO invocation, shared by ordinary runs and the
/// common verifier. Each execution still owns its container and coordinator.
struct DynamoRioInvocation {
    execution: DynamoRioExecution,
    runner: DynamoRioRunner,
    guest: StdCommand,
    drrun: PathBuf,
    config: Config,
    _unsupported_report: DynamoRioUnsupportedSyscallReport,
}

impl DynamoRioInvocation {
    #[allow(clippy::too_many_arguments)]
    fn new(
        program: &Path,
        args: &[String],
        config: &Config,
        mut environment: BTreeMap<OsString, OsString>,
        workdir: Option<&Path>,
        binds: Vec<(PathBuf, PathBuf)>,
        scope: ContainerScope,
        summary: bool,
    ) -> Result<Self, Error> {
        // The DYNAMORIO backend drives a single Detcore external scheduler, so it cannot
        // honor a request to relax thread sequentialization. Fail loudly rather
        // than silently ignoring the flag.
        if !config.sequentialize_threads {
            return Err(Error::msg(
                "the dynamorio backend requires sequentialized threads; \
             remove --no-sequentialize-threads (or --strace-only) to use hermit-dynamorio",
            ));
        }
        // Strict schedule boundaries have never run on this backend: the
        // configuration form that once carried Detcore's configuration to the
        // DYNAMORIO runtime could not express them, which refused such a run. They
        // stay refused, by name.
        if config.strict_schedule_boundaries {
            return Err(Error::msg(
                "the dynamorio backend does not run --strict-schedule-boundaries",
            ));
        }
        // The DYNAMORIO runtime receives this configuration through the coordinator's
        // first RPC frame, never through the guest's environment, which carries
        // only what the guest was given.
        let native_artifacts =
            Arc::new(crate::engine_artifact::NativeArtifacts::from_required_producer_input()?);
        let drrun = native_artifacts.drrun().to_owned();
        let client = native_artifacts.client().to_owned();
        let mut config = config.clone();
        crate::physical::declare_dynamorio_launcher(&mut config, &drrun)
            .map_err(|error| Error::msg(format!("{error:#}")))?;
        let config = &config;
        // The coordinator's first RPC frame carries this configuration to the
        // runtime. One that cannot be serialized (a path option that is not
        // UTF-8, say) is refused here, before any guest runs.
        serde_json::to_vec(config).map_err(|error| {
            Error::msg(format!(
                "failed to serialize the Detcore config for the DYNAMORIO backend: {error}"
            ))
        })?;
        // The fail-closed policy (https://github.com/rrnewton/hermit/pull/644) also drives process-group isolation
        // and the client flag here.
        let panic_on_unsupported_syscalls = config.panic_on_unsupported_syscalls;

        let marker = std::env::var_os(hermit_test_workdir::REQUEST_ENV);
        let isolated_workdir = hermit_test_workdir::requested_workdir(marker.as_deref())?;
        let binds = binds
            .into_iter()
            .map(|(source, target)| hermit_test_workdir::BindMount { source, target })
            .collect::<Vec<_>>();
        // A bound workdir is as fresh as the bind source the caller made for this
        // run, which is how the other backends get theirs.
        let workdir_is_bound = workdir
            .is_some_and(|workdir| binds.iter().any(|bind| workdir.starts_with(&bind.target)));
        if isolated_workdir.is_some() && workdir != isolated_workdir && !workdir_is_bound {
            return Err(Error::msg(
                "HERMIT_E2E_EMPTY_WORKDIR=/test requires --workdir=/test, or a --workdir inside a \
             --bind target, for DYNAMORIO",
            ));
        }
        // The container applies the binds; keep the existing target admission.
        for bind in &binds {
            hermit_test_workdir::check_bind_target(&bind.target)?;
        }
        let unsupported_report = DynamoRioUnsupportedSyscallReport::new()?;
        let execution = DynamoRioExecution::new(
            scope,
            isolated_workdir.is_some() && workdir == isolated_workdir,
            drrun.clone(),
        )
        .with_native_artifacts(native_artifacts)
        .reporting_unsupported_to(unsupported_report.writer.clone())
        .launching(
            program,
            workdir,
            environment.get(OsStr::new("PATH")).map(OsString::as_os_str),
        );

        // Retain the controller sink before the runner replaces the guest's
        // fd 2 with a capture pipe. Native diagnostics must not enter that pipe.
        let diagnostics =
            std::fs::File::from(std::io::stderr().as_fd().try_clone_to_owned().map_err(
                |error| {
                    Error::msg(format!(
                        "failed to retain DYNAMORIO diagnostic stderr: {error}"
                    ))
                },
            )?);
        let mut runner = DynamoRioRunner::new(&drrun, &client)
            .map_err(|error| {
                Error::msg(format!(
                    "failed to configure the DynamoRIO DYNAMORIO runner (drrun={}, client={}): {error}",
                    drrun.display(),
                    client.display()
                ))
            })?
            .private_environment_prefix(crate::native_contract::PRIVATE_ENV_PREFIX)?
            .private_placement(crate::native_contract::PRIVATE_PLACEMENT)
            .summary(summary)
            .isolated_process_group(panic_on_unsupported_syscalls)
            .tool_answers_brk(true)
            .diagnostic_file(&diagnostics)
            .map_err(|error| {
                Error::msg(format!(
                    "failed to configure DYNAMORIO diagnostic stderr: {error}"
                ))
            })?;
        drop(diagnostics);
        if panic_on_unsupported_syscalls {
            runner = runner.client_argument("-panic-on-unsupported-syscalls");
        }

        eprintln!(
            "hermit: [dynamorio backend] Detcore Tool active; running {program:?} under DynamoRIO ({})",
            drrun.display()
        );

        let mut guest = StdCommand::new(program);
        environment.remove(OsStr::new("HERMIT_LOG_FILE"));
        // The runtime checks this against its own build before it reads the
        // configuration frame (`crate::native_contract::CONFIG_FINGERPRINT_ENV`); the
        // runner's private prefix keeps it from the guest.
        environment.insert(
            crate::native_contract::CONFIG_FINGERPRINT_ENV.into(),
            detcore::config_wire_fingerprint().into(),
        );
        apply_exact_environment(&mut guest, &environment);
        guest.args(args);
        apply_dynamorio_workdir(&mut guest, workdir);

        Ok(Self {
            execution,
            runner,
            guest,
            drrun,
            config: config.clone(),
            _unsupported_report: unsupported_report,
        })
    }

    fn run(
        mut self,
        summary: bool,
        engagement_json: Option<&Path>,
        log: Option<LevelFilter>,
        log_file: Option<&Path>,
    ) -> Result<ExitStatus, Error> {
        if dynamorio_log_file_is_refused(log_file.is_some(), false) {
            return Err(Error::msg(DYNAMORIO_LOG_FILE_REFUSAL));
        }
        if let Some(level) = log {
            self.guest.env("HERMIT_LOG", level.to_string());
        }
        let stats = match self.execution.stats_capture() {
            Ok(capture) => Some(capture),
            Err(error) if summary || engagement_json.is_some() => return Err(error),
            Err(error) => {
                eprintln!(
                    ":: WARNING: the DYNAMORIO process records cannot be set up, so a backend failure in a guest child cannot be ruled out: {error}"
                );
                None
            }
        };
        if let Some(capture) = &stats {
            self.runner = self.execution.configure_stats(capture, self.runner)?;
        }
        let status = if std::io::stdin().is_terminal() {
            process_status(run_status(
                &self.execution,
                &self.runner,
                &self.guest,
                &self.drrun,
            )?)
        } else {
            let output = run_once_with_inherited_input(
                &self.execution,
                &self.runner,
                &self.guest,
                &self.drrun,
                None,
            )?;
            write_output(&output)?;
            output_status(&output)
        };
        let stats = stats.map(|sink| sink.capture());
        if let Some(capture) = &stats {
            finish_single_run_dynamorio_stats(capture, summary, engagement_json)?;
        }
        if summary {
            // Report the supplied configuration separately from timer delivery.
            // DYNAMORIO's runtime still clears its maximum internally; this does not
            // change that execution model or claim a preemption mechanism.
            eprintln!(
                "{}",
                detcore_model::summary::PreemptionSummary {
                    max_timeslice_ns: self.config.max_timeslice.map(|maximum| maximum.get()),
                    timer_enabled: Some(false),
                }
            );
        }
        Ok(fail_on_dynamorio_backend_failure(status, stats.as_ref()))
    }

    fn verification(self, stdin: Option<std::fs::File>) -> Result<DynamoRioVerification, Error> {
        let terminal = stdin
            .as_ref()
            .is_some_and(|input| unsafe { libc::isatty(input.as_raw_fd()) } == 1);
        Ok(DynamoRioVerification {
            invocation: self,
            stdin,
            terminal,
            replay: tempfile::tempfile()?,
        })
    }
}

/// One pair's lazy stdin replay state; the common app owns run ordering.
struct DynamoRioVerification {
    invocation: DynamoRioInvocation,
    stdin: Option<std::fs::File>,
    terminal: bool,
    replay: std::fs::File,
}

impl DynamoRioVerification {
    fn run(
        &mut self,
        mode: Mode,
        capture: VerificationCapture<'_>,
    ) -> Result<PhysicalRun<Pending>, Error> {
        let invocation = &self.invocation;
        // The live pair requires its one shared /etc/ld.so.cache snapshot.
        // Saved verification historically runs once without that pair-only
        // prerequisite. `mode` is the session's exact declared mode, checked
        // against Execute before this call; it is not a new caller option.
        // https://github.com/rrnewton/hermit/issues/4693
        if mode == Mode::VerificationPair {
            anyhow::ensure!(
                invocation.execution.scope.has_shared_ld_so_cache(),
                "DYNAMORIO verification run has no shared /etc/ld.so.cache snapshot"
            );
        }
        let run = capture.run;
        let stats = invocation.execution.stats_capture()?;
        let runner = invocation
            .execution
            .configure_stats(&stats, invocation.runner.clone())?;
        let mut evidence_file = tempfile::tempfile()?;
        let evidence = Some(DynamoRioRunEvidence {
            file: &evidence_file,
            log_level: dynamorio_evidence_log_level(capture.evidence_log_level()),
            label: match run {
                VerificationRun::Run1 => "run-1",
                VerificationRun::Run2 => "run-2",
            },
        });
        let deadlock_recording = capture.record_terminal_deadlock();
        let output = if self.terminal {
            run_once_with_inherited_input(
                &invocation.execution,
                &runner,
                &invocation.guest,
                &invocation.drrun,
                evidence,
            )
        } else {
            let input: Box<dyn Read + Send> = match run {
                VerificationRun::Run1 => match self.stdin.take() {
                    Some(input) => Box::new(TeeReader {
                        input,
                        replay: self.replay.try_clone()?,
                    }),
                    None => Box::new(std::io::empty()),
                },
                VerificationRun::Run2 => {
                    self.replay.seek(SeekFrom::Start(0))?;
                    Box::new(self.replay.try_clone()?)
                }
            };
            run_once(
                &invocation.execution,
                &runner,
                &invocation.guest,
                &invocation.drrun,
                evidence,
                input,
            )
        };
        drop(deadlock_recording);
        let output = output?;
        let evidence = decode_dynamorio_evidence(&mut evidence_file)
            .map_err(|error| error.context(hermit::BackendRunFailure))?;
        materialize_dynamorio_evidence_log(&evidence, capture.log, capture.path)
            .map_err(|error| error.context(hermit::BackendRunFailure))?;
        Ok(PhysicalRun {
            output: dynamorio_verification_output(output),
            skid_overshoots: 0,
            latches: dynamorio_run_latches(&evidence),
            pending: Pending(DynamoRioRunCompletion {
                stats,
                initialization_records: evidence.initialization_records(),
                terminal_stdin: self.terminal,
            }),
        })
    }
}

/// Stats are consumed only after latches and first-run exit policy, preserving
/// their diagnostic precedence even when the stats artifact is missing.
struct DynamoRioRunCompletion {
    stats: DynamoRioStatsSink,
    initialization_records: usize,
    terminal_stdin: bool,
}

impl DynamoRioRunCompletion {
    fn finish(
        self,
        output: &reverie::process::Output,
        run: VerificationRun,
    ) -> Result<DynamoRioRunFacts, Error> {
        // Read once, after the common latches and first-run exit policy. The
        // same validated native bytes are retained for the core-owned writer.
        let capture = self.stats.capture();
        let stats = require_dynamorio_stats(&capture)
            .map_err(|error| error.context(hermit::BackendRunFailure))?;
        if run == VerificationRun::Run1 && self.terminal_stdin && stats.stdin_reads() != 0 {
            std::io::stdout().write_all(&output.stdout)?;
            std::io::stderr().write_all(&output.stderr)?;
            return Err(Error::msg(format!(
                "DYNAMORIO verification cannot replay terminal stdin: guest attempted {} fd-0 read syscall(s)",
                stats.stdin_reads()
            )));
        }
        Ok(DynamoRioRunFacts {
            stats,
            raw: capture.records()?.to_vec(),
            initialization_records: self.initialization_records,
        })
    }
}

/// Validated physical facts and their exact native payload. Fields stay private:
/// the core owns saved-file publication, metadata and comparison decisions.
#[derive(Debug)]
pub struct DynamoRioRunFacts {
    stats: DynamoRioBackendStatsSnapshot,
    raw: Vec<u8>,
    initialization_records: usize,
}

impl DynamoRioRunFacts {
    fn restore(
        raw: Result<&[u8], &std::io::Error>,
        initialization_records: u64,
    ) -> Result<Self, Error> {
        let capture = match raw {
            Ok(bytes) => DynamoRioStatsCapture::Records(bytes.to_vec()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                DynamoRioStatsCapture::Missing
            }
            Err(error) => DynamoRioStatsCapture::Unreadable {
                message: error.to_string(),
                raw_os_error: error.raw_os_error(),
            },
        };
        // Preserve legacy offline order: require the actual raw statistics
        // through the same decoder, then check the metadata count conversion.
        let stats = require_dynamorio_stats(&capture)?;
        let initialization_records = usize::try_from(initialization_records)?;
        Ok(Self {
            stats,
            raw: capture.records()?.to_vec(),
            initialization_records,
        })
    }

    fn native_facts(&self) -> NativeFacts {
        NativeFacts {
            counted_branches: self.stats.counted_branches(),
            intercepted_syscalls: self.stats.intercepted_syscalls(),
            rewritten_syscalls: self.stats.rewritten_syscalls(),
            stdin_reads: self.stats.stdin_reads(),
            memory_hash_fold: self.stats.memory_hash_fold(),
            initialization_records: self.initialization_records,
            statistics_diagnostic: format!("{:?}", self.stats),
        }
    }
}

/// Where the runtime keeps per-invocation configuration, hidden from the guest.
const DYNAMORIO_CONFIG_DIR_ENV: &str = "DYNAMORIO_CONFIGDIR";

fn dynamorio_stats_capture(
    statistics: &std::cell::RefCell<Option<Bind>>,
) -> Result<DynamoRioStatsSink, Error> {
    // Also clear the preceding mapping when optional allocation fails.
    statistics.replace(None);
    DynamoRioStatsSink::with_prefix("hermit-dynamorio-verify-stats-").map_err(|error| {
        Error::msg(format!(
            "failed to create DYNAMORIO whole-process statistics sink: {error}"
        ))
    })
}

fn configure_dynamorio_stats_at(
    statistics: &std::cell::RefCell<Option<Bind>>,
    capture: &DynamoRioStatsSink,
    runner: DynamoRioRunner,
    target: Option<PathBuf>,
) -> Result<DynamoRioRunner, Error> {
    let Some(directory) = target else {
        return Ok(capture.configure(runner));
    };
    statistics.replace(Some(Bind::new(capture.directory(), &directory)));
    Ok(capture.configure_at(runner, &directory))
}

struct DynamoRioExecution {
    scope: ContainerScope,
    drrun: PathBuf,
    /// Exact installed core cohort retained through each physical teardown.
    native_artifacts: Option<Arc<crate::engine_artifact::NativeArtifacts>>,
    /// The current physical run's unique host sink at its stable guest address.
    statistics: std::cell::RefCell<Option<Bind>>,
    /// Existing invocation writer; Arc cloning does not create a descriptor.
    unsupported_report: Option<Arc<std::fs::File>>,
    /// DynamoRIO's configuration directory inside the container; see
    /// `contained_command`.
    config_dir: PathBuf,
    /// The guest program, working directory and `PATH`, from which the
    /// container records the guest's first image
    /// (`crate::physical::record_dynamorio_launched_image`).
    launched: Option<(PathBuf, Option<PathBuf>, Option<OsString>)>,
}

impl DynamoRioExecution {
    fn new(scope: ContainerScope, test_workdir: bool, drrun: PathBuf) -> Self {
        // A marked workdir (the validate harness's HERMIT_E2E_EMPTY_WORKDIR)
        // gets a fresh tmpfs, unless the caller already mounts a tmpfs there.
        // It is the guest's working directory, part of the guest's machine,
        // so it is a mount of the caller's kind, built exactly as the
        // harness's `--mount=type=tmpfs,target=/test` is for every other
        // backend (DYNAMORIO refuses --mount): the guest's mount table then shows
        // the same row on every backend
        // (https://github.com/rrnewton/hermit/issues/4213).
        let test_workdir =
            test_workdir && !scope.mounts_tmpfs_at(Path::new(hermit_test_workdir::WORKDIR));
        let scope = if test_workdir {
            let workdir = format!("type=tmpfs,target={}", hermit_test_workdir::WORKDIR)
                .parse::<hermit::app::backend_hook::MountArg>()
                .expect("a tmpfs mount at the test workdir parses");
            scope.with_guest_mount(workdir)
        } else {
            scope
        };
        Self {
            scope,
            drrun,
            native_artifacts: None,
            statistics: Default::default(),
            unsupported_report: None,
            config_dir: dynamorio_config_dir(),
            launched: None,
        }
    }

    fn reporting_unsupported_to(mut self, writer: Arc<std::fs::File>) -> Self {
        self.unsupported_report = Some(writer);
        self
    }

    fn with_native_artifacts(
        mut self,
        artifacts: Arc<crate::engine_artifact::NativeArtifacts>,
    ) -> Self {
        self.native_artifacts = Some(artifacts);
        self
    }

    /// Records `program`, run from `workdir` and found through `path`, as
    /// the guest's first image in the container
    /// (`crate::physical::record_dynamorio_launched_image`).
    fn launching(mut self, program: &Path, workdir: Option<&Path>, path: Option<&OsStr>) -> Self {
        self.launched = Some((
            program.to_owned(),
            workdir.map(Path::to_owned),
            path.map(OsStr::to_owned),
        ));
        self
    }

    fn stats_capture(&self) -> Result<DynamoRioStatsSink, Error> {
        dynamorio_stats_capture(&self.statistics)
    }

    fn configure_stats(
        &self,
        capture: &DynamoRioStatsSink,
        runner: DynamoRioRunner,
    ) -> Result<DynamoRioRunner, Error> {
        let target = self
            .scope
            .runtime_directory_target(".hermit-dynamorio-stats-")
            .map_err(|_| {
                Error::msg("DYNAMORIO statistics directory conflicts with declared binds")
            })?;
        configure_dynamorio_stats_at(&self.statistics, capture, runner, target)
    }

    /// `guest`, copied, with DynamoRIO's configuration directory moved to a
    /// directory of this invocation's own under the container's /tmp.
    ///
    /// drrun hands its client options to the launched process in a one-time
    /// file, `<dir>/.dynamorio/<program>.<pid>.1config64`, where `<dir>` is
    /// `DYNAMORIO_CONFIGDIR` or else `HOME`. In the container the PID is the
    /// namespace's own, so concurrent runs that share `<dir>` agree on the
    /// file name: one guest then reads and deletes the other's file, runs with
    /// that run's evidence channel, and neither run receives its evidence
    /// ("DYNAMORIO evidence received no image START"). The container's /tmp alone
    /// is not private enough: `--tmp=DIR` binds a shared directory there, as
    /// the robust_futex_owner_death tests do with `--tmp=/tmp`, and those
    /// collided when nextest ran them eight at a time. So `<dir>` is a
    /// directory named for this invocation, made inside the container before
    /// each physical run and removed after it.
    ///
    /// DynamoRIO removes every `DYNAMORIO_` variable from the guest's
    /// environment. The value is the same on every physical run of one
    /// invocation (both runs of a verification), and its length is the same
    /// on every invocation, so the guest's initial stack does not change
    /// between runs.
    fn contained_command(&self, guest: &StdCommand) -> StdCommand {
        let mut command = copy_dynamorio_command(guest);
        if self.scope.uses_namespace() {
            command.env(DYNAMORIO_CONFIG_DIR_ENV, &self.config_dir);
        }
        command
    }

    fn new_runtime() -> Result<tokio::runtime::Runtime, Error> {
        // RPC and scheduler waits yield asynchronously. The launcher moves
        // blocking child waits and I/O to spawn_blocking after guest creation.
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| {
                Error::msg(format!(
                    "failed to start the DYNAMORIO coordinator: {error}"
                ))
            })
    }

    /// Run `run` with a fresh coordinator runtime and `config` completed for
    /// the container. The completed configuration carries this run's host
    /// mount IDs; it reaches the DYNAMORIO runtime only through the coordinator's
    /// first RPC frame, never through the guest's environment.
    fn run<F, T>(&self, mut run: F) -> Result<T, Error>
    where
        F: FnMut(&tokio::runtime::Runtime, &Config) -> Result<T, Error> + 'static,
        T: serde::Serialize + serde::de::DeserializeOwned + 'static,
    {
        let artifacts = self
            .native_artifacts
            .as_ref()
            .ok_or_else(|| {
                Error::msg("DynamoRIO physical execution has no admitted core engine cohort")
            })?
            .clone();
        artifacts.recheck()?;
        let contained_artifacts = artifacts.clone();
        // Core owns container admission, identity, source mounts, timeout,
        // namespace PID baseline and normalized backend configuration. Only the
        // current physical statistics sink joins its backend-owned mount plan.
        let scope = self
            .scope
            .clone()
            .with_runtime_mounts(self.statistics.borrow().as_slice(), Vec::new());
        let launched = self.launched.clone();
        let drrun = self.drrun.clone();
        let config_dir = self.scope.uses_namespace().then(|| self.config_dir.clone());
        // Core retains the same declared DYNAMORIO capabilities and prepared config
        // for both runs, including the original pre-fork terminal observation.
        let result = scope
            .run((), "the DynamoRIO coordinator", move |_, mut config| {
                // Resolve and recheck the same held cohort in the actual namespace.
                // A hidden/replaced path refuses before the physical launcher.
                contained_artifacts.recheck()?;
                // Preflight already verified this resource and serialization before
                // guest execution. Re-declare only its launcher in the completed
                // namespace, then resolve the actual guest image there as before.
                crate::physical::declare_dynamorio_launcher(&mut config, &drrun)
                    .map_err(|error| Error::msg(format!("{error:#}")))?;
                if let Some((program, workdir, path)) = &launched {
                    crate::physical::record_dynamorio_launched_image(
                        &mut config,
                        program,
                        workdir.as_deref(),
                        path.as_deref(),
                    );
                }
                let _config_dir = config_dir
                    .as_deref()
                    .map(DynamoRioConfigDir::create)
                    .transpose()?;
                let runtime = Self::new_runtime()?;
                // GlobalState cleanup is consumed below. Runtime Drop joins workers
                // before the core-owned container publishes its physical result.
                let result = run(&runtime, &config);
                drop(runtime);
                contained_artifacts.complete(result)
            })
            .map(|(value, ())| value);
        artifacts.complete(result)
    }
}

/// A run's wait status and output, in the form the container init publishes.
#[derive(serde::Serialize, serde::Deserialize)]
struct DynamoRioPhysicalOutput {
    status: i32,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl From<Output> for DynamoRioPhysicalOutput {
    fn from(output: Output) -> Self {
        Self {
            status: output.status.into_raw(),
            stdout: output.stdout,
            stderr: output.stderr,
        }
    }
}

impl From<DynamoRioPhysicalOutput> for Output {
    fn from(output: DynamoRioPhysicalOutput) -> Self {
        Self {
            status: std::process::ExitStatus::from_raw(output.status),
            stdout: output.stdout,
            stderr: output.stderr,
        }
    }
}

/// The container init's copy-on-write view of the caller's input. The init
/// exits without unwinding, which closes its descriptors; dropping the reader
/// would free heap the parent still owns.
struct ChildInput<R>(std::mem::ManuallyDrop<R>);

impl<R: Read> Read for ChildInput<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buffer)
    }
}

/// [`DynamoRioRunEvidence`] with its own descriptor, for the container init.
struct OwnedDynamoRioRunEvidence {
    file: std::fs::File,
    log_level: DynamoRioEvidenceLogLevel,
    label: &'static str,
}

impl OwnedDynamoRioRunEvidence {
    fn from_borrowed(evidence: Option<DynamoRioRunEvidence<'_>>) -> Result<Option<Self>, Error> {
        evidence
            .map(|evidence| {
                Ok(Self {
                    file: evidence.file.try_clone()?,
                    log_level: evidence.log_level,
                    label: evidence.label,
                })
            })
            .transpose()
    }

    fn borrowed(&self) -> DynamoRioRunEvidence<'_> {
        DynamoRioRunEvidence {
            file: &self.file,
            log_level: self.log_level,
            label: self.label,
        }
    }
}

/// A copy of `guest`, which `std::process::Command` cannot clone.
/// A DynamoRIO configuration directory under the container's /tmp, named
/// for one Hermit invocation: `/tmp/.hermit-dynamorio-` and 16 hex digits
/// from a randomly keyed hash, so concurrent invocations sharing /tmp never
/// share it and every name has the same length.
fn dynamorio_config_dir() -> PathBuf {
    use std::hash::BuildHasher;
    let key = std::collections::hash_map::RandomState::new().hash_one((
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos()),
    ));
    PathBuf::from(format!("/tmp/.hermit-dynamorio-{key:016x}"))
}

/// The configuration directory of one physical DYNAMORIO run, made inside the
/// container before drrun starts and removed, with the `.dynamorio` files
/// drrun leaves in it, when the run ends.
struct DynamoRioConfigDir(PathBuf);

impl DynamoRioConfigDir {
    fn create(path: &Path) -> Result<Self, Error> {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(path)
            .map_err(|error| {
                Error::msg(format!(
                    "failed to create the DynamoRIO configuration directory {}: {error}",
                    path.display()
                ))
            })?;
        Ok(Self(path.to_path_buf()))
    }
}

impl Drop for DynamoRioConfigDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn copy_dynamorio_command(guest: &StdCommand) -> StdCommand {
    let mut command = StdCommand::new(guest.get_program());
    command.args(guest.get_args());
    for (name, value) in guest.get_envs() {
        match value {
            Some(value) => {
                command.env(name, value);
            }
            None => {
                command.env_remove(name);
            }
        }
    }
    if let Some(directory) = guest.get_current_dir() {
        command.current_dir(directory);
    }
    command
}

/// Create the abstract Unix evidence listener only after entering this physical
/// run's network namespace. A listener created by the caller would be invisible
/// to the guest. The completed artifact is an ordinary file read by the caller.
struct DynamoRioRunEvidence<'a> {
    file: &'a std::fs::File,
    log_level: DynamoRioEvidenceLogLevel,
    label: &'static str,
}

fn configure_dynamorio_run_evidence(
    runner: &DynamoRioRunner,
    evidence: Option<DynamoRioRunEvidence<'_>>,
) -> Result<DynamoRioRunner, Error> {
    let runner = runner.clone();
    match evidence {
        Some(evidence) => runner
            .evidence_file_with_validator(
                evidence.file,
                crate::native_evidence::validate_evidence_record,
            )
            .map(|runner| runner.evidence_log_level(evidence.log_level))
            .map_err(|error| {
                Error::msg(format!(
                    "failed to configure protected DYNAMORIO {} evidence: {error}",
                    evidence.label
                ))
            }),
        None => Ok(runner),
    }
}

fn run_once<R: Read + Send + 'static>(
    execution: &DynamoRioExecution,
    runner: &DynamoRioRunner,
    guest: &StdCommand,
    drrun: &Path,
    evidence: Option<DynamoRioRunEvidence<'_>>,
    input: R,
) -> Result<Output, Error> {
    let runner = runner.clone();
    let guest = execution.contained_command(guest);
    let drrun = drrun.to_path_buf();
    let report = execution.unsupported_report.clone();
    let evidence = OwnedDynamoRioRunEvidence::from_borrowed(evidence)?;
    let mut input = Some(input);
    execution
        .run(move |runtime, config| {
            let runner = configure_dynamorio_run_evidence(
                &runner,
                evidence.as_ref().map(OwnedDynamoRioRunEvidence::borrowed),
            )?;
            let guest = prepare_dynamorio_physical_command(&guest)?;
            let input =
                ChildInput(std::mem::ManuallyDrop::new(input.take().ok_or_else(
                    || Error::msg("DYNAMORIO physical input already consumed"),
                )?));
            let (output, global) = runtime
                .block_on(
                    runner.output_with_detached_reader_and_global::<detcore::GlobalState, _>(
                        &guest,
                        input,
                        config.clone(),
                    ),
                )
                .map_err(|error| dynamorio_run_error(&drrun, error))?;
            runtime.block_on(clean_up_dynamorio_global(
                &output.status,
                global,
                report.as_deref(),
            ))?;
            Ok(DynamoRioPhysicalOutput::from(output))
        })
        .map(Output::from)
}

fn run_once_with_inherited_input(
    execution: &DynamoRioExecution,
    runner: &DynamoRioRunner,
    guest: &StdCommand,
    drrun: &Path,
    evidence: Option<DynamoRioRunEvidence<'_>>,
) -> Result<Output, Error> {
    let runner = runner.clone();
    let guest = execution.contained_command(guest);
    let drrun = drrun.to_path_buf();
    let report = execution.unsupported_report.clone();
    let evidence = OwnedDynamoRioRunEvidence::from_borrowed(evidence)?;
    execution
        .run(move |runtime, config| {
            let runner = configure_dynamorio_run_evidence(
                &runner,
                evidence.as_ref().map(OwnedDynamoRioRunEvidence::borrowed),
            )?;
            let guest = prepare_dynamorio_physical_command(&guest)?;
            let (output, global) = runtime
                .block_on(
                    runner.output_with_inherited_stdin_and_global::<detcore::GlobalState>(
                        &guest,
                        config.clone(),
                    ),
                )
                .map_err(|error| dynamorio_run_error(&drrun, error))?;
            runtime.block_on(clean_up_dynamorio_global(
                &output.status,
                global,
                report.as_deref(),
            ))?;
            Ok(DynamoRioPhysicalOutput::from(output))
        })
        .map(Output::from)
}

fn run_status(
    execution: &DynamoRioExecution,
    runner: &DynamoRioRunner,
    guest: &StdCommand,
    drrun: &Path,
) -> Result<std::process::ExitStatus, Error> {
    let runner = runner.clone();
    let guest = execution.contained_command(guest);
    let drrun = drrun.to_path_buf();
    let report = execution.unsupported_report.clone();
    execution
        .run(move |runtime, config| {
            let guest = prepare_dynamorio_physical_command(&guest)?;
            let (status, global) = runtime
                .block_on(runner.status_with_global::<detcore::GlobalState>(&guest, config.clone()))
                .map_err(|error| dynamorio_run_error(&drrun, error))?;
            runtime.block_on(clean_up_dynamorio_global(
                &status,
                global,
                report.as_deref(),
            ))?;
            Ok(status.into_raw())
        })
        .map(std::process::ExitStatus::from_raw)
}

async fn clean_up_dynamorio_global(
    status: &std::process::ExitStatus,
    mut global: detcore::GlobalState,
    report: Option<&std::fs::File>,
) -> Result<(), Error> {
    // The runner has drained RPC owners before returning exclusive GlobalState.
    // Forward its aggregate through the existing invocation pipe used by
    // copied-child records, without opening or duplicating a descriptor.
    let exported = if let Some(mut report) = report {
        global
            .take_unsupported_syscalls()
            .into_iter()
            .try_for_each(|name| writeln!(report, "{name}"))
    } else {
        Ok(())
    };
    if !status.success() {
        global.force_shutdown_with_error();
        // The physical supervisor and RPC owner drain have finished. A client
        // that failed before registration cannot start the owned scheduler.
        global.cancel_internal_scheduler().await;
    }
    let finalized = global.clean_up(false, &None).await;
    // Export failure must never bypass scheduler cleanup or lose its class.
    detcore::combine_finalization(exported.map_err(Error::from), finalized)
}

/// Name the stage that actually failed.
///
/// This used to be `launch_error`, and it announced EVERY `io::Error` from a
/// DYNAMORIO run as "failed to launch drrun ({path})". But the calls it wraps --
/// `output_with_detached_reader` and `output_with_inherited_stdin` -- run the
/// whole lifecycle: spawn, wait, collect output, and finalize the protected
/// evidence session. A failure in any later stage was reported as a failure of
/// the first one.
///
/// Measured cost of that: a campaign agent hit
/// `"failed to launch drrun (target/install_pkg/rsrcs/dynamorio/bin64/drrun):
/// DYNAMORIO guest exited with status S..."`, correctly observed that drrun was a
/// real 737 KB ELF matching the build cache, and could not proceed. The inner
/// text is the giveaway -- it comes from `reverie-dynamorio`'s
/// `(Ok(status), Err(error))` arm, so the guest HAD RUN AND EXITED and the
/// failure was in evidence finalization. A binary that failed to launch cannot
/// produce an exit status. The wrapper had renamed a post-run failure into a
/// missing-binary hunt.
///
/// The discriminator is `ErrorKind`. A real spawn failure surfaces the OS error
/// (`NotFound` when drrun is absent, `PermissionDenied` when it is not
/// executable, `ExecutableFileBusy`). Where the kind does not prove a spawn
/// failure this deliberately does NOT claim one.
///
/// The pinned Reverie revision preserves this distinction: once it has an exit
/// status, an evidence-finalization failure is returned as `Other`, even when
/// the underlying error was `NotFound` or `PermissionDenied`. Spawn failures
/// bypass that combination and retain their original kind. Do not replace this
/// typed boundary with matching on display text.
fn dynamorio_run_error(drrun: &Path, error: std::io::Error) -> Error {
    use std::io::ErrorKind;
    // A run that failed after its tree was reaped (protected evidence reporting
    // a backend failure, for one) still carries what it captured. Its stderr
    // usually holds the line that says why, so show it before the error.
    if let Some(output) = reverie_dynamorio::failed_run_output(&error)
        && !output.stderr.is_empty()
    {
        eprintln!(":: stderr captured from the failed DYNAMORIO run:");
        let _ = std::io::stderr().write_all(&output.stderr);
    }
    match error.kind() {
        ErrorKind::NotFound | ErrorKind::PermissionDenied | ErrorKind::ExecutableFileBusy => {
            Error::msg(format!(
                "failed to launch drrun ({}): {error}",
                drrun.display()
            ))
        }
        // Typed under the message: `run --verify` records it as a backend
        // failure of the run, not as a run that never started.
        _ => Error::new(hermit::BackendRunFailure).context(format!(
            "drrun started ({}) and the DYNAMORIO run then failed: {error} \
             -- this is NOT a launch failure; the drrun binary is not implicated. \
             Read the text after the last ':' for the stage that actually failed.",
            drrun.display()
        )),
    }
}

// ⚠️ `test` AS WELL AS `dynamorio`, AND THE REASON IS THAT THE BRACKETS BELOW MUST RUN.
// `dynamorio` is not in `default` (hermit-cli/Cargo.toml: `default = []`), so under
// `#[cfg(feature = "dynamorio")]` alone this function and any test of it compile only in
// a build validation does not perform. Measured on main before this change:
//
//     $ cargo test -p hermit-run --bin hermit dynamorio_status
//     running 0 tests
//
// Zero -- so the only check that a signalled death is not reported as a normal exit
// contributed nothing to any receipt. Adding `test` keeps the symbol out of a
// feature-off PRODUCTION build, where `clippy -D warnings` would call the import
// dead, while letting the brackets execute wherever tests do.
fn process_status(status: std::process::ExitStatus) -> ExitStatus {
    ExitStatus::from_raw(status.into_raw())
}

fn write_output(output: &Output) -> Result<(), Error> {
    std::io::stdout().write_all(&output.stdout)?;
    std::io::stderr().write_all(&output.stderr)?;
    Ok(())
}

// `test` as well as `dynamorio`, matching process_status above. This is the site where
// the signal-losing conversion was MISSED -- `code().unwrap_or(1)` reported every
// signalled death as Exited(1) -- so it is the one that most needs a bracket that
// actually runs, and the bracket for it ran ZERO times on the default build.
fn output_status(output: &Output) -> ExitStatus {
    // ⚠️ SAME SIGNAL-LOSING CONVERSION `process_status` ALREADY FIXED, MISSED HERE.
    // `std::process::ExitStatus::code()` is `None` for a process killed by a
    // signal, so `code().unwrap_or(1)` reported every signalled death as a
    // normal `Exited(1)`: a guest killed by SIGSEGV came back as "exited
    // normally with status 1", and WIFSIGNALED/WTERMSIG/WCOREDUMP were all lost.
    // `ExitStatus::from_raw` decodes exited-versus-signalled and the core-dump
    // flag the same way the ptrace backend does.
    ExitStatus::from_raw(output.status.into_raw())
}

#[cfg(test)]
#[path = "native_saved_tests.rs"]
mod native_saved_tests;

#[cfg(test)]
#[path = "adapter_physical_tests.rs"]
mod adapter_physical_tests;

#[cfg(test)]
#[path = "adapter_stats_tests.rs"]
mod adapter_stats_tests;

#[cfg(test)]
#[path = "adapter_evidence_tests.rs"]
mod adapter_evidence_tests;

#[cfg(test)]
#[path = "adapter_status_tests.rs"]
mod adapter_status_tests;
