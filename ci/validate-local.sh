#!/usr/bin/env bash
# Local route: the same validate the hosted workflow runs.
# Usage (from a dev-hermit checkout, so bin/safehermit is available):
#   DEV_HERMIT=/path/to/dev-hermit bash ci/validate-local.sh
# Usage (standalone): bash ci/validate-local.sh
# Any hermit binary invocation goes through $DEV_HERMIT/bin/safehermit
# when DEV_HERMIT is set, per dev-hermit harness rules.
set -euo pipefail
cd "$(dirname "$0")/.."

HERMIT_PIN=${HERMIT_PIN:-890f7e672ecd351644f53ab07fc78ad88a76af61}
REVERIE_PIN=${REVERIE_PIN:-42b59ad872680b7e7815fc7a88ed76ca3a233832}
CORE_SRC=${CORE_SRC:-.core/hermit}

if [[ ! -d "$CORE_SRC/ci" ]]; then
    mkdir -p .core
    git clone --quiet https://github.com/rrnewton/hermit.git "$CORE_SRC"
fi
git -C "$CORE_SRC" fetch --quiet origin "$HERMIT_PIN" || true
git -C "$CORE_SRC" checkout --quiet "$HERMIT_PIN"

if [[ -n "${DEV_HERMIT:-}" ]]; then
    export HERMIT_WRAPPER="$DEV_HERMIT/bin/safehermit"
fi

# The four library dispatch tests need an installed core engine cohort
# (HERMIT_DYNAMORIO_ENGINE_ARTIFACT). Without it they are the documented
# hosted residual; with it, the full workspace suite runs.
SKIP_ENGINE_TESTS=()
if [[ -z "${HERMIT_DYNAMORIO_ENGINE_ARTIFACT:-}" ]]; then
    SKIP_ENGINE_TESTS=(
        --skip dynamorio_library_environment_privacy_is_caller_selected
        --skip dynamorio_private_placement_preserves_library_dispatch
        --skip dynamorio_public_dispatch_runs_echo_through_detcore
        --skip dynamorio_public_status_dispatch_runs_true_through_detcore
    )
fi
cargo test -p hermit-dynamorio --lib --locked -- "${SKIP_ENGINE_TESTS[@]}"
cargo test -p hermit-dynamorio --test qualification --no-run --locked
cargo test --workspace --locked --exclude hermit-dynamorio
cargo fmt --all -- --check
cargo clippy --workspace --locked -- -D warnings
CORE_SRC="$CORE_SRC" EXECUTION_OWNER=dynamorio bash ci/run-owned-nextest.sh
