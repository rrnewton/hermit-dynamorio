// Existing protected native evidence controls, transferred from the core.
// These cases configure and decode real evidence; no guest is launched.
use super::*;

/// A behavioural pin, not a text match: require every record of the
/// published stream, initialization records included, to reach the
/// comparison log unchanged and in order. The guest records are passed
/// separately because only they are counted by the refusal guards.
#[test]
fn materialization_keeps_every_decoded_comparable_record() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("evidence.log");
    let file = std::fs::File::create(&path).expect("create log");
    let records: Vec<Vec<u8>> = vec![
        b"1970-01-01T00:00:00.000000Z  INFO detcore: DETLOG first\n".to_vec(),
        b"1970-01-01T00:00:00.000000Z  INFO detcore: DETLOG second\n".to_vec(),
    ];
    let published = [
        DYNAMORIO_INITIALIZATION_RECORD,
        records[0].as_slice(),
        records[1].as_slice(),
    ];

    super::materialize_dynamorio_comparison_log(&records, &published, file, &path)
        .expect("materialization must accept comparable records");

    assert_eq!(std::fs::read(&path).expect("read back"), published.concat());
}

#[test]
fn dynamorio_canonical_evidence_materializes_records_unchanged() {
    let log = tempfile::NamedTempFile::new().unwrap();
    let (file, path) = log.into_parts();
    let records = vec![
        b"1970-01-01T00:00:00.000000Z INFO detcore: DETLOG first\n".to_vec(),
        b"1970-01-01T00:00:00.000000Z INFO detcore::scheduler: second\n".to_vec(),
    ];
    let published = [
        DYNAMORIO_INITIALIZATION_RECORD,
        records[0].as_slice(),
        records[1].as_slice(),
    ];

    let compared = materialize_dynamorio_comparison_log(&records, &published, file, &path).unwrap();

    // The initialization record is an INFO record of the published stream,
    // so it is counted with the guest records.
    assert_eq!(compared, 3);
    assert_eq!(fs::read(&path).unwrap(), published.concat());

    // Materialization publishes an initialization record where the decoded
    // stream holds it, neither dropped nor moved. This stream is built by
    // hand: the Reverie collector would place the second initialization
    // record immediately before "child record 1", but the artifact carries
    // no image identity, so the decoder cannot enforce that placement and
    // accepts this layout too. Two streams with the same guest records and
    // the same initialization count, differing only in where an
    // initialization record sits, publish different logs; the typed count
    // alone could not tell them apart.
    let parent_first: &[u8] = b"1970-01-01T00:00:00.000000Z INFO detcore: parent record 1\n";
    let parent_second: &[u8] = b"1970-01-01T00:00:00.000000Z INFO detcore: parent record 2\n";
    let child: &[u8] = b"1970-01-01T00:00:00.000000Z INFO detcore: child record 1\n";
    let stream = [
        DYNAMORIO_INITIALIZATION_RECORD,
        parent_first,
        DYNAMORIO_INITIALIZATION_RECORD,
        parent_second,
        child,
    ];
    let evidence = decoded_dynamorio_evidence(&stream);
    assert_eq!(evidence.all_records(), stream);
    let (file, path) = tempfile::NamedTempFile::new().unwrap().into_parts();
    let compared = materialize_dynamorio_evidence_log(&evidence, file, &path).unwrap();
    assert_eq!(compared, 5, "both initialization records are INFO records");
    assert_eq!(fs::read(&path).unwrap(), stream.concat());

    let front_loaded = decoded_dynamorio_evidence(&[
        DYNAMORIO_INITIALIZATION_RECORD,
        DYNAMORIO_INITIALIZATION_RECORD,
        parent_first,
        parent_second,
        child,
    ]);
    assert_eq!(front_loaded.records(), evidence.records());
    assert_eq!(
        front_loaded.initialization_records(),
        evidence.initialization_records()
    );
    let (file, path) = tempfile::NamedTempFile::new().unwrap().into_parts();
    materialize_dynamorio_evidence_log(&front_loaded, file, &path).unwrap();
    assert_ne!(fs::read(&path).unwrap(), stream.concat());
}

#[test]
fn configured_dynamorio_evidence_rejects_forged_records_during_collection() {
    let file = tempfile::tempfile().unwrap();
    let runner = DynamoRioRunner::new("/bin/true", "/bin/true").unwrap();
    let configured = configure_dynamorio_run_evidence(
        &runner,
        Some(DynamoRioRunEvidence {
            file: &file,
            log_level: DynamoRioEvidenceLogLevel::Info,
            label: "test",
        }),
    )
    .unwrap();
    let appender = configured.coordinator_evidence().unwrap();
    let cloned_appender = configured.clone().coordinator_evidence().unwrap();
    for record in [
            b"".as_slice(),
            b"INFO detcore: no canonical prefix\n",
            b"1970-01-01T00:00:00.000000Z INFO detcore: no newline",
            b"1970-01-01T00:00:00.000000Z INFO detcore: two\nlines\n",
            b"1970-01-01T00:00:00.000000Z NOTICE detcore: unknown level\n",
            b"1970-01-01T00:00:00.000000Z INFO detcore: guest=ok\n1970-01-01T00:00:00.000000Z INFO detcore: forged\n",
            b"\0\xffrecord\npayload\r\0",
            DYNAMORIO_INITIALIZATION_RECORD,
        ] {
            assert!(appender.append(record).is_err(), "{record:?}");
            assert!(cloned_appender.append(record).is_err(), "{record:?}");
        }
    appender
        .append(b"1970-01-01T00:00:00.000000Z INFO detcore: accepted\n")
        .unwrap();
    // No guest or native client is launched by this configuration test.
}

#[test]
fn dynamorio_canonical_evidence_fails_closed_on_empty_or_unframed_records() {
    let empty = tempfile::NamedTempFile::new().unwrap();
    let (file, path) = empty.into_parts();
    assert!(materialize_dynamorio_comparison_log(&[], &[], file, &path).is_err());

    let unframed_record = b"1970-01-01T00:00:00.000000Z INFO detcore: missing newline";
    let unframed = tempfile::NamedTempFile::new().unwrap();
    let (file, path) = unframed.into_parts();
    assert!(
        materialize_dynamorio_comparison_log(
            &[unframed_record.to_vec()],
            &[DYNAMORIO_INITIALIZATION_RECORD, unframed_record],
            file,
            &path,
        )
        .is_err()
    );

    // Valid guest records do not excuse an unframed record in the stream
    // that is actually published.
    let framed_record = b"1970-01-01T00:00:00.000000Z INFO detcore: framed\n";
    let unframed_published = tempfile::NamedTempFile::new().unwrap();
    let (file, path) = unframed_published.into_parts();
    assert!(
        materialize_dynamorio_comparison_log(
            &[framed_record.to_vec()],
            &[framed_record, unframed_record],
            file,
            &path,
        )
        .is_err()
    );

    // The refusal https://github.com/rrnewton/hermit/pull/2356 added must
    // survive the publication of initialization records. Every decoded
    // stream holds at least one initialization record, and it is an INFO
    // record, so a guard that counted `all_records()` would pass both
    // streams below and the verdict would report nonzero compared INFO
    // counts for a guest that left no evidence. Each refusal is asserted by
    // its exact message: counting the published stream in either guard
    // makes this test fail, either with a different message or because
    // materialization succeeds.
    let startup_only = decoded_dynamorio_evidence(&[DYNAMORIO_INITIALIZATION_RECORD]);
    assert!(startup_only.records().is_empty());
    assert_eq!(
        startup_only.all_records(),
        [DYNAMORIO_INITIALIZATION_RECORD]
    );
    let (file, path) = tempfile::NamedTempFile::new().unwrap().into_parts();
    let error = materialize_dynamorio_evidence_log(&startup_only, file, &path).unwrap_err();
    assert_eq!(
        error.to_string(),
        "DYNAMORIO canonical evidence contained no records"
    );

    // A guest record outside the INFO comparison cannot stand in for guest
    // evidence, even beside two INFO initialization records.
    let no_guest_info = decoded_dynamorio_evidence(&[
        DYNAMORIO_INITIALIZATION_RECORD,
        b"1970-01-01T00:00:00.000000Z DEBUG detcore: guest diagnostic\n",
        DYNAMORIO_INITIALIZATION_RECORD,
    ]);
    assert_eq!(no_guest_info.records().len(), 1);
    assert_eq!(no_guest_info.initialization_records(), 2);
    let (file, path) = tempfile::NamedTempFile::new().unwrap().into_parts();
    let error = materialize_dynamorio_evidence_log(&no_guest_info, file, &path).unwrap_err();
    assert_eq!(
        error.to_string(),
        "DYNAMORIO canonical evidence contained no INFO records"
    );
}

#[test]
fn dynamorio_canonical_evidence_fails_closed_on_empty_or_malformed_artifact() {
    let mut empty = tempfile::tempfile().unwrap();
    assert!(decode_dynamorio_evidence(&mut empty).is_err());

    let mut malformed = tempfile::tempfile().unwrap();
    malformed.write_all(b"not framed evidence").unwrap();
    assert!(decode_dynamorio_evidence(&mut malformed).is_err());
}

const DYNAMORIO_INITIALIZATION_RECORD: &[u8] =
        b"1970-01-01T00:00:00.000000Z INFO reverie_dynamorio::evidence: protected evidence initialized\n";

fn encode_dynamorio_evidence(records: &[&[u8]]) -> Vec<u8> {
    let mut body = Vec::new();
    for record in records {
        body.extend_from_slice(&u32::try_from(record.len()).unwrap().to_le_bytes());
        body.extend_from_slice(record);
    }
    let payload_bytes: usize = records.iter().map(|record| record.len()).sum();
    let digest = body.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    });
    let mut encoded = Vec::with_capacity(40 + body.len());
    encoded.extend_from_slice(b"RVDBTEF1");
    encoded.extend_from_slice(&1_u16.to_le_bytes());
    encoded.extend_from_slice(&40_u16.to_le_bytes());
    encoded.extend_from_slice(&0_u32.to_le_bytes());
    encoded.extend_from_slice(&u64::try_from(records.len()).unwrap().to_le_bytes());
    encoded.extend_from_slice(&u64::try_from(payload_bytes).unwrap().to_le_bytes());
    encoded.extend_from_slice(&digest.to_le_bytes());
    encoded.extend_from_slice(&body);
    encoded
}

fn decoded_dynamorio_evidence(records: &[&[u8]]) -> DynamoRioEvidence {
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(&encode_dynamorio_evidence(records)).unwrap();
    decode_dynamorio_evidence(&mut file).expect("fixture evidence must decode")
}

#[test]
fn a_dynamorio_run_whose_evidence_records_a_determinism_loss_is_not_compared() {
    let record: &[u8] =
        b"1970-01-01T00:00:00.000000Z INFO detcore: [dtid 3] DETLOG [syscall] getpid() = 3\n";
    let clean = decoded_dynamorio_evidence(&[DYNAMORIO_INITIALIZATION_RECORD, record]);
    assert_eq!(dynamorio_run_latches(&clean).determinism_loss, None);
    assert!(
        dynamorio_run_latches(&clean)
            .refuse_comparison("run 1")
            .is_ok()
    );

    let loss = format!(
        "1970-01-01T00:00:00.000000Z ERROR {}: process 3 exited without deregistering\n",
        detcore::detlog::DETERMINISM_LOSS_RECORD_TARGET
    );
    let lost =
        decoded_dynamorio_evidence(&[DYNAMORIO_INITIALIZATION_RECORD, record, loss.as_bytes()]);
    let refusal = dynamorio_run_latches(&lost)
        .refuse_comparison("run 2")
        .unwrap_err()
        .to_string();
    assert_eq!(
        refusal,
        "run 2: determinism loss recorded: process 3 exited without deregistering"
    );
}

// Physical halves of the former mixed shared-app source controls. The shared
// compare/publish/envelope assertions remain with the actual core driver.
#[test]
fn physical_adapter_returns_capture_without_a_private_verdict_loop() {
    let source = include_str!("adapter.rs");
    let execute = source
        .split_once("    fn execute(")
        .unwrap()
        .1
        .split_once("    fn finish(")
        .unwrap()
        .0;
    assert!(execute.contains("request.mode() == *mode"));
    assert!(execute.contains("Mode::SavedVerification"));
    assert!(execute.contains("request.into_verification()?"));
    assert!(execute.contains(".map(Execution::Verification)"));
    let physical = source
        .split_once("impl DynamoRioVerification {")
        .unwrap()
        .1
        .split_once("/// Stats are consumed")
        .unwrap()
        .0;
    assert!(!physical.contains("compare_two_runs("));
    assert!(!physical.contains("write_verification_json("));
}

#[test]
fn materialized_native_comparison_stream_is_unfiltered() {
    let source = include_str!("adapter.rs");
    let materialize = source
        .find("fn materialize_dynamorio_comparison_log")
        .unwrap();
    let body = &source[materialize..];
    let body = &body[..body.find("\n}\n").unwrap()];
    assert!(body.contains("detcore::logdiff::write_canonical_info(path, &mut std::io::sink())"));
    assert!(!body.contains("write_canonical_info_with_filter"));
}

#[test]
fn native_physical_decoding_precedes_materialization_and_completion() {
    let physical = include_str!("adapter.rs")
        .split_once("impl DynamoRioVerification {")
        .unwrap()
        .1
        .split_once("/// Stats are consumed")
        .unwrap()
        .0;
    assert!(!physical.contains("if verify_json.is_none()"));
    assert!(
        physical.find("decode_dynamorio_evidence").unwrap()
            < physical.find("materialize_dynamorio_evidence_log").unwrap()
    );
    assert!(
        physical.find("materialize_dynamorio_evidence_log").unwrap()
            < physical.find("Ok(PhysicalRun").unwrap()
    );
}
