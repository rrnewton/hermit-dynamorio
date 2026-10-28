/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Exact maintained canonical epoch and typed-skid-only subprocess helpers.

use std::process::Command;
use std::process::Output;

#[path = "skid_overshoot.rs"]
pub mod skid_overshoot;

pub const SKID_ATTEMPTS: u32 = 5;

pub fn hermit_output(label: &str, command: &mut Command) -> Output {
    hermit_output_attempts(label, SKID_ATTEMPTS, command)
}

pub fn hermit_output_attempts(label: &str, max_attempts: u32, command: &mut Command) -> Output {
    pin_canonical_epoch(command);
    skid_overshoot::retry_typed_skid_refusal(label, max_attempts, || {
        let output = command
            .output()
            .unwrap_or_else(|error| panic!("failed to start {label}: {command:?}: {error}"));
        (output, ())
    })
    .1
}

pub fn hermit_output_with<T>(label: &str, attempt: impl FnMut() -> (Output, T)) -> (Output, T) {
    let (_, output, kept) = skid_overshoot::retry_typed_skid_refusal(label, SKID_ATTEMPTS, attempt);
    (output, kept)
}

pub fn pin_canonical_epoch(command: &mut Command) {
    if std::env::var_os("HERMIT_EPOCH").is_none()
        && !command.get_envs().any(|(key, _)| key == "HERMIT_EPOCH")
    {
        command.env("HERMIT_EPOCH", detcore_model::config::CANONICAL_TEST_EPOCH);
    }
}
