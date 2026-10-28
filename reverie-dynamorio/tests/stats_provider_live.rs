/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Live end-to-end validation of the DYNAMORIO backend-stats provider.
//!
//! This exercises the full transport path: the native client's `event_exit`
//! appends one fixed-size wire record per real runtime image to the sink file,
//! and [`DynamoRioRunner::output_with_global_and_stats`] drains and decodes those
//! records into a [`reverie_dynamorio::DynamoRioBackendStatsSource`]. Because the C encoder
//! and the Rust codec are independent implementations of the same byte layout,
//! a successful decode with sane field values is the byte-exact ABI check: a
//! layout mismatch would either fail to decode (a hard `WireError`) or surface
//! garbage in the typed fields.
//!
//! It is `#[ignore]`d because it requires a built DynamoRIO tree and the
//! reverie-dynamorio native client, which the hosted CI runner does not provide. Run
//! it locally after `scripts/build-client.sh` with:
//!
//! ```text
//! DYNAMORIO_HOME=<...> REVERIE_DYNAMORIO_CLIENT=<...>/libreverie_dynamorio_client.so \
//!   cargo test -p reverie-dynamorio --test stats_provider_live -- --ignored --nocapture
//! ```

use std::process::Command;

use reverie_dynamorio::Counter2Global;
use reverie_dynamorio::DynamoRioRunner;

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires a built DynamoRIO and the reverie-dynamorio native client; run with --ignored"]
async fn dynamorio_backend_stats_provider_reports_live_translation_metrics() {
    let runner = DynamoRioRunner::from_env()
        .expect("DYNAMORIO_HOME (or DynamoRIO_DIR) and REVERIE_DYNAMORIO_CLIENT must be set")
        .private_environment_key("HERMIT_DYNAMORIO_COUNTER2")
        .expect("configure private Counter2 control");

    // A short-lived tree: the shell image plus several followed `/bin/true`
    // execs. Every real runtime image should append exactly one wire record.
    let mut guest = Command::new("/bin/sh");
    guest
        .args(["-c", "for i in 1 2 3; do /bin/true; done"])
        .env("HERMIT_DYNAMORIO_COUNTER2", "1");

    let (output, _global, stats) = runner
        .output_with_global_and_stats::<Counter2Global>(&guest, ())
        .await
        .expect("coordinated stats run should complete");
    assert!(
        output.status.success(),
        "guest exited unsuccessfully: {output:?}"
    );

    let source = stats.expect("stats were requested, so the source must be Some");
    let snapshot = source.snapshot();
    eprintln!("live DYNAMORIO stats: {snapshot}");

    // A successful drain already proves magic/version/record-length decode; these
    // assertions pin the u64 field offsets to real, non-degenerate values.
    assert!(
        snapshot.process_images() >= 1,
        "at least the root shell image must report a record"
    );

    let translation = snapshot.translation();
    assert!(
        translation.process_images_with_stats() >= 1,
        "at least one image must carry DynamoRIO translation stats"
    );
    assert!(
        translation.basic_blocks_built() > 0,
        "real execution must translate at least one basic block"
    );
}
