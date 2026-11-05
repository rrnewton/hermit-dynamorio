/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! `libdetcore_dynamorio.so`, the Detcore runtime the DYNAMORIO client loads, with the
//! `reverie_dynamorio_runtime_*` symbols detcore-dynamorio defines.
//!
//! This crate is the shared object only, so that `detcore-dynamorio` can be an
//! rlib alone. A crate that also builds a cdylib cannot be pipelined: its
//! dependents wait for its full build and link instead of starting from its
//! metadata. With detcore-dynamorio and hermit-cli both building one, a cold
//! validate-profile workspace build measured 231 s, against 178 s with the
//! two split out (2026-10-16).

pub use detcore_dynamorio_rlib::*;
