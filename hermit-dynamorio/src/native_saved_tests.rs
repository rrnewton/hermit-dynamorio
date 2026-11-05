/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

use super::*;
use reverie_dynamorio::backend_stats::{DynamoRioProcessRecord, encode_process_record};

fn record() -> DynamoRioProcessRecord {
    DynamoRioProcessRecord {
        process_identity: 17,
        branches: 11,
        syscalls: 23,
        rewritten: 5,
        stdin_reads: 2,
        memory_hash: 0x1234_abcd,
        ..DynamoRioProcessRecord::default()
    }
}

#[test]
fn native_saved_payload_round_trips_raw_bytes_and_all_six_facts() {
    for mode in [
        Mode::Ordinary,
        Mode::VerificationPair,
        Mode::SavedVerification,
    ] {
        DynamoRioAdapter.validate_mode(mode).unwrap();
    }
    let raw = encode_process_record(&record()).to_vec();
    let original = DynamoRioRunFacts::restore(Ok(&raw), 7).unwrap();
    let saved = DynamoRioAdapter.save_native_evidence(&original).unwrap();
    assert_eq!(saved.raw(), raw);
    assert_eq!(saved.initialization_records(), 7);
    let restored = DynamoRioAdapter
        .restore_native_evidence(saved.as_ref())
        .unwrap();
    let PairEvidence::NativeCounters { first, second } = DynamoRioAdapter
        .pair_evidence(&original, &restored)
        .unwrap()
    else {
        panic!("native saved evidence must stay native");
    };
    for facts in [&first, &second] {
        assert_eq!(facts.counted_branches, 11);
        assert_eq!(facts.intercepted_syscalls, 23);
        assert_eq!(facts.rewritten_syscalls, 5);
        assert_eq!(facts.stdin_reads, 2);
        assert_eq!(facts.memory_hash_fold, 0x1234_abcd);
        assert_eq!(facts.initialization_records, 7);
    }
    assert_eq!(first.statistics_diagnostic, second.statistics_diagnostic);
    assert_eq!(restored.raw, raw);
}

#[test]
fn native_saved_restore_keeps_missing_and_unreadable_diagnostics() {
    let missing = std::io::Error::from_raw_os_error(libc::ENOENT);
    let unreadable = std::io::Error::from_raw_os_error(libc::EACCES);
    let missing_error = DynamoRioRunFacts::restore(Err(&missing), 7)
        .unwrap_err()
        .to_string();
    let unreadable_error = DynamoRioRunFacts::restore(Err(&unreadable), 7)
        .unwrap_err()
        .to_string();
    assert!(missing_error.contains("typed whole-process statistics were missing"));
    assert!(unreadable_error.contains("failed to read typed whole-process statistics"));
    assert!(unreadable_error.contains(&unreadable.to_string()));
    assert_ne!(missing_error, unreadable_error);
}

#[test]
fn native_saved_restore_uses_the_live_decoder_and_completeness_refusals() {
    let valid = encode_process_record(&record()).to_vec();
    let mut bad_magic = valid.clone();
    bad_magic[0] ^= 1;
    let mut inconsistent = record();
    inconsistent.rewritten = inconsistent.syscalls + 1;
    let mut no_branches = record();
    no_branches.branches = 0;
    let mut no_syscalls = record();
    no_syscalls.syscalls = 0;
    no_syscalls.rewritten = 0;
    for raw in [
        Vec::new(),
        valid[..valid.len() - 1].to_vec(),
        bad_magic,
        encode_process_record(&inconsistent).to_vec(),
        encode_process_record(&no_branches).to_vec(),
        encode_process_record(&no_syscalls).to_vec(),
    ] {
        let expected = require_dynamorio_stats(&DynamoRioStatsCapture::Records(raw.clone()))
            .unwrap_err()
            .to_string();
        let actual = DynamoRioRunFacts::restore(Ok(&raw), 7)
            .unwrap_err()
            .to_string();
        assert_eq!(actual, expected);
    }
}

#[test]
fn native_saved_initialization_count_uses_checked_metadata_conversion() {
    let raw = encode_process_record(&record()).to_vec();
    for count in [0, 1, 7, u64::MAX] {
        match usize::try_from(count) {
            Ok(expected) => {
                let facts = DynamoRioRunFacts::restore(Ok(&raw), count).unwrap();
                assert_eq!(facts.native_facts().initialization_records, expected);
                let saved = DynamoRioAdapter.save_native_evidence(&facts).unwrap();
                assert_eq!(saved.initialization_records(), count);
            }
            Err(_) => assert!(DynamoRioRunFacts::restore(Ok(&raw), count).is_err()),
        }
    }
}
