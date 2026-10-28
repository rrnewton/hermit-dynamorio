#!/usr/bin/env bash
set -euo pipefail
: "${HERMIT_CORE_SOURCE:?explicit original core tool source required}"
: "${HERMIT_EXTENSION_ROOT:?explicit extension source required}"
: "${HERMIT_NEXTEST_PROJECT:?required prepared project descriptor}"
core=$(realpath -e -- "$HERMIT_CORE_SOURCE")
project=$(realpath -e -- "$HERMIT_EXTENSION_ROOT")
[[ $core != "$project" && $(realpath -e -- "$PWD") == "$project" ]] || { echo 'owned Nextest: wrong source working directory' >&2; exit 2; }
[[ $HERMIT_NEXTEST_PROJECT == "$project/ci/prepared-project-host.json" || $HERMIT_NEXTEST_PROJECT == "$project/ci/prepared-project-pinned.json" ]] || { echo 'owned Nextest: unknown descriptor' >&2; exit 2; }
export HERMIT_PREPARED_NEXTEST_REQUIRED=1
export HERMIT_RUST_SCRIPT_ARTIFACT_ROOT="$core/target/ci/rust-scripts"
export HERMIT_PREBUILT_RUST_SCRIPTS_REQUIRED=1
export CARGO_TARGET_DIR="$project/target"
export PATH="$core/ci/rust-script-bin:$PATH"
exec "$core/ci/run-with-core-build.sh" "$core/ci/run-nextest-counted.sh" "$@"
