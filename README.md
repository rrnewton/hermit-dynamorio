# Hermit DynamoRIO extension

This repository contains the Hermit and Reverie DynamoRIO backend layers.
`hermit-dynamorio` is a reusable backend adapter with a thin executable over
the shared Hermit application. It does not contain another run or verification
driver. Generic Detcore, Reverie and application policy stay in their core
repositories.

## Source layout

| Directory | Responsibility |
| --- | --- |
| `hermit-dynamorio` | Adapter, backend-owned physical preparation and maintained full Detcore qualification cases |
| `detcore-dynamorio` | Current full Detcore guest integration, moved from `detcore-dbt` |
| `detcore-dynamorio-cdylib` | Final shared-object link for that integration |
| `reverie-dynamorio` | Generic Reverie Guest, runner, native client and SDK producer, moved from `reverie-dbt` |
| `third-party/dynamorio` | Complete patched fork and three exact recursive submodules |
| `support` | Exact shared Tool and C-fixture source inputs from the pinned Reverie core |

The staged core inputs are Hermit
[`1a52541080c50457cbd3b691709885827908d5a6`](https://github.com/rrnewton/hermit/commit/1a52541080c50457cbd3b691709885827908d5a6)
and Reverie
[`05e82111a13bc066654e8dff33a0802ac1ba8a17`](https://github.com/rrnewton/reverie/commit/05e82111a13bc066654e8dff33a0802ac1ba8a17).
Dependencies go from this extension to those exact core inputs; no core
third-party backend feature is enabled. Package versions are preserved move
inputs, not a new version-bump attestation.

The default package selection uses the full Detcore runtime. The generic
Reverie `prototype-runtime` feature and its tests remain available in a separate
`-p reverie-dynamorio --all-features` selection. They implement the same native
runtime ABI, so workspace-wide `--all-features` must not mix them into one
Detcore link.

## Complete DynamoRIO source

Use a recursive Git checkout. The producer requires the complete fork
[`d790195243d31ed9da6f5bc867fd4ea0de4f7518`](https://github.com/rrnewton/dynamorio/commit/d790195243d31ed9da6f5bc867fd4ea0de4f7518)
and all three pinned child commits. It checks the Git identities and every
physical source byte, executable mode and symlink target. Only the four
validated Git administration entries are excluded. Missing Git, source
archives, dirty checkouts, unknown children or incomplete sources refuse;
there is no vendor or downloaded-SDK fallback.

`REVISION` remains historical provenance for the old curated source. It is
not the identity of the complete fork. The producer separately binds the
actual commit, tree, recursive sources, Git executable and generated version
inputs. Its original install, cache, portability and 572 job-second build
ratchet remain in force. See [the producer documentation](reverie-dynamorio/README.md#build).

## Build and qualification status

The source is staged for review. Cargo resolution/lock generation, compilation,
the complete-fork SDK build and all renamed runtime cases have not been run.
The earlier prototype's three full Detcore cases passed with older core and
SDK inputs; those results do not qualify this moved tree.

The executable must be produced with the core's maintained
`ci/build-compatible-cli.rs` path, which measures both actual libtest
populations and performs the final gated build. A source pin or a copied core
lockfile is not a compatibility stamp. The workflow here compiles the three
qualification cases; it does not run Hermit guests. Actual guest qualification
uses the maintained bounded harness route, with real extension and harness
artifact identities and no missing-runtime early success.

Ordinary launch and strict live-pair verification are the current target.
External saved-record/offline verification explicitly refuses before resource
or writer setup. The shared compiled in-guest payload, the `014` integration
and final public source/compatibility bindings remain open. The current
`detcore-dynamorio` guest is the moved full Detcore implementation, not evidence
of one compiled engine reused across backends.

See [NOTICE](NOTICE) for source attribution and [support/README.md](support/README.md)
for the temporary exact textual inputs.

## Explicit native caller artifacts

The reusable `reverie-dynamorio` library, package binaries and SDK build do not
implicitly produce a runtime shared object. `reverie-dynamorio/scripts/build-client.sh`
explicitly builds the prototype runtime shared object and links its native client.
The alternate caller artifacts are independent Cargo workspaces, selected through
that same producer with `--caller-manifest` and `--check`; they are excluded from
the unified workspace so its full Detcore default cannot enable prototype exports
inside a caller DSO. See `reverie-dynamorio/scripts/CALLER_ARTIFACTS.md`.

`dynamorio-strace` uses the one public Tool-only core Strace implementation. Its
producer binds the actual resolved Tool source files and refuses constructor or
host features. Final composed core pins and ordinary regenerated standalone locks
are required before compilation; the original copied fixture locks remain source
provenance, not a claim that the final graph has resolved.
