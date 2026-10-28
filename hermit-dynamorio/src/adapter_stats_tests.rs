// Existing backend-native stats policy controls, transferred from the core.
// No guest or runtime is launched by these cases.
use super::*;

fn dynamorio_stats(
    branches: u64,
    syscalls: u64,
    rewritten: u64,
    stdin_reads: u64,
    memory_hash: u64,
) -> DynamoRioBackendStatsSnapshot {
    use reverie_dynamorio::backend_stats::DynamoRioBackendStatsAggregator;
    use reverie_dynamorio::backend_stats::DynamoRioProcessRecord;

    let record = DynamoRioProcessRecord {
        branches,
        syscalls,
        rewritten,
        stdin_reads,
        memory_hash,
        ..Default::default()
    };
    let mut aggregator = DynamoRioBackendStatsAggregator::new();
    aggregator.record(&record);
    aggregator.snapshot()
}
#[test]
fn dynamorio_engagement_record_follows_the_typed_branch_count() {
    fn record(branches: u64) -> serde_json::Value {
        let capture = DynamoRioStatsSink::new().unwrap();
        let row = reverie_dynamorio::backend_stats::DynamoRioProcessRecord {
            branches,
            syscalls: 1,
            rewritten: 1,
            ..Default::default()
        };
        fs::write(
            capture.directory().join("records.bin"),
            reverie_dynamorio::backend_stats::encode_process_record(&row),
        )
        .unwrap();
        let output = tempfile::NamedTempFile::new().unwrap();
        finish_single_run_dynamorio_stats(&capture.capture(), false, Some(output.path())).unwrap();
        serde_json::from_slice(&fs::read(output.path()).unwrap()).unwrap()
    }

    let first = record(18);
    let second = record(19);
    assert_eq!(first["engagement"]["backend"], "dbt");
    assert_eq!(first["engagement"]["counted_branches"], 18);
    assert_eq!(second["engagement"]["counted_branches"], 19);
    assert_ne!(
        first, second,
        "mutating the producer count must move the record"
    );
}
#[test]
fn terminal_failure_records_do_not_establish_execution_completeness() {
    use reverie_dynamorio::backend_stats::DynamoRioProcessRecord;
    use reverie_dynamorio::backend_stats::encode_process_record;

    let capture = DynamoRioStatsCapture::Records(
        encode_process_record(&DynamoRioProcessRecord {
            backend_failure: true,
            process_identity: 7,
            parent_identity: 3,
            ..Default::default()
        })
        .to_vec(),
    );
    let killed = ExitStatus::Signaled(reverie::Signal::SIGKILL, false);
    assert_eq!(
        fail_on_dynamorio_backend_failure(killed, Some(&capture)),
        ExitStatus::Exited(101)
    );
    assert!(require_dynamorio_stats(&capture).is_err());
    assert!(
        finish_single_run_dynamorio_stats(&capture, false, None)
            .unwrap()
            .is_none()
    );
    let output = tempfile::NamedTempFile::new().unwrap();
    assert!(finish_single_run_dynamorio_stats(&capture, false, Some(output.path())).is_err());
    assert_eq!(output.as_file().metadata().unwrap().len(), 0);
}
#[test]
fn terminal_failure_status_requires_a_complete_flagged_record() {
    use reverie_dynamorio::backend_stats::DynamoRioProcessRecord;
    use reverie_dynamorio::backend_stats::encode_process_record;

    let flagged = encode_process_record(&DynamoRioProcessRecord {
        backend_failure: true,
        ..Default::default()
    });
    let mut bad_magic = flagged.to_vec();
    bad_magic[0] ^= 1;
    let malformed_tail = [flagged.as_slice(), &flagged[..13]].concat();
    for capture in [
        DynamoRioStatsCapture::Missing,
        DynamoRioStatsCapture::Unreadable {
            message: "unreadable".into(),
            raw_os_error: None,
        },
        DynamoRioStatsCapture::Records(Vec::new()),
        DynamoRioStatsCapture::Records(bad_magic),
        DynamoRioStatsCapture::Records(malformed_tail),
        DynamoRioStatsCapture::Records(
            encode_process_record(&DynamoRioProcessRecord::default()).to_vec(),
        ),
    ] {
        for status in [
            ExitStatus::Signaled(reverie::Signal::SIGKILL, false),
            ExitStatus::Exited(0),
            ExitStatus::Exited(101),
        ] {
            assert_eq!(
                fail_on_dynamorio_backend_failure(status, Some(&capture)),
                status
            );
        }
    }
}
#[test]
fn typed_stats_capture_refuses_missing_empty_or_truncated_evidence() {
    let missing = DynamoRioStatsSink::new().unwrap();
    let error = require_dynamorio_stats(&missing.capture()).unwrap_err();
    assert!(
        error.to_string().contains("statistics were missing"),
        "{error}"
    );

    let empty = DynamoRioStatsSink::new().unwrap();
    fs::write(empty.directory().join("records.bin"), b"").unwrap();
    let error = require_dynamorio_stats(&empty.capture()).unwrap_err();
    assert!(
        error.to_string().contains("statistics were empty"),
        "{error}"
    );

    let truncated = DynamoRioStatsSink::new().unwrap();
    fs::write(truncated.directory().join("records.bin"), b"truncated").unwrap();
    let error = require_dynamorio_stats(&truncated.capture()).unwrap_err();
    assert!(
        error.to_string().contains("statistics were unreadable"),
        "{error}"
    );
    assert!(error.to_string().contains("truncated"), "{error}");

    let inconsistent = DynamoRioStatsSink::new().unwrap();
    let record = reverie_dynamorio::backend_stats::DynamoRioProcessRecord {
        branches: 18,
        syscalls: 1,
        rewritten: 2,
        ..Default::default()
    };
    fs::write(
        inconsistent.directory().join("records.bin"),
        reverie_dynamorio::backend_stats::encode_process_record(&record),
    )
    .unwrap();
    let error = require_dynamorio_stats(&inconsistent.capture()).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("typed native callback counters are inconsistent"),
        "{error}"
    );
}
#[test]
fn dynamorio_stats_block_labels_counters_honestly() {
    let rendered =
        format_dynamorio_stats(&dynamorio_stats(563_145, 169, 168, 0, 0x4b5e0e70f3050157));
    // The branch counter must be labeled as a branch clock, never as
    // "basic blocks translated" — the client counts retired branches.
    assert!(rendered.contains("counted branches (deterministic branch clock): 563145"));
    assert!(rendered.contains("syscalls intercepted:                          169"));
    assert!(rendered.contains("syscall instructions rewritten:                168"));
    assert!(rendered.contains("stdin (fd 0) reads:                            0"));
    assert!(rendered.contains("observed guest-memory hash:                    4b5e0e70f3050157"));
    assert!(!rendered.to_lowercase().contains("basic block"));
}
#[test]
fn dynamorio_stats_block_renders_the_typed_wire_snapshot() {
    let rendered = format_dynamorio_stats(&dynamorio_stats(42, 7, 6, 0, 0xcbf29ce484222325));
    assert!(rendered.contains("counted branches (deterministic branch clock): 42"));
    assert!(rendered.contains("syscalls intercepted:                          7"));
    assert!(rendered.contains("observed guest-memory hash:                    cbf29ce484222325"));
}

#[test]
fn typed_stats_capture_aggregates_the_whole_process_tree() {
    use reverie_dynamorio::backend_stats::DynamoRioProcessRecord;
    use reverie_dynamorio::backend_stats::encode_process_record;

    use hermit::app::backend_hook::SavedNativeEvidence;

    let capture = DynamoRioStatsSink::new().unwrap();
    let root = DynamoRioProcessRecord {
        branches: 400,
        syscalls: 7,
        memory_hash: 0xff00_0a0d_001a_0000,
        ..Default::default()
    };
    let child = DynamoRioProcessRecord {
        branches: 23,
        syscalls: 2,
        ..Default::default()
    };
    let bytes = [encode_process_record(&root), encode_process_record(&child)].concat();
    fs::write(capture.directory().join("records.bin"), &bytes).unwrap();

    let captured = capture.capture();
    let snapshot = require_dynamorio_stats(&captured).unwrap();

    assert_eq!(snapshot.process_images(), 2);
    assert_eq!(snapshot.counted_branches(), 423);
    assert_eq!(snapshot.intercepted_syscalls(), 9);
    // The actual saved-run writer must preserve unequal binary records,
    // not regenerate an aggregate or reorder images. Offline strict policy
    // reads that same artifact through the shared capture/codec owner.
    let directory = tempfile::tempdir().unwrap();
    let evidence = SavedNativeEvidence::new(captured.records().unwrap().to_vec(), 0);
    let artifact = evidence
        .create_uncommitted_artifact(&directory.path().join("saved"))
        .unwrap();
    let saved = artifact.path().to_path_buf();
    assert_eq!(fs::read(&saved).unwrap(), bytes);
    assert_eq!(
        require_dynamorio_stats(&DynamoRioStatsCapture::read(&saved)).unwrap(),
        snapshot
    );
}

#[test]
fn typed_stats_failure_leaves_the_pre_stamped_no_result() {
    let verdict_file = tempfile::NamedTempFile::new().unwrap();
    hermit::app::backend_hook::write_pending_verification_json(verdict_file.path()).unwrap();

    let empty = DynamoRioStatsSink::new().unwrap();
    fs::write(empty.directory().join("records.bin"), b"").unwrap();
    assert!(require_dynamorio_stats(&empty.capture()).is_err());

    let json: serde_json::Value =
        serde_json::from_slice(&fs::read(verdict_file.path()).unwrap()).unwrap();
    assert_eq!(json["verdict"], "no_result");
    assert_eq!(json["verified"], false);
    assert_eq!(json["bitwise_parity"], false);
    assert!(json.get("dbt_counted_branches").is_none());
}

// The native halves of the original mixed mapping/completion controls. Common
// comparison, retention, saved classification and JSON publication stay in core.
#[test]
fn dynamorio_facts_preserve_each_dimension_through_the_real_pair_evidence() {
    fn facts(
        branches: u64,
        syscalls: u64,
        rewritten: u64,
        stdin_reads: u64,
        memory_hash: u64,
        initialization_records: usize,
    ) -> DynamoRioRunFacts {
        let record = reverie_dynamorio::backend_stats::DynamoRioProcessRecord {
            branches,
            syscalls,
            rewritten,
            stdin_reads,
            memory_hash,
            ..Default::default()
        };
        DynamoRioRunFacts {
            stats: dynamorio_stats(branches, syscalls, rewritten, stdin_reads, memory_hash),
            raw: reverie_dynamorio::backend_stats::encode_process_record(&record).to_vec(),
            initialization_records,
        }
    }

    let distinct = facts(101, 23, 7, 3, 53, 2);
    let native = distinct.native_facts();
    assert_eq!(
        (
            native.counted_branches,
            native.intercepted_syscalls,
            native.rewritten_syscalls,
            native.stdin_reads,
            native.memory_hash_fold,
            native.initialization_records,
        ),
        (101, 23, 7, 3, 53, 2),
        "the production facts mapping must keep every observation in its own dimension"
    );

    let other = facts(211, 47, 11, 5, 97, 4);
    let PairEvidence::NativeCounters { first, second } =
        DynamoRioAdapter.pair_evidence(&distinct, &other).unwrap()
    else {
        panic!("native evidence must retain the native counter contract");
    };
    assert_eq!(
        (
            first.counted_branches,
            first.intercepted_syscalls,
            first.rewritten_syscalls,
            first.stdin_reads,
            first.memory_hash_fold,
            first.initialization_records,
        ),
        (101, 23, 7, 3, 53, 2)
    );
    assert_eq!(
        (
            second.counted_branches,
            second.intercepted_syscalls,
            second.rewritten_syscalls,
            second.stdin_reads,
            second.memory_hash_fold,
            second.initialization_records,
        ),
        (211, 47, 11, 5, 97, 4)
    );
}

#[test]
fn missing_dynamorio_completion_stats_are_typed_backend_failures_for_each_run() {
    let output = reverie::process::Output {
        status: ExitStatus::Exited(0),
        stdout: Vec::new(),
        stderr: Vec::new(),
    };
    for run in [VerificationRun::Run1, VerificationRun::Run2] {
        let completion = DynamoRioRunCompletion {
            stats: DynamoRioStatsSink::new().unwrap(),
            initialization_records: 1,
            terminal_stdin: false,
        };
        let error = DynamoRioAdapter
            .finish(
                Pending(completion),
                Observation {
                    output: &output,
                    run,
                },
            )
            .err()
            .expect("missing stats must refuse");
        assert!(error.downcast_ref::<hermit::BackendRunFailure>().is_some());
        assert!(format!("{error:#}").contains("typed whole-process statistics were missing"));
    }

    // Record210's actual native half: the same unequal-completeness sequence
    // with guest status23. Use the existing real sink, unchanged native wire
    // encoder, BackendAdapter::finish and actual private NativeFacts mapping.
    let first_capture = DynamoRioStatsSink::new().unwrap();
    let record = reverie_dynamorio::backend_stats::DynamoRioProcessRecord {
        branches: 1,
        syscalls: 1,
        rewritten: 1,
        ..Default::default()
    };
    let bytes = reverie_dynamorio::backend_stats::encode_process_record(&record).to_vec();
    fs::write(first_capture.directory().join("records.bin"), &bytes).unwrap();
    let completed_output = reverie::process::Output {
        status: ExitStatus::Exited(23),
        stdout: Vec::new(),
        stderr: Vec::new(),
    };
    let facts = DynamoRioAdapter
        .finish(
            Pending(DynamoRioRunCompletion {
                stats: first_capture,
                initialization_records: 1,
                terminal_stdin: false,
            }),
            Observation {
                output: &completed_output,
                run: VerificationRun::Run1,
            },
        )
        .unwrap();
    assert_eq!(facts.raw, bytes);
    let native = facts.native_facts();
    assert_eq!(
        (
            native.counted_branches,
            native.intercepted_syscalls,
            native.rewritten_syscalls,
            native.stdin_reads,
            native.memory_hash_fold,
            native.initialization_records,
        ),
        (1, 1, 1, 0, 0, 1),
        "record210's common driver fixture must match REAL successful finish facts"
    );
    let error = DynamoRioAdapter
        .finish(
            Pending(DynamoRioRunCompletion {
                stats: DynamoRioStatsSink::new().unwrap(),
                initialization_records: 1,
                terminal_stdin: false,
            }),
            Observation {
                output: &completed_output,
                run: VerificationRun::Run2,
            },
        )
        .err()
        .expect("missing second-run stats must refuse");
    assert!(error.downcast_ref::<hermit::BackendRunFailure>().is_some());
    assert!(format!("{error:#}").contains("typed whole-process statistics were missing"));
}
