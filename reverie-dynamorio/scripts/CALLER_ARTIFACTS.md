# External caller artifacts

The Strace DYNAMORIO caller compiles the original
`reverie-examples/strace/{tool,config,filter,global_state}.rs` as a normal
`cdylib`. It depends on `reverie-dynamorio` with `default-features = false` and exports
the complete caller ABI through `export_caller_tool!`. It does not link the
example preload or an in-guest constructor.

The Counter1 DynamoRIO caller compiles the unchanged
`reverie-examples/counter1_tool.rs` through the same export. It uses the actual
`CounterLocal` and returns its `CounterGlobal` to the CLI, which prints the
single `counter1-global syscalls=` summary. Its configuration is explicitly
`()`, as defined by the shared Tool. The separate no-coordinator Counter1
control still uses the prototype runtime and its local GlobalState.

The caller is a standalone Cargo workspace with its own lockfile. The parent
workspace explicitly excludes it so an unrelated `--all-features` build cannot
unify prototype exports into the caller. This is a separate validation boundary:
parent workspace checks remain required and do not validate this artifact. Keep
the OSS caller manifest and its internal build target synchronized on export.

The initial native caller domain requires a single application root without
inherited seccomp restrictions. Before serving Config or admitting the Tool,
the controller reads that owned root task's proc status and requires matching
PID/TGID, seccomp mode zero, and zero filters. An unreadable, incomplete, or
ambiguous status refuses with `caller-inherited-seccomp-unsupported`; cleanup
retains the actual terminated child status and captured output. Child-side
queries remain additional checks, because a filter can spoof their result.
This check does not impose an unfiltered requirement on the trap backend's
own interception filters.

## Produce a pair

Use the repository toolchain, cached Cargo dependencies, and the same existing
target directory and profile as the launcher. First obtain the ordinary
workspace build and its validated DynamoRIO install using the existing build
workflow. The explicit caller producer refuses a missing or stale SDK cache; it
does not initiate a cold SDK build or change the SDK recipe.

From the repository root:

```sh
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=8 \
  reverie-dynamorio/scripts/build-client.sh \
  --caller-manifest reverie-dynamorio-examples/strace-dynamorio-runtime/Cargo.toml \
  --profile debug --jobs 8 --check
target/debug/dynamorio-strace --trace write -- /bin/echo hello

CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=8 \
  reverie-dynamorio/scripts/build-client.sh \
  --caller-manifest reverie-dynamorio-examples/counter1-dynamorio-runtime/Cargo.toml \
  --profile debug --jobs 8 --check
target/debug/dynamorio-counter1 -- /bin/echo hello
```

`CARGO_TARGET_DIR` selects the existing shared target directory; otherwise it is
`target`. The supported profiles are `debug`, `release`, and `validate`.
`validate` inherits release with debug assertions and overflow checks enabled,
and LTO disabled. The caller build uses Cargo's ordinary `--offline --locked`
path, at most eight jobs, and the actual `cdylib` filename reported by Cargo
JSON. It never links a transient rlib or starts a second target directory.
Every child command shares a 900-second producer deadline; invoking CI or local
resource admission may impose a shorter bound.

The wrapper requires `rust-script` for package generation only. It generates the
producer package under `target/ci/reverie-dynamorio-caller-producer`, seeds its lock
from this repository, resolves it offline, and runs ordinary locked Cargo with
the same target, profile and job count. It does not build in rust-script's
private executable cache. The external CI route must prepare `rust-script` through its normal supported
package producer before invoking this wrapper. The wrapper, its generated-workspace
contract, the Rust producer, and the root lock are source-identity inputs.

No-argument `build-client.sh` retains the prototype producer and its package
binaries. It explicitly requests the prototype cdylib after the ordinary
package build. Normal dependencies use a hash-bearing rlib, so a root workspace
build cannot replace a standalone caller's different dependency closure.
`PROFILE` accepts `debug`, `release`, or `validate` for this prototype path.
Supplying
`--caller-manifest` selects explicit caller production. `--artifact-output DIR`
selects a separate output; it refuses a directory that already has a manifest.

To check shared-target artifact reuse, build the root examples, produce this
caller, then repeat both commands without changing source, features, or target:

```sh
cargo test --locked -p reverie-dynamorio-examples --test allocator --no-run
reverie-dynamorio/scripts/build-client.sh \
  --caller-manifest reverie-dynamorio-examples/strace-dynamorio-runtime/Cargo.toml \
  --profile debug --jobs 8
cargo test --locked -p reverie-dynamorio-examples --test allocator --no-run
reverie-dynamorio/scripts/build-client.sh \
  --caller-manifest reverie-dynamorio-examples/strace-dynamorio-runtime/Cargo.toml \
  --profile debug --jobs 8
```

Use the same existing `CARGO_TARGET_DIR` throughout. Both Rust closures must
compile successfully; an existing staged image alone is not evidence of this
regression check. Standalone quality gates and native tests remain separate.

The standard descriptor is
`target/<profile>/reverie-dynamorio-callers/<artifact-label>/manifest.json`. Its paired
images live in immutable `generations/<input-identity>-<pid>-<counter>/`
directories beneath that location. The runtime image retains its actual Cargo-emitted library filename; the native
client is `libreverie_dynamorio_client.so`. The native client embeds the
exact immutable runtime path. Successful production atomically replaces only
the small standard descriptor. Failed production leaves earlier descriptors
and image bytes intact; it never silently reuses an older pair. Generation
cleanup belongs to the normal target-directory lifecycle.

Counter1 and Strace select `--dynamorio-artifact PATH` first, then
`REVERIE_DYNAMORIO_CALLER_ARTIFACT`, then the standard descriptor beside the launcher.
A missing, stale, or malformed explicit selection refuses; it cannot fall back
to the default. For an explicitly staged pair:

```sh
target/debug/dynamorio-strace --dynamorio-artifact /path/to/pair/manifest.json \
  --trace write -- /bin/echo hello
```

## Identity and freshness

The caller build script requires `REVERIE_DYNAMORIO_CALLER_INPUT_IDENTITY` to contain
exactly 64 lowercase hexadecimal characters. Missing or malformed identity is
a build failure. There is no default identity. The producer supplies the same
computed bytes to the caller build and native CMake client.

The digest frames each input, sorted by raw path bytes, as its absolute UTF-8
path, NUL, lowercase SHA256, and newline. Inputs include:

- Every regular file beneath Cargo's resolved local production package roots,
  including their manifests, build scripts, new modules, and the caller lock.
- Explicit caller `package.metadata.reverie-dynamorio.input-roots` directories for
  modules included outside their package. The maintained split callers use resolved core package inputs;
  Counter1 resolves the shared Tool through its exact core `reverie-examples` dependency.
- The parent manifest, lock, toolchain selection, and Cargo configuration.
- Native client/CMake sources, the maintained SDK recipe and vendored inputs,
  producer sources, and the validated SDK install's inventory and provenance.

Source symlinks, missing inputs, constructor-bearing dependencies, and unified
DYNAMORIO prototype exports refuse production. Cargo resolves registry dependencies
against the exact lockfile. The manifest records exact `rustc -Vv` output,
profile, host target, SDK recipe, and hashes of the actual staged runtime and
client. Input identity describes declared source/build inputs; it is not an
ELF self-hash or a substitute for the separate image hashes.

The producer revalidates the existing SDK inventory and recipe before and after
production, and rehashes the complete source closure before publishing. The
loader verifies input and image hashes, the framed identity, and the SDK
inventory's files and symlink targets against its hash-bound provenance.
Native startup performs the paired embedded-identity check. Staging does not
execute the DSO and establishes no Tool activation or native execution result.

## Required validation

The regular and hardware extension CI routes must run the explicit producer with
`--profile debug --check` before nonignored DynamoRIO tests. It uses standard staging and does not
export an identity or a test-only artifact override into unrelated workspace
builds. The hardware job's existing ten-minute resource limit is unchanged.

`--check` runs these standalone gates with the actual generated identity and
the same target/profile before publishing the descriptor:

```text
cargo check --offline --locked --manifest-path <caller> --profile <profile> --jobs 8 --all-targets --all-features
cargo fmt --manifest-path <caller> -- --check
cargo clippy --offline --locked --manifest-path <caller> --profile <profile> --jobs 8 --all-targets --all-features -- -D warnings
cargo rustdoc --offline --locked --manifest-path <caller> --profile <profile> --jobs 8 --lib --all-features -- -D warnings
```

Here Cargo's `dev` profile corresponds to the producer's `debug`. For separately
admitted gates, take the identity from the verified pair's
`runtime_input_identity` field and pass it only to that caller's compiler
command. Never substitute a dummy identity or enable prototype exports to make
a standalone gate succeed. Parent workspace format, Clippy, documentation and
test gates remain unchanged and required.

The original LiteInst Strace tests retain their original assertions and helper.
The separate `dynamorio_exact_strace_tool_observes_filtered_write` and
`dynamorio_exact_strace_tool_observes_thread_and_process_exit` tests reuse those exact
oracle bodies and the same ten-second process-group bound. The override control
first requires a valid default pair, then checks absent/malformed descriptors
and stale source/runtime hashes through both public override forms. These
tests must execute against the real native caller path; source review or a
standalone compilation is not evidence that they passed.

Counter1 retains the original allocator-growth output, nonzero Global count,
exactly-one-summary assertion and ten-second helper in the maintained extension;
the separate ptrace and trap controls stay core-owned. Its caller-selection regression supplies a valid caller descriptor and
an absent prototype client, so success requires the caller route. The override
control checks absent and malformed descriptors through both public override
forms with a valid default pair present; neither may fall back to that pair.
