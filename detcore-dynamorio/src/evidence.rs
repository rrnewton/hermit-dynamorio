/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Hermit's tracing-record policy for protected DYNAMORIO evidence.

use std::io;

use crate::DYNAMORIO_LOG_RECORD_PREFIX;

const MAX_RECORD_BYTES: usize = 1024 * 1024;

/// Validates one canonical Hermit tracing record before it enters the evidence.
///
/// Exactly one terminal newline prevents a guest-controlled field from
/// synthesizing another parser-visible INFO record. Reverie separately checks
/// authenticated transport, framing, lifecycle, counts, and digests.
pub fn validate_evidence_record(record: &[u8]) -> io::Result<()> {
    if record.is_empty()
        || record.len() > MAX_RECORD_BYTES
        || record.last() != Some(&b'\n')
        || record[..record.len() - 1]
            .iter()
            .any(|byte| matches!(*byte, b'\r' | b'\n'))
    {
        return Err(invalid_data(
            "DYNAMORIO evidence record is not exactly one terminal-newline tracing record",
        ));
    }
    let tagged = record
        .strip_prefix(DYNAMORIO_LOG_RECORD_PREFIX.as_bytes())
        .ok_or_else(|| {
            invalid_data("DYNAMORIO evidence record has no canonical timestamp prefix")
        })?;
    if ![
        b"ERROR ".as_slice(),
        b"WARN ",
        b"INFO ",
        b"DEBUG ",
        b"TRACE ",
    ]
    .iter()
    .any(|prefix| tagged.starts_with(prefix))
    {
        return Err(invalid_data(
            "DYNAMORIO evidence record has no tracing level tag",
        ));
    }
    Ok(())
}

/// Decodes protected DYNAMORIO evidence using Hermit's collection-time record policy.
pub fn decode_evidence(bytes: &[u8]) -> io::Result<reverie_dynamorio::DynamoRioEvidence> {
    reverie_dynamorio::decode_evidence_with_validator(bytes, validate_evidence_record)
}

fn invalid_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
