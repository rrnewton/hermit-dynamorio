#!/usr/bin/env python3
"""Publish Cargo's actual core env-guest ELF for the extension allocator tests."""
import argparse
import ctypes
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import tomllib
import time


def require(ok, message):
    if not ok:
        raise RuntimeError(message)


def digest(data):
    return hashlib.sha256(data).hexdigest()


def group_present(pgid):
    try:
        os.killpg(pgid, 0)
        return True
    except ProcessLookupError:
        return False


def wait_owned_group(child, seconds, reaped):
    deadline = time.monotonic() + seconds
    while True:
        # Popen alone reaps its leader. Once it has, reap only descendants
        # adopted from this exact newly-created process group.
        child.poll()
        if child.returncode is not None:
            while True:
                try:
                    pid, status = os.waitpid(-child.pid, os.WNOHANG)
                except ChildProcessError:
                    break
                if pid == 0:
                    break
                reaped.append({"pid": pid, "status": status})
        if not group_present(child.pid):
            return True
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            return False
        time.sleep(min(0.1, remaining))


def run(argv, stdout, stderr, env):
    # Adopt orphaned compiler descendants so our exact owned group can be
    # reaped even when the Cargo leader exits before them. No foreign group
    # or unrelated child is signalled or passed to waitpid.
    libc = ctypes.CDLL(None, use_errno=True)
    libc.prctl.argtypes = [ctypes.c_int] + [ctypes.c_ulong] * 4
    libc.prctl.restype = ctypes.c_int
    require(libc.prctl(36, 1, 0, 0, 0) == 0,
            f"cannot own compiler descendant reaping: errno {ctypes.get_errno()}")
    record = {"argv": argv, "wall_seconds_bound": 900,
              "term_seconds_bound": 30, "kill_reap_seconds_bound": 5,
              "signals": [], "reaped_descendants": [], "first_failure": None}
    started = time.monotonic()
    caught = None
    with stdout.open("xb") as out, stderr.open("xb") as err:
        child = subprocess.Popen(argv, stdout=out, stderr=err, env=env,
                                 start_new_session=True)
        record["pid_and_pgid"] = child.pid
        try:
            child.wait(timeout=900)
        except subprocess.TimeoutExpired:
            record["first_failure"] = "core guest Cargo stage exceeded 900 seconds"
        except BaseException as error:
            caught = error
            record["first_failure"] = f"Cargo wait interrupted: {type(error).__name__}"
        if child.returncode not in (None, 0) and record["first_failure"] is None:
            record["first_failure"] = f"Cargo refused ({child.returncode}); see {stderr}"
        record["group_present_after_leader_wait"] = group_present(child.pid)
        if record["group_present_after_leader_wait"]:
            if record["first_failure"] is None:
                record["first_failure"] = "Cargo leader returned with a live compiler group"
            for sig, seconds in ((signal.SIGTERM, 30), (signal.SIGKILL, 5)):
                if group_present(child.pid):
                    try:
                        os.killpg(child.pid, sig)
                        record["signals"].append(sig.name)
                    except ProcessLookupError:
                        pass  # The exact owned group exited before this signal.
                if wait_owned_group(child, seconds, record["reaped_descendants"]):
                    break
        else:
            wait_owned_group(child, 0, record["reaped_descendants"])
        record["returncode"] = child.poll()
        record["group_absent"] = not group_present(child.pid)
        record["wall_seconds"] = time.monotonic() - started
    record["stdout"] = {"path": str(stdout), "sha256": digest(stdout.read_bytes())}
    record["stderr"] = {"path": str(stderr), "sha256": digest(stderr.read_bytes())}
    record["passed"] = (record["first_failure"] is None
                        and record["returncode"] == 0 and record["group_absent"])
    with stdout.with_name(stdout.name + ".process.json").open("x") as output:
        json.dump(record, output, indent=2)
        output.write("\n")
    if caught is not None:
        raise caught
    require(record["group_absent"] and record["returncode"] is not None,
            f"owned compiler group did not terminate/reap: {record}")
    require(record["passed"], record["first_failure"] or f"Cargo refused; see {stderr}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target-dir", required=True, type=Path)
    parser.add_argument("--destination", required=True, type=Path)
    parser.add_argument("--github-env", required=True, type=Path)
    args = parser.parse_args()
    workspace = Path(__file__).resolve().parents[2]
    target = args.target_dir
    require(target.is_absolute() and target.resolve(strict=True) == target,
            "select the real absolute sole extension target directory")
    destination = args.destination
    require(destination.is_absolute() and destination.parent == target,
            "publication must be a new direct child of the sole target")
    destination.mkdir(mode=0o700)  # Never reuse an older publication.
    env = dict(os.environ, CARGO_TARGET_DIR=str(target), CARGO_BUILD_JOBS="8",
               CARGO_INCREMENTAL="0")
    root_manifest = workspace / "Cargo.toml"
    root_bytes = root_manifest.read_bytes()
    root_lock = (workspace / "Cargo.lock").read_bytes()
    declared = tomllib.loads(root_bytes.decode())["workspace"]["dependencies"]["reverie-examples"]
    revision = declared["rev"]
    require(len(revision) == 40 and all(c in "0123456789abcdef" for c in revision),
            "core fixture must use the exact declared core revision")
    source = f'git+{declared["git"]}?rev={revision}#{revision}'
    metadata_path = destination / "cargo-metadata.json"
    run(["cargo", "metadata", "--manifest-path", str(root_manifest),
         "--locked", "--format-version", "1"], metadata_path,
        destination / "metadata.stderr.log", env)
    metadata = json.loads(metadata_path.read_bytes())
    packages = [p for p in metadata["packages"]
                if p["name"] == "reverie-examples" and p.get("source") == source]
    require(len(packages) == 1, "one source-matched core examples package is required")
    package = packages[0]
    manifest = Path(package["manifest_path"])
    targets = [t for t in package["targets"]
               if t["name"] == "reverie-liteinst-env-guest"
               and t["kind"] == ["bin"] and t["crate_types"] == ["bin"]]
    require(len(targets) == 1, "one actual core env-guest binary target is required")
    expected_target = targets[0]
    src = Path(expected_target["src_path"])
    require(manifest.is_absolute() and manifest.resolve(strict=True) == manifest,
            "core package manifest must be an exact real source path")
    require(src.is_absolute() and src.resolve(strict=True) == src,
            "core guest source must be an exact real source path")
    # A direct build at that authenticated Git source has a local workspace
    # package ID. Join its real metadata by exact manifest/target; never pretend
    # it emits the dependency-context Git package ID from the extension graph.
    core_metadata_path = destination / "core-cargo-metadata.json"
    run(["cargo", "metadata", "--manifest-path", str(manifest), "--locked",
         "--format-version", "1", "--no-deps"], core_metadata_path,
        destination / "core-metadata.stderr.log", env)
    core_metadata = json.loads(core_metadata_path.read_bytes())
    core_packages = [p for p in core_metadata["packages"]
                     if p["name"] == "reverie-examples"
                     and p["manifest_path"] == str(manifest)]
    require(len(core_packages) == 1, "one actual manifest-root core package is required")
    core_package = core_packages[0]
    require([t for t in core_package["targets"]
             if t["name"] == "reverie-liteinst-env-guest"] == [expected_target],
            "actual build target differs from the pinned dependency source target")
    core_root = Path(core_metadata["workspace_root"])
    require(core_root.is_absolute() and core_root.resolve(strict=True) == core_root
            and manifest.is_relative_to(core_root), "core workspace source root differs")
    source_inputs = {p: p.read_bytes() for p in
                     [root_manifest, workspace / "Cargo.lock", manifest,
                      core_root / "Cargo.lock", src]}
    events_path = destination / "cargo-build-events.jsonl"
    run(["cargo", "build", "--manifest-path", str(manifest), "--locked",
         "-p", "reverie-examples", "--no-default-features", "--features", "runtime",
         "--bin", "reverie-liteinst-env-guest", "--jobs", "8",
         "--target-dir", str(target), "--message-format=json"], events_path,
        destination / "build.stderr.log", env)
    events = [json.loads(line) for line in events_path.read_text().splitlines() if line]
    finished = [event for event in events if event.get("reason") == "build-finished"]
    require(len(finished) == 1 and finished[0].get("success") is True,
            "core guest requires one successful completed Cargo build")
    candidates = [event for event in events
                  if event.get("reason") == "compiler-artifact"
                  and event.get("package_id") == core_package["id"]
                  and event.get("manifest_path") == str(manifest)
                  and event.get("target") == expected_target
                  and event.get("profile", {}).get("test") is False
                  and event.get("executable")]
    require(len(candidates) == 1, "one emitted source/target/profile-matched guest ELF is required")
    emitted = Path(candidates[0]["executable"])
    require(emitted.is_absolute() and emitted.resolve(strict=True) == emitted
            and emitted.is_relative_to(target), "guest ELF must belong to the sole target")
    fd = os.open(emitted, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC)
    try:
        identity = os.fstat(fd)
        with os.fdopen(os.dup(fd), "rb") as stream:
            contents = stream.read()
        fresh = os.fstat(fd)
        fields = ("st_dev", "st_ino", "st_mode", "st_size", "st_mtime_ns", "st_ctime_ns")
        require(contents[:4] == b"\x7fELF"
                and all(getattr(fresh, key) == getattr(identity, key) for key in fields),
                "emitted guest must remain the actual unchanged ELF")
    finally:
        os.close(fd)
    held = destination / "env-guest"
    with held.open("xb") as stream:
        stream.write(contents)
    held.chmod(0o555)
    require(held.read_bytes() == contents and emitted.read_bytes() == contents,
            "guest publication differs from Cargo's emitted ELF")
    for path, original in source_inputs.items():
        require(path.read_bytes() == original, f"source or lock changed during guest build: {path}")
    require(root_manifest.read_bytes() == root_bytes
            and (workspace / "Cargo.lock").read_bytes() == root_lock,
            "extension source/lock changed during fixture publication")
    publication = {
        "schema": 1, "role": "reverie-examples-env-guest",
        "source": source, "dependency_package_id": package["id"],
        "build_package_id": core_package["id"], "target": expected_target,
        "profile": candidates[0]["profile"], "emitted_executable": str(emitted),
        "held_executable": str(held), "bytes": len(contents), "sha256": digest(contents),
        "metadata_sha256": digest(metadata_path.read_bytes()),
        "core_metadata_sha256": digest(core_metadata_path.read_bytes()),
        "cargo_events_sha256": digest(events_path.read_bytes()),
        "source_inputs": [{"path": str(p), "bytes": len(data), "sha256": digest(data)}
                          for p, data in source_inputs.items()],
    }
    (destination / "publication.json").write_text(json.dumps(publication, indent=2) + "\n")
    require("\n" not in str(held) and "\r" not in str(held), "invalid env publication path")
    with args.github_env.open("a") as output:
        output.write(f"REVERIE_EXAMPLES_ENV_GUEST={held}\n")
    print(json.dumps(publication, indent=2))


if __name__ == "__main__":
    main()
