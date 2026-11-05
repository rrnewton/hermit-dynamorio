/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! DynamoRIO physical adapter for the one shared Hermit application.
//! The core owns parsing, admission, verification and report publication.

#![deny(clippy::all)]
#![allow(clippy::uninlined_format_args)]

extern crate self as hermit_dynamorio;

pub mod adapter;
mod engine_artifact;
mod native_client;
mod native_contract;
mod native_evidence;
mod physical;
#[doc(hidden)]
pub mod test_support;
pub use adapter::DynamoRioAdapter;

mod library;
pub use library::{run, run_with_output};

/// Validate the actual already-produced native client against its held current engine.
pub use engine_artifact::check_current_engine_client;
