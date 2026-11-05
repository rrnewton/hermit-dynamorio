# Reverie DynamoRIO Backend Prototype

This crate is an x86-64 Linux prototype of an in-process Reverie backend. Its
native DynamoRIO client:

- inserts an atomic 64-bit counter update before every application branch via
  DynamoRIO's `drx` instruction-rewriting helper;
- replaces application `CPUID` instructions with a call into the runtime,
  which answers through its Tool's `handle_cpuid_event`. The client keeps no
  CPU table: under Hermit the answer is Detcore's machine description, the
  same one `/proc/cpuinfo` is rendered from, and the standalone prototype
  runtime forwards the host's answer;
- receives all application syscall entry events without ptrace;
- disables guest ASLR so non-fixed mappings remain stable;
- routes `open`/`openat`/`creat`, `read`/`write`/`close`, the legacy and modern
  stat variants, `lseek`, `access`/`faccessat`/`faccessat2` and a few others
  through a forwarding Rust tool (`PrototypeTool`) and `Guest::inject`,
  returning the injected result while suppressing the original syscall. The
  tool counts these calls and changes none of them: determinism is Detcore's
  job, reached through Hermit's runtime, not this standalone one's;
- launches shebang programs through their interpreter while preserving the exact
  guest environment; and
- optionally reports process totals when the application exits.

## Caller-owned environment privacy

`DynamoRioRunner` leaves ordinary guest variables visible, including application
variables named `HERMIT_DYNAMORIO_*`. A caller can select its private inputs with
`private_environment_key("TOOL_SECRET")` or
`private_environment_prefix("TOOL_PRIVATE_")`. These fallible builders reject
empty selectors and NUL, `:` or `=` bytes; non-UTF8 names are supported. An exact
key does not hide a longer key such as `TOOL_SECRET_SUFFIX`.

The runner always selects its exact coordinator endpoint and native preemption
control (`HERMIT_DYNAMORIO_RPC_SOCKET` and `HERMIT_DYNAMORIO_PREEMPT_ANYPC`); DynamoRIO also
hides its own controls. The encoded selection overrides an inherited or
guest-supplied `DYNAMORIO_HIDE_ENV_PREFIXES`. The runtime receives a private copy
before the guest environment is filtered, and DynamoRIO preserves selected
values across followed fork/exec paths. Runner clones preserve the selection.

Other runtime/Tool flags are visible unless their caller selects them. They
follow ordinary environment inheritance and are not privately reintroduced
after an exec that supplies a replacement environment. The prototype Counter2
bins and tests explicitly select their own keys. Generic example/recorder
runners use the default without an implicit caller prefix. Raw `drrun` scripts
remain responsible for their own environment; scripts and benchmarks invoking
the Counter2 bins inherit those bins' selection.

## Caller-owned private placement

`DynamoRioRunner` leaves DynamoRIO's `vm_max_offset` option unset by default.
Callers that require the historical zero-offset request select
`private_placement(DynamoRioPrivatePlacement::NoRandomOffset)`; selecting
`SdkDefault` afterward removes that request. Clones preserve the selection.
Hermit selects `NoRandomOffset` through its own startup policy.

This controls jitter in the engine's preferred-base private reservation path.
Near-application placement may run first, and relocation or shrinking can still
change the result. The engine's existing random-number source and guest
`ADDR_NO_RANDOMIZE` personality handling are unchanged. Neither choice
guarantees a fixed address, repeatable mappings, or memory determinism.
Private image reservation ownership and named refusal rules remain unchanged.

## Build

DynamoRIO's complete patched source is a recursive Git submodule at
`../third-party/dynamorio`, pinned to
[`d790195243d31ed9da6f5bc867fd4ea0de4f7518`](https://github.com/rrnewton/dynamorio/commit/d790195243d31ed9da6f5bc867fd4ea0de4f7518).
Its three child repositories must be initialized at their exact gitlink
commits. Build from this workspace normally:

```bash
cargo build -p reverie-dynamorio
```

This builds the reusable Rust library, package binaries, and SDK. To produce
the prototype runtime and native client, use `scripts/build-client.sh` (or
`PROFILE=release scripts/build-client.sh` for a release build).

Cargo configures and builds the pinned source in a temporary directory beside
the package `OUT_DIR`, then atomically publishes a content-addressed install in
the Cargo target profile. Distinct package fingerprints (such as `cargo build`
and `cargo doc`) reuse that immutable install; it never mutates its source
checkout and does not fetch a runtime or source bundle. No external SDK or
`DYNAMORIO_HOME` is used. The native client requires `drcontainers`, `drmgr`,
`drreg`, `drwrap` and `drx`. The full source also retains upstream extras,
notices and recursive children; it is not the old pruned vendor tree.

The producer requires Git and verifies all four exact commit/tree identities,
the tracked index, every physical source byte, executable mode and symlink
target. Only their validated Git administration entries are excluded. Source
archives, missing or dirty children, unknown files and incomplete checkouts
refuse. The producer never initializes submodules or fetches missing objects.
Its finite read-only Git operations disable replacement objects, optional
locks and automatic lazy fetching, and clear inherited `GIT_*` overrides. Git
must support the explicit `--no-lazy-fetch` option.

`REVISION` still records `929840ad9190e5086775e8debc0f0b79b4208d59`, the old
curated-source provenance, not the current full-fork identity. A single resolved
absolute Git executable and its bytes enter the cache key and are also passed
to CMake under the same controlled environment. The pinned raw commit time and
observed Git time must agree; generated headers, package version and installed
version are checked before publication. This prevents a differing version
from being published under one key. It does not prove that CMake had no
transient Git failure when its wall-clock fallback happens to select the same
day.

The first build compiles DynamoRIO with its tests, samples, and documentation
disabled. Cargo reuses that install until the build script, build tool recipe,
or pinned source changes. Concurrent first builds publish by atomic rename, so
no consumer can observe or overwrite a partial install.

Clean CI builds enforce a concurrency-normalized source-build ratchet. Three
clean builds measured on 2026-08-03 were 13.91s and 14.54s with 16 jobs on
a development machine and 71.49s with 4 jobs on a GitHub-hosted runner (`n=3`). Their
elapsed-seconds times jobs proxies were 222.56, 232.64, and 285.96
job-seconds. CI rejects a value above 572 job-seconds, twice the slowest
observation rounded up. The job count is the `NUM_JOBS` request, clamped to
1–16 and then capped at the CPUs available to the build
(`std::thread::available_parallelism()`, which follows the affinity mask and
cgroup quota). Jobs beyond that cap could not run in parallel, so the build
neither passes them to cmake nor counts them; the slowest baseline itself ran
4 jobs on a 4-vCPU runner. This is a build-throughput regression guard, not a CPU
time measurement or an ETA. Set `REVERIE_DYNAMORIO_MAX_BUILD_SECONDS` to enforce an
explicit wall-time limit on a controlled machine; local builds otherwise report
actual duration without enforcing a machine-independent wall-time guess.

Run the native client smoke tests directly:

```bash
reverie-dynamorio/scripts/test-echo.sh
reverie-dynamorio/scripts/test-cpuid.sh
```

Set `REVERIE_DYNAMORIO_SUMMARY=1` when using `DynamoRioRunner` to print branch and syscall
totals. The summary is opt-in because its branch count is diagnostic and can
vary between otherwise equivalent runs.

The Rust runtime and native client still have two link phases. Normal Cargo
builds emit a reusable rlib. `build-client.sh` explicitly requests the prototype
`libreverie_dynamorio.so` with `cargo rustc --lib --crate-type cdylib`, then asks the
Cargo-built path helper for `DynamoRIOConfig.cmake` and links the native client
against that Rust runtime. The separate cdylib request preserves Cargo's
hash-bearing dependency artifacts when workspaces with different dependency
features share one target directory. This ordering is required because Cargo build scripts
run before their package's Rust library exists. Per-thread tool state is
allocated by DynamoRIO and stored in `drmgr` TLS. The Rust runtime receives
that state through an explicit C ABI pointer, avoiding dynamic Rust TLS inside
DynamoRIO's private loader.

Private client images and their dependencies require an owned VMM reservation
before their segments are mapped. If a fixed mapping or owned-range restoration
fails, the loader emits a named fatal diagnostic and exits with status 255,
retaining the claim instead of retrying unsafe cleanup. A failure during exit-time
unloading therefore overrides the application's ordinary exit status. Optional
auxiliary-load refusals that retain known ownership can still return NULL after
safe cleanup; mandatory startup ownership refusals terminate without running the
guest uninstrumented. The bootstrap DynamoRIO core keeps its existing owner;
this does not establish independence from changes to that core image's size.

The native `-branch-facts` option enables a separate per-task count of completed
original application conditional instructions from the first application
instruction, including the interpreter and loaded modules. It is disabled by
default because it adds instrumentation to every original conditional. The
internal `DynamoRioGuest::branch_fact` observation is distinct from mixed branch
statistics and `Guest::read_clock`: it does not enable timers or change Detcore
time, costs, or scheduling. Its equality-only owner identifies one live task
and image; samples from different owners cannot be subtracted. A new task starts
at zero, and a newly loaded image has a new scope. Unavailable observations are
explicit errors rather than zero work. This interface is a mechanism prerequisite,
not a continuous clock or a claim of PMU equivalence on every processor.

With branch facts enabled, a returning syscall injection classified by DR's
prehandler as CLONE_VM fails with a named backend error before kernel entry.
This includes requests that Linux might later reject. Errors already established
by DR's prehandler remain Linux errors. The returning bridge lacks the
original/tail path's child-entry machinery; the refusal supplies no fabricated
guest errno and does not silently convert a returning operation into a tail
operation. Original and tail execution use their existing child-entry path.

Collection is off by default. The accompanying cache-context retirement changes
add local release-build checks for failed context reads, translations, and
replacements. Failures return from a helper or suppress its abandonment
notification; the void helpers do not return failure to outer reset/detach
callers, which continue processing. These changes also apply when collection is
disabled; component setter controls do not establish general failed-reset/detach
recovery.

## Prototype Boundaries

- `DynamoRioGuest` implements Reverie's shared `Guest` contract, local memory,
  register reads, syscall injection, in-process global RPC, and a branch clock.
- Tail injection, stack allocation, timers, signals, clone/exec lifecycle, and
  exact post-branch threshold traps remain production-backend work.
- The process-wide branch counter is sampled at syscall boundaries, so the
  displayed total may omit branches after the process's final syscall.
- The synthetic `/proc` snapshots expose only the stable fields needed by the
  current deterministic policy; they are not complete Linux procfs emulation.
- Restartable sequences are disabled in the smoke test so libc selects its
  supported fallback path.

## Third-party licenses

Building this crate compiles the pinned DynamoRIO source and links the host's
build prerequisites. The full copyright, license, and acknowledgement files
are retained under `../third-party/dynamorio`; see those files and the repository
[`NOTICE`](../NOTICE) for attribution and distribution obligations.
