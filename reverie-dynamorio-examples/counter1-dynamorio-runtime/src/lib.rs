/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * This source code is licensed under the BSD-style license in LICENSE.
 */

//! The shared Counter1 Tool hosted by the caller-selected DynamoRIO runtime.

use reverie_examples::tools::counter1 as counter1_tool;

include!(concat!(env!("OUT_DIR"), "/caller_identity.rs"));

reverie_dynamorio::export_caller_tool!(counter1_tool::CounterLocal, CALLER_INPUT_IDENTITY);
