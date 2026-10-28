#!/usr/bin/env bash
# Copyright (c) Meta Platforms, Inc. and affiliates.
# All rights reserved.
# This source code is licensed under the BSD-style license in the root LICENSE.

# Backend preparation and data selection only. The core comparator owns every
# comparison, timeout, alignment and ledger-staleness predicate.
set -uo pipefail

usage() {
    cat <<'EOF'
usage: reverie-dynamorio-examples/e2e/run.sh
         --core-comparator ELF --core-e2e-dir DIR
         [--backends dynamorio] [--environment NAME] [--artifacts DIR]
         [--report-only] [--write-ledger FILE]

ELF is the actual core reverie-e2e-compare artifact. DIR is the exact core
reverie-examples/e2e data directory. Both are required; no PATH, sibling or
source-name fallback selects them. Build/source identity must be bound by the
normal caller before execution. The extension owns its normal recorder,
backend preparation and historical ledger; the core owns the corpus/comparator.
EOF
}

E2E_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd -- "$E2E_DIR/../.." && pwd)"
core_comparator=
core_e2e_dir=
environment=
artifacts=
report_only=0
write_ledger=
while (($#)); do
    case $1 in
        --core-comparator) core_comparator=${2:?--core-comparator needs an ELF}; shift 2 ;;
        --core-e2e-dir) core_e2e_dir=${2:?--core-e2e-dir needs a directory}; shift 2 ;;
        --backends)
            [[ ${2:?--backends needs a list} == dynamorio ]] || { usage >&2; exit 2; }
            shift 2 ;;
        --environment) environment=${2:?--environment needs a name}; shift 2 ;;
        --artifacts) artifacts=${2:?--artifacts needs a directory}; shift 2 ;;
        --report-only) report_only=1; shift ;;
        --write-ledger) write_ledger=${2:?--write-ledger needs a file}; shift 2 ;;
        -h | --help) usage; exit 0 ;;
        *) usage >&2; exit 2 ;;
    esac
done
if [[ -z $core_comparator || ! -x $core_comparator ||
      -z $core_e2e_dir || ! -f $core_e2e_dir/corpus.tsv ]]; then
    usage >&2
    exit 2
fi
if [[ -z $environment ]]; then
    os_id=unknown
    os_version=unknown
    if [[ -r /etc/os-release ]]; then
        os_id=$(. /etc/os-release && printf '%s' "${ID:-unknown}")
        os_version=$(. /etc/os-release && printf '%s' "${VERSION_ID:-unknown}")
    fi
    glibc=$(getconf GNU_LIBC_VERSION 2>/dev/null | awk '{ print $2 }')
    environment="$os_id-$os_version-glibc${glibc:-unknown}"
fi

target_dir=${CARGO_TARGET_DIR:-$ROOT_DIR/target}
binaries=$target_dir/debug
artifacts=${artifacts:-$target_dir/e2e}
mkdir -p "$artifacts" || exit 2
say() { printf 'reverie dynamorio e2e: %s\n' "$*"; }
if ! cargo build --locked --manifest-path "$ROOT_DIR/Cargo.toml" \
    -p reverie-dynamorio-examples --lib --bins; then
    say "build failed"
    exit 2
fi
# Preserve the original backend's unavailable-versus-build-failure distinction.
if ! command -v cmake >/dev/null; then
    say "REFUSED dynamorio: cmake, which builds DynamoRIO and the client, is not on PATH"
    exit 1
elif [[ ! -f $ROOT_DIR/third-party/dynamorio/CMakeLists.txt ]]; then
    say "REFUSED dynamorio: the pinned full DynamoRIO source is missing"
    exit 1
elif ! CARGO_TARGET_DIR=$target_dir bash "$ROOT_DIR/reverie-dynamorio/scripts/build-client.sh" \
    >"$artifacts/dynamorio-client-build.log" 2>&1; then
    say "the DynamoRIO client failed to build ($artifacts/dynamorio-client-build.log)"
    exit 2
fi

ledger=$E2E_DIR/ledgers/$environment.tsv
compare=(
    "$core_comparator"
    --recorder "$binaries/dynamorio-recorder"
    --backends dynamorio
    --corpus "$core_e2e_dir/corpus.tsv"
    --artifacts "$artifacts"
)
if [[ -n $write_ledger ]]; then
    compare+=(--write-ledger "$write_ledger")
fi
if ((report_only == 0)) && [[ -f $ledger ]]; then
    say "judging against ledgers/$environment.tsv"
    compare+=(--expected "$ledger")
else
    if ((report_only == 0)); then
        say "NO LEDGER for environment $environment: verdicts are reported, not judged. Measure one with --write-ledger."
    fi
    compare+=(--report-only)
fi
cd "$ROOT_DIR" || exit 2
"${compare[@]}"
