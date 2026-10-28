use super::*;
use std::os::unix::process::ExitStatusExt as _;

#[test]
fn a_signalled_guest_is_not_reported_as_a_normal_exit() {
    use std::os::unix::process::ExitStatusExt as _;

    // raw wait status for "killed by SIGSEGV" (11), no core dump.
    let raw = 11i32;
    let native = std::process::ExitStatus::from_raw(raw);
    assert!(native.code().is_none(), "SIGSEGV death has no exit code");

    let converted = process_status(native);
    assert!(
        !matches!(converted, ExitStatus::Exited(_)),
        "a signalled guest must not read as a normal exit, got {converted:?}"
    );

    // ⚠️ CALL `output_status` ITSELF, not an expression that looks like it.
    // My first version compared `process_status(..)` against an inline
    // `ExitStatus::from_raw(..)` and never invoked `output_status` at all --
    // so reverting `output_status` to the buggy form left this test GREEN.
    // Caught by mutating BOTH sites instead of one.
    let output = Output {
        status: native,
        stdout: Vec::new(),
        stderr: Vec::new(),
    };
    let from_output = output_status(&output);
    assert!(
        !matches!(from_output, ExitStatus::Exited(_)),
        "output_status must not report a signalled guest as a normal exit, got {from_output:?}"
    );
    assert_eq!(
        converted, from_output,
        "output_status and process_status must agree; they diverged once and \
             only one of them was fixed"
    );
}

#[test]
fn dynamorio_status_preserves_normal_exit_codes() {
    for code in [0, 1, 42, 255] {
        let raw = std::process::ExitStatus::from_raw(code << 8);
        assert_eq!(
            process_status(raw),
            ExitStatus::Exited(code),
            "a normal exit with code {code} must round-trip"
        );
    }
}

#[test]
fn dynamorio_status_preserves_death_by_signal() {
    for (signum, signal) in [
        (libc::SIGABRT, reverie::Signal::SIGABRT),
        (libc::SIGSEGV, reverie::Signal::SIGSEGV),
        (libc::SIGFPE, reverie::Signal::SIGFPE),
        (libc::SIGILL, reverie::Signal::SIGILL),
        (libc::SIGTERM, reverie::Signal::SIGTERM),
        (libc::SIGKILL, reverie::Signal::SIGKILL),
        (libc::SIGTRAP, reverie::Signal::SIGTRAP),
    ] {
        // without a core dump
        let raw = std::process::ExitStatus::from_raw(signum);
        assert_eq!(
            process_status(raw),
            ExitStatus::Signaled(signal, false),
            "death by signal {signum} must be reported as Signaled, not Exited"
        );
        // with the core-dump flag set (bit 0x80 of the wait status)
        let raw = std::process::ExitStatus::from_raw(signum | 0x80);
        assert_eq!(
            process_status(raw),
            ExitStatus::Signaled(signal, true),
            "the core-dump flag for signal {signum} must survive the conversion"
        );
    }
}

#[test]
fn dynamorio_log_file_is_refused_on_the_single_run_adapter_and_permitted_under_verify() {
    // the refusal: a log file was asked for, and this is not a verification run
    assert!(
        dynamorio_log_file_is_refused(true, false),
        "a requested --log-file must be refused on the ordinary single-run adapter"
    );
    // --verify owns its own sink, so the request is honoured there
    assert!(
        !dynamorio_log_file_is_refused(true, true),
        "--verify must keep accepting --log-file; the verification adapter owns the sink"
    );
    // no request, nothing to refuse, either way
    assert!(!dynamorio_log_file_is_refused(false, false));
    assert!(!dynamorio_log_file_is_refused(false, true));
}

#[test]
fn dynamorio_log_file_refusal_names_the_adapter_it_is_about() {
    assert_eq!(
        DYNAMORIO_LOG_FILE_REFUSAL,
        "DYNAMORIO --log-file is unavailable on the ordinary single-run adapter"
    );
}
