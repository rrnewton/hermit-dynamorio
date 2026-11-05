#!/usr/bin/env bash
set -euo pipefail

root=$(git -C "$(dirname -- "${BASH_SOURCE[0]}")" rev-parse --show-toplevel)
# The full fork and all three children are normal pinned submodules. Its Cargo
# producer independently verifies exact clean source/tree/version custody.
make -C "$root" --no-print-directory checkout-all
PROFILE=release "$root/reverie-dynamorio/scripts/build-client.sh"
