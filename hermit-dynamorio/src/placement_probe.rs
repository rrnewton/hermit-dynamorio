/* Copyright (c) Meta Platforms, Inc. and affiliates. */

//! Test-only observations of the selected shared-engine native client launch.
//! The real runner is not replaced; production artifact custody remains active.

use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

pub(crate) struct Probe {
    _directory: tempfile::TempDir,
    root: PathBuf,
    observation: PathBuf,
    record: PathBuf,
    guest_bind: Option<PathBuf>,
    artifacts: hermit_dynamorio::test_support::LaunchArtifacts,
}

impl Probe {
    pub(crate) fn new(contained_cli: bool) -> Self {
        assert!(
            reverie_dynamorio::launch_observation_supported(),
            "placement control requires the real runner's launch-observation test build"
        );
        let artifacts = hermit_dynamorio::test_support::LaunchArtifacts::required()
            .expect("real source-matched shared-engine native client and SDK");
        let target = std::env::var_os("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../target"));
        let directory = tempfile::Builder::new()
            .prefix("dynamorio-placement-")
            .tempdir_in(target)
            .expect("owned placement probe directory");
        let root = fs::canonicalize(directory.path()).unwrap();
        assert!(
            !root.starts_with("/tmp"),
            "the observation must remain visible outside the CLI's private /tmp"
        );
        let record = root.join("argv-record");
        fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&record)
            .unwrap();
        let guest_bind = contained_cli.then(|| Path::new("/tmp").join(root.file_name().unwrap()));
        let observation = guest_bind
            .as_ref()
            .map_or_else(|| record.clone(), |path| path.join("argv-record"));
        fs::write(
            root.join("artifacts.txt"),
            format!(
                "drrun={}\nclient={}\n",
                artifacts.runner().display(),
                artifacts.client().display()
            ),
        )
        .unwrap();
        fs::write(
            root.join("observation.txt"),
            "Actual runner pre-exec; all credentials validated and redacted before append.\n",
        )
        .unwrap();
        eprintln!("DYNAMORIO placement observation: {}", record.display());
        Self {
            _directory: directory,
            root,
            observation,
            record,
            guest_bind,
            artifacts,
        }
    }

    pub(crate) fn launch(&self) -> (&Path, Option<String>) {
        (
            &self.observation,
            self.guest_bind
                .as_ref()
                .map(|bind| format!("--bind={}:{}", self.root.display(), bind.display())),
        )
    }

    pub(crate) fn assert_zero(&self, launches: usize, protected: bool) {
        // Normal tests remove the entire stage, including on assertion failure.
        // Evidence runs may retain only these bounded, credential-redacted files.
        if let Some(destination) = std::env::var_os("HERMIT_DYNAMORIO_PLACEMENT_OBSERVATION_DIR") {
            let destination = PathBuf::from(destination);
            assert!(
                destination.is_absolute() && destination.is_dir(),
                "explicit evidence directory"
            );
            let destination = destination.join(self.root.file_name().unwrap());
            fs::create_dir(&destination).expect("fresh evidence directory");
            for (name, limit) in [
                ("argv-record", 1_048_576),
                ("observation.txt", 65_536),
                ("artifacts.txt", 16_384),
            ] {
                let path = self.root.join(name);
                assert!(
                    fs::metadata(&path).unwrap().len() <= limit,
                    "bounded observation evidence"
                );
                let bytes = fs::read(path).unwrap();
                assert!(bytes.len() as u64 <= limit, "bounded observation evidence");
                fs::write(destination.join(name), bytes).unwrap();
            }
        }
        self.artifacts
            .recheck()
            .expect("real native cohort after physical teardown");
        let text = fs::read_to_string(&self.record).expect("read complete launch observations");
        let mut records = Vec::new();
        let mut current = None;
        let mut incoming = None;
        for line in text.lines() {
            match line {
                "BEGIN" => {
                    assert!(current.is_none(), "interleaved launch records");
                    current = Some(Vec::<Vec<u8>>::new());
                }
                "END" => records.push((
                    incoming.take().expect("incoming argv0"),
                    current.take().expect("record start"),
                )),
                "TOKEN64" | "SOCKET32" => {
                    current
                        .as_mut()
                        .expect("record start")
                        .push(line.as_bytes().to_vec());
                }
                _ => {
                    let is_incoming = line.starts_with('I');
                    let hex = line
                        .strip_prefix(if is_incoming { 'I' } else { 'A' })
                        .expect("known record field");
                    assert_eq!(hex.len() % 2, 0, "complete argument encoding");
                    let bytes = hex
                        .as_bytes()
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|pair| {
                            u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap()
                        })
                        .collect();
                    if is_incoming {
                        assert!(
                            current.as_ref().expect("record start").is_empty(),
                            "incoming argv0 precedes executed argv"
                        );
                        assert!(incoming.replace(bytes).is_none(), "one incoming argv0");
                    } else {
                        current.as_mut().expect("record start").push(bytes);
                    }
                }
            }
        }
        assert!(current.is_none(), "truncated launch observation");
        assert_eq!(
            records.len(),
            launches,
            "exact real launch observation count"
        );
        for (incoming, args) in records {
            assert_eq!(
                incoming,
                self.artifacts.runner().as_os_str().as_bytes(),
                "actual incoming held runner argv0"
            );
            let client = args
                .iter()
                .position(|a| a == b"-c")
                .expect("client boundary");
            self.artifacts
                .check_observed(&incoming, &args[client + 1])
                .expect("actual runner/client and SDK must match the held schema2 cohort");
            let guest = args
                .iter()
                .enumerate()
                .skip(client + 2)
                .find(|(_, a)| *a == b"--")
                .map(|(i, _)| i)
                .expect("guest boundary");
            let placement = args[..client]
                .windows(2)
                .filter(|pair| pair[0] == b"-vm_max_offset")
                .collect::<Vec<_>>();
            assert_eq!(
                placement.len(),
                1,
                "Hermit must select exactly one zero offset"
            );
            assert_eq!(
                placement[0][1], b"0x0",
                "Hermit must select explicit zero offset"
            );
            let controls = &args[client + 2..guest];
            for (flag, value) in [
                (b"-evidence-token".as_slice(), b"TOKEN64".as_slice()),
                (b"-evidence-socket".as_slice(), b"SOCKET32".as_slice()),
            ] {
                assert_eq!(
                    controls.iter().filter(|a| a.as_slice() == flag).count(),
                    usize::from(protected),
                    "exact client-region protected control count",
                );
                assert_eq!(
                    controls
                        .windows(2)
                        .filter(|p| p[0] == flag && p[1] == value)
                        .count(),
                    usize::from(protected),
                    "validated client-region protected control value",
                );
            }
        }
    }
}
