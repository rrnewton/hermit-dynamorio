/* Copyright (c) Meta Platforms, Inc. and affiliates. */

//! The selected DynamoRIO client links the explicit, source-owned core engine.
//!
//! This is artifact custody and a measured source-cohort check. It does not
//! grant private-image authority or qualify the native mechanism. In particular,
//! an old engine which has not emitted the complete native entry set refuses.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use sha2::{Digest, Sha256};

pub const ENGINE_ARTIFACT_ENV: &str = "HERMIT_DYNAMORIO_ENGINE_ARTIFACT";
const ENGINE: &str = "libhermit_inguest_engine.so";
const HEADER: &str = "hermit-engine-v1.h";
const NATIVE_HEADER: &str = "hermit-engine-native-v1.h";
const MAX_RECORD: u64 = 64 * 1024 * 1024;
const MAX_FILE: u64 = 2 * 1024 * 1024 * 1024;

// These are the actual public declarations in the source-owned native header,
// not the legacy client's ABI16 callback table or a version-only acceptance.
const REQUIRED_NATIVE_EXPORTS: &[&str] = &[
    "hermit_engine_native_image_begin_v1",
    "hermit_engine_native_ready_v1",
    "hermit_engine_native_background_v1",
    "hermit_engine_native_initialize_thread_v1",
    "hermit_engine_native_thread_created_v1",
    "hermit_engine_native_process_clone_result_v1",
    "hermit_engine_native_exec_failed_v1",
    "hermit_engine_native_totals_v1",
    "hermit_engine_native_copied_post_v1",
    "hermit_engine_native_initialize_copied_thread_v1",
    "hermit_engine_native_copied_syscall_v1",
    "hermit_engine_native_syscall_v1",
    "hermit_engine_native_instruction_v1",
    "hermit_engine_native_signal_v1",
    "hermit_engine_native_completed_v1",
    "hermit_engine_native_thread_exit_v1",
    "hermit_engine_native_process_exit_v1",
];

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    file: String,
    bytes: u64,
    sha256: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CompiledUnit {
    package_id: String,
    target: String,
    crate_types: Vec<String>,
    features: Vec<String>,
    profile: serde_json::Value,
    fresh: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema: u32,
    kind: String,
    core_h_revision: String,
    core_h_tree: String,
    core_r_revision: String,
    artifacts: BTreeMap<String, Record>,
    profile: serde_json::Value,
    features: Vec<String>,
    cargo_package_id: String,
    source_records: BTreeMap<String, Record>,
    compiled_units: Vec<CompiledUnit>,
    qualification: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Identity {
    device: u64,
    inode: u64,
    bytes: u64,
    mode: u32,
    mtime: (i64, i64),
    ctime: (i64, i64),
}

impl From<Metadata> for Identity {
    fn from(info: Metadata) -> Self {
        Self {
            device: info.dev(),
            inode: info.ino(),
            bytes: info.size(),
            mode: info.mode(),
            mtime: (info.mtime(), info.mtime_nsec()),
            ctime: (info.ctime(), info.ctime_nsec()),
        }
    }
}

struct HeldFile {
    path: PathBuf,
    file: File,
    identity: Identity,
    sha256: String,
}

impl HeldFile {
    fn open(path: &Path, bound: u64, expected: Option<&Record>, executable: bool) -> Result<Self> {
        ensure!(
            path.is_absolute() && fs::canonicalize(path)? == path,
            "artifact path must be absolute and canonical: {}",
            path.display()
        );
        let before = fs::symlink_metadata(path)?;
        ensure!(
            before.is_file() && !before.file_type().is_symlink(),
            "artifact is not a real regular file: {}",
            path.display()
        );
        ensure!(
            0 < before.len() && before.len() <= bound,
            "artifact exceeds its length bound: {}",
            path.display()
        );
        ensure!(
            !executable || before.mode() & 0o111 != 0,
            "artifact is not executable: {}",
            path.display()
        );
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)?;
        let identity = Identity::from(file.metadata()?);
        ensure!(
            identity == Identity::from(before),
            "artifact changed while opening"
        );
        let sha256 = {
            let mut hash = Sha256::new();
            let mut buffer = [0_u8; 64 * 1024];
            let mut bounded = (&mut file).take(bound + 1);
            let mut total = 0_u64;
            loop {
                let count = bounded.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                total += count as u64;
                ensure!(
                    total <= identity.bytes && total <= bound,
                    "artifact grew while hashing: {}",
                    path.display()
                );
                hash.update(&buffer[..count]);
            }
            ensure!(
                total == identity.bytes,
                "artifact changed length while hashing"
            );
            format!("{:x}", hash.finalize())
        };
        if let Some(record) = expected {
            validate_record(record, bound)?;
            ensure!(
                path.file_name()
                    .is_some_and(|name| name == record.file.as_str())
                    && identity.bytes == record.bytes
                    && sha256 == record.sha256,
                "artifact length/name/SHA-256 differs from the producer: {}",
                path.display()
            );
        }
        let result = Self {
            path: path.to_owned(),
            file,
            identity,
            sha256,
        };
        result.recheck()?;
        Ok(result)
    }

    fn recheck(&self) -> Result<()> {
        let visible = fs::symlink_metadata(&self.path)?;
        ensure!(
            visible.is_file()
                && !visible.file_type().is_symlink()
                && Identity::from(visible) == self.identity
                && Identity::from(self.file.metadata()?) == self.identity
                && fs::canonicalize(&self.path)? == self.path,
            "held or visible artifact changed: {}",
            self.path.display()
        );
        Ok(())
    }

    fn bytes(&self, bound: u64) -> Result<Vec<u8>> {
        use std::os::unix::fs::FileExt;
        self.recheck()?;
        ensure!(self.identity.bytes <= bound, "artifact exceeds read bound");
        let mut bytes = vec![0; usize::try_from(self.identity.bytes)?];
        self.file.read_exact_at(&mut bytes, 0)?;
        ensure!(
            format!("{:x}", Sha256::digest(&bytes)) == self.sha256,
            "held artifact bytes changed: {}",
            self.path.display()
        );
        self.recheck()?;
        Ok(bytes)
    }
}

fn full_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn validate_record(record: &Record, bound: u64) -> Result<()> {
    ensure!(
        !record.file.is_empty()
            && Path::new(&record.file).components().count() == 1
            && matches!(
                Path::new(&record.file).components().next(),
                Some(std::path::Component::Normal(_))
            )
            && record.bytes > 0
            && record.bytes <= bound
            && full_hex(&record.sha256, 64),
        "invalid producer file record"
    );
    Ok(())
}

fn validate_profile(profile: &serde_json::Value) -> Result<()> {
    let profile = profile
        .as_object()
        .context("engine profile is not an object")?;
    for field in [
        "opt_level",
        "debuginfo",
        "debug_assertions",
        "overflow_checks",
        "test",
    ] {
        ensure!(profile.contains_key(field), "engine profile omits {field}");
    }
    ensure!(
        profile.get("test") == Some(&serde_json::Value::Bool(false)),
        "a test artifact is not the core engine"
    );
    Ok(())
}

fn require_native_exports(engine: &HeldFile) -> Result<()> {
    engine.recheck()?;
    let actual = crate::physical::defined_dynamic_symbols_in_file(&engine.file, &engine.path)?;
    for required in REQUIRED_NATIVE_EXPORTS {
        ensure!(
            actual.iter().any(|name| name == required),
            "core engine {} has not emitted required native entry {required}; no legacy runtime is selected",
            engine.path.display()
        );
    }
    engine.recheck()?;
    Ok(())
}

/// Retained across launch, both verification runs and actual teardown.
/// No accepted pathname substitutes for these hashed held objects.
pub(crate) struct NativeArtifacts {
    manifest: HeldFile,
    engine: HeldFile,
    header: HeldFile,
    native_header: HeldFile,
    inputs: Vec<HeldFile>,
    drrun: HeldFile,
    client: HeldFile,
}

impl NativeArtifacts {
    pub(crate) fn from_required_producer_input() -> Result<Self> {
        Self::from_producer_input(true)
    }

    fn from_existing_producer_input() -> Result<Self> {
        Self::from_producer_input(false)
    }

    fn from_producer_input(produce_client: bool) -> Result<Self> {
        let input = std::env::var_os(ENGINE_ARTIFACT_ENV)
            .context(format!("{ENGINE_ARTIFACT_ENV} must name the installed core engine-artifact.json; no runtime is discovered"))?;
        let input = PathBuf::from(input);
        let manifest = HeldFile::open(&input, MAX_RECORD, None, false)?;
        ensure!(
            input
                .file_name()
                .is_some_and(|name| name == "engine-artifact.json"),
            "the explicit input is not engine-artifact.json"
        );
        let record: Manifest = serde_json::from_slice(&manifest.bytes(MAX_RECORD)?)?;
        ensure!(
            record.schema == 2 && record.kind == "hermit-inguest-engine",
            "unsupported core engine artifact schema; schema 1 cannot admit a native client"
        );
        ensure!(
            full_hex(&record.core_h_revision, 40)
                && full_hex(&record.core_h_tree, 40)
                && full_hex(&record.core_r_revision, 40),
            "engine source identity is incomplete"
        );
        // current() refuses dirty/unknown/native/export producers. A path or
        // environment variable cannot supply or replace this measured identity.
        let current = serde_json::to_value(hermit::compatibility::current()?)?;
        ensure!(
            current["source"]["commit"].as_str() == Some(record.core_h_revision.as_str())
                && current["source"]["tree"].as_str() == Some(record.core_h_tree.as_str())
                && current["reverie"]["commit"].as_str() == Some(record.core_r_revision.as_str()),
            "core engine and the executable have different measured H/tree/R source cohorts"
        );
        validate_profile(&record.profile)?;
        ensure!(
            record.features.iter().all(|name| matches!(
                name.as_str(),
                "default" | "dispatch-observer" | "native-first-instruction-observer"
            )) && record.features.iter().collect::<BTreeSet<_>>().len() == record.features.len(),
            "unexpected or duplicate engine features"
        );
        ensure!(
            !record.cargo_package_id.is_empty() && !record.compiled_units.is_empty(),
            "engine Cargo unit provenance is missing"
        );
        let mut units = BTreeSet::new();
        let mut emitted_engine = 0;
        for unit in &record.compiled_units {
            ensure!(
                !unit.package_id.is_empty()
                    && !unit.target.is_empty()
                    && !unit.crate_types.is_empty(),
                "incomplete compiled engine dependency unit"
            );
            // Profile.test is true only for actual test units; the producer is
            // a completed ordinary cdylib build and does not accept them.
            validate_profile(&unit.profile)?;
            let key = serde_json::to_string(&(
                &unit.package_id,
                &unit.target,
                &unit.crate_types,
                &unit.features,
                &unit.profile,
            ))?;
            ensure!(units.insert(key), "duplicate engine compiled unit");
            let _fresh = unit.fresh; // Cargo freshness is recorded, never inferred as new compilation.
            if unit.package_id == record.cargo_package_id
                && unit.target == "hermit_inguest_engine"
                && unit.crate_types == ["cdylib"]
            {
                ensure!(
                    unit.features == record.features && unit.profile == record.profile,
                    "engine artifact differs from its actual Cargo unit"
                );
                emitted_engine += 1;
            }
        }
        ensure!(
            emitted_engine == 1,
            "engine requires exactly one actual cdylib unit"
        );
        ensure!(
            record
                .source_records
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>()
                == BTreeSet::from([
                    "cargo_events",
                    "cargo_metadata",
                    "cargo_lock",
                    "package_manifest",
                    "installer"
                ]),
            "engine source record set is incomplete"
        );
        for row in record.source_records.values() {
            validate_record(row, MAX_RECORD)?;
        }
        ensure!(
            record.qualification
                == "artifact installation only; no loader/runtime/private qualification",
            "unsupported engine qualification declaration"
        );
        ensure!(
            record
                .artifacts
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>()
                == BTreeSet::from(["engine", "header", "native_header"]),
            "engine requires precisely the engine and both header roles"
        );
        let directory = input.parent().context("engine manifest has no directory")?;
        let engine = HeldFile::open(
            &directory.join(ENGINE),
            MAX_FILE,
            Some(&record.artifacts["engine"]),
            true,
        )?;
        let header = HeldFile::open(
            &directory.join(HEADER),
            MAX_FILE,
            Some(&record.artifacts["header"]),
            false,
        )?;
        let native_header = HeldFile::open(
            &directory.join(NATIVE_HEADER),
            MAX_FILE,
            Some(&record.artifacts["native_header"]),
            false,
        )?;
        require_native_exports(&engine)?;
        engine.recheck()?;

        // The build-script source recipe is compiled into reverie-dynamorio;
        // revalidate its actual SDK inventory, not a host-provided recipe name.
        reverie_dynamorio::verify_bundled_sdk()?;
        let source = fs::canonicalize(reverie_dynamorio::native_client_source_dir())?;
        let bridge =
            fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("native/engine_bridge.c"))?;
        let mut inputs = Vec::new();
        let sdk_root = fs::canonicalize(reverie_dynamorio::bundled_dynamorio_home())?;
        for path in [
            source.join("client.c"),
            source.join("CMakeLists.txt"),
            source.join("reverie_vdso_symbols.h"),
            source.join("task_membership.h"),
            source.join("branch_counter.h"),
            source.join("branch_owner_test.h"),
            source.join("private_memory_bootstrap_test.h"),
            source.join("mapping_capture.h"),
            source.join("mapping_capture_test_signals.h"),
            source.join("mapping_capture_test_topology.h"),
            fs::canonicalize(
                source.join("../../third-party/dynamorio/core/unix/bootstrap_private_segments.h"),
            )?,
            bridge.clone(),
            bridge.with_extension("h"),
            bridge.with_file_name("client_engine_entry.inc"),
            bridge.with_file_name("client_engine_bootstrap.inc"),
            bridge.with_file_name("client_engine_runtime.inc"),
            sdk_root.join(".reverie-dynamorio-install.sha256"),
            sdk_root.join(".reverie-dynamorio-install.provenance"),
        ] {
            inputs.push(HeldFile::open(&path, MAX_RECORD, None, false)?);
        }
        let mut identity = Sha256::new();
        identity.update(b"hermit-dynamorio-core-engine-client/v1\0");
        identity.update(manifest.sha256.as_bytes());
        identity.update([0]);
        identity.update(reverie_dynamorio::bundled_sdk_source_recipe().as_bytes());
        for input in &inputs {
            identity.update([0]);
            identity.update(input.path.as_os_str().as_bytes());
            identity.update([0]);
            identity.update(input.sha256.as_bytes());
        }
        let identity = format!("{:x}", identity.finalize());
        let (drrun_path, client_path) = if produce_client {
            crate::native_client::prepare_native_client_for_engine(
                &engine.path,
                &native_header.path,
                &bridge,
                &identity,
            )?
        } else {
            (
                reverie_dynamorio::bundled_drrun_path().to_path_buf(),
                crate::native_client::published_client_for_engine(&engine.path, &identity)?,
            )
        };
        for input in &inputs {
            input.recheck()?;
        }
        reverie_dynamorio::verify_bundled_sdk()?;
        let drrun = HeldFile::open(&fs::canonicalize(drrun_path)?, MAX_FILE, None, true)?;
        let client = HeldFile::open(&fs::canonicalize(client_path)?, MAX_FILE, None, true)?;
        let client_bytes = client.bytes(MAX_FILE)?;
        let elf = goblin::elf::Elf::parse(&client_bytes)?;
        ensure!(
            elf.header.e_machine == goblin::elf::header::EM_X86_64
                && elf.header.e_type == goblin::elf::header::ET_DYN,
            "native client is not an x86-64 shared ELF"
        );
        let engine_name = engine
            .path
            .to_str()
            .context("engine link pathname is not UTF-8")?;
        ensure!(
            elf.libraries
                .iter()
                .filter(|name| **name == engine_name)
                .count()
                == 1,
            "native client does not bind the exact absolute core engine in DT_NEEDED"
        );
        ensure!(
            !elf.libraries
                .iter()
                .any(|name| name.contains("libdetcore_dynamorio")
                    || name.contains("libreverie_dynamorio.so")),
            "native client still links a legacy Rust runtime"
        );
        let result = Self {
            manifest,
            engine,
            header,
            native_header,
            inputs,
            drrun,
            client,
        };
        result.recheck()?;
        Ok(result)
    }

    pub(crate) fn require_native_first_instruction_observer(&self) -> Result<()> {
        // The same held manifest was joined to its one actual compiled cdylib
        // unit above. A path/header/env flag alone cannot declare this feature.
        let record: Manifest = serde_json::from_slice(&self.manifest.bytes(MAX_RECORD)?)?;
        ensure!(
            record
                .features
                .iter()
                .any(|name| name == "native-first-instruction-observer"),
            "first-instruction control requires the actual observer-built core engine"
        );
        self.recheck()
    }

    pub(crate) fn drrun(&self) -> &Path {
        &self.drrun.path
    }
    pub(crate) fn client(&self) -> &Path {
        &self.client.path
    }

    pub(crate) fn recheck(&self) -> Result<()> {
        for file in [
            &self.manifest,
            &self.engine,
            &self.header,
            &self.native_header,
            &self.drrun,
            &self.client,
        ] {
            file.recheck()?;
        }
        for file in &self.inputs {
            file.recheck()?;
        }
        reverie_dynamorio::verify_bundled_sdk()?;
        Ok(())
    }

    /// Check after actual launch/teardown on both success and error. Context
    /// preserves the original typed failure for the shared core classifier.
    pub(crate) fn complete<T>(&self, result: Result<T>) -> Result<T> {
        match (result, self.recheck()) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), Ok(())) => Err(error),
            (Ok(_), Err(error)) => {
                Err(error.context("native artifact custody after physical teardown"))
            }
            (Err(error), Err(custody)) => Err(error.context(format!(
                "native artifact custody also failed after physical teardown: {custody:#}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn held_header_refuses_changed_bytes_replaced_inode_and_symlink() {
        let directory = tempfile::tempdir().unwrap();
        let path = fs::canonicalize(directory.path())
            .unwrap()
            .join(NATIVE_HEADER);
        fs::write(&path, b"actual header input").unwrap();
        let original = HeldFile::open(&path, MAX_RECORD, None, false).unwrap();
        assert_eq!(original.bytes(MAX_RECORD).unwrap(), b"actual header input");
        fs::write(&path, b"changed header input").unwrap();
        assert!(original.recheck().is_err());
        fs::write(&path, b"actual header input").unwrap();
        let original = HeldFile::open(&path, MAX_RECORD, None, false).unwrap();
        let replacement = path.with_extension("replacement");
        fs::write(&replacement, b"actual header input").unwrap();
        fs::rename(&replacement, &path).unwrap();
        assert!(original.recheck().is_err());
        let link = path.with_extension("link");
        symlink(&path, &link).unwrap();
        assert!(HeldFile::open(&link, MAX_RECORD, None, false).is_err());
    }

    #[test]
    fn real_nonengine_elf_does_not_satisfy_native_entry_contract() {
        let path = fs::canonicalize(std::env::current_exe().unwrap()).unwrap();
        let file = HeldFile::open(&path, MAX_FILE, None, true).unwrap();
        assert!(require_native_exports(&file).is_err());
    }
}

/// Check the already-produced current client against its exact held core engine.
/// This follows the same strict source/schema/header/SDK/link custody as a real
/// launch, but never invokes CMake or accepts a caller-supplied client/cache key.
/// Returned names are the actual undefined native imports satisfied by the
/// selected engine's actual loader-visible definitions, not header declarations.
pub fn check_current_engine_client() -> Result<Vec<String>> {
    let artifacts = NativeArtifacts::from_existing_producer_input()?;
    let result = (|| {
        let imports = crate::physical::undefined_dynamic_symbols_in_file(
            &artifacts.client.file,
            &artifacts.client.path,
        )?
        .into_iter()
        .filter(|name| name.starts_with("hermit_engine_native_"))
        .collect::<BTreeSet<_>>();
        ensure!(
            !imports.is_empty(),
            "actual client imports no shared native engine callbacks"
        );
        let exports = crate::physical::defined_dynamic_symbols_in_file(
            &artifacts.engine.file,
            &artifacts.engine.path,
        )?
        .into_iter()
        .collect::<BTreeSet<_>>();
        let missing = imports.difference(&exports).cloned().collect::<Vec<_>>();
        ensure!(
            missing.is_empty(),
            "selected core engine does not define actual client callback imports: {}",
            missing.join(", ")
        );
        Ok(imports.into_iter().collect())
    })();
    artifacts.complete(result)
}
