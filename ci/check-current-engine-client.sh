#!/bin/sh
# Actual current producer/cohort ABI guard. Never builds or discovers a runtime.
set -eu
: "${HERMIT_DYNAMORIO_ENGINE_ARTIFACT:?explicit installed current engine-artifact.json required}"
: "${HERMIT_DYNAMORIO_ENGINE_CHECKER:?explicit normally-built current check-current-engine-client ELF required}"
case "$HERMIT_DYNAMORIO_ENGINE_ARTIFACT" in /*) ;; *) echo 'engine manifest must be absolute' >&2; exit 2 ;; esac
case "$HERMIT_DYNAMORIO_ENGINE_CHECKER" in /*) ;; *) echo 'native checker must be absolute' >&2; exit 2 ;; esac
[ -f "$HERMIT_DYNAMORIO_ENGINE_ARTIFACT" ] || { echo 'current engine manifest missing' >&2; exit 2; }
[ -x "$HERMIT_DYNAMORIO_ENGINE_CHECKER" ] || { echo 'current native checker ELF missing' >&2; exit 2; }
exec "$HERMIT_DYNAMORIO_ENGINE_CHECKER"
