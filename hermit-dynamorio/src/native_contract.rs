/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

// AUTONOMOUS-BOT-IMPLEMENTED

//! Existing host/client wire names and physical placement selections.

pub(crate) const UNSUPPORTED_SYSCALL_REPORT_FD: i32 = 199;

pub(crate) const DYNAMORIO_LOG_RECORD_PREFIX: &str = "1970-01-01T00:00:00.000000Z ";

pub(crate) const CONFIG_FINGERPRINT_ENV: &str = "HERMIT_DYNAMORIO_CONFIG_FINGERPRINT";

pub(crate) const PRIVATE_ENV_PREFIX: &str = "HERMIT_DYNAMORIO_";

pub(crate) const PRIVATE_PLACEMENT: reverie_dynamorio::DynamoRioPrivatePlacement =
    reverie_dynamorio::DynamoRioPrivatePlacement::NoRandomOffset;
