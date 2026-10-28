/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Read-only custody for the complete recursive DynamoRIO fork.
//!
//! A source archive or historical REVISION marker cannot establish this
//! contract. Only the four verified Git administration roots are omitted;
//! every other physical entry must belong to the exact recursive Git trees.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::io::BufRead;
use std::io::BufReader;
use std::io::Read;
use std::io::Write;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::path::PathBuf;
use std::process::Child;
use std::process::ChildStdin;
use std::process::ChildStdout;
use std::process::Command;
use std::process::Stdio;

use sha2::Digest;
use sha2::Sha256;

struct Pin {
    path: &'static str,
    commit: &'static str,
    tree: &'static str,
}

const PINS: &[Pin] = &[
    Pin {
        path: "",
        commit: "d790195243d31ed9da6f5bc867fd4ea0de4f7518",
        tree: "4719aa0f2ae61abd481dc1799158bf6fdea06d65",
    },
    Pin {
        path: "third_party/libipt",
        commit: "c848a85c3104e2f5780741f85de5c9e65476ece2",
        tree: "1776fd54dda401069a8a7f0d76ba1f2c319a537d",
    },
    Pin {
        path: "third_party/zlib",
        commit: "51b7f2abdade71cd9bb0e7a373ef2610ec6f9daf",
        tree: "16b86ef85c591c244523c98c71508be7908d1189",
    },
    Pin {
        path: "third_party/elfutils",
        commit: "302252356da5475670ac5b10dadd091c59689425",
        tree: "589332c287fe3bfca8c20c7a3edb8b6a8235befb",
    },
];

const PINNED_COMMIT_ARGS: &[&str] = &["cat-file", "commit", PINS[0].commit];

#[derive(Clone, Copy)]
enum GitRead {
    TopLevel,
    GitDir,
    Head,
    Tree,
    Entries,
    Index,
    Dirty,
    Blobs,
    Commit,
    Timestamp,
}

impl GitRead {
    fn args(self) -> &'static [&'static str] {
        match self {
            Self::TopLevel => &["rev-parse", "--show-toplevel"],
            Self::GitDir => &["rev-parse", "--absolute-git-dir"],
            Self::Head => &["rev-parse", "--verify", "HEAD"],
            Self::Tree => &["rev-parse", "--verify", "HEAD^{tree}"],
            Self::Entries => &["ls-tree", "-rz", "--full-tree", "HEAD"],
            Self::Index => &["ls-files", "--stage", "-z"],
            Self::Dirty => &[
                "diff",
                "--no-ext-diff",
                "--quiet",
                "--ignore-submodules=none",
                "HEAD",
                "--",
            ],
            Self::Blobs => &["cat-file", "--batch"],
            Self::Commit => PINNED_COMMIT_ARGS,
            Self::Timestamp => &["log", "-n", "1", "--format=%ct"],
        }
    }
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

// Fixed policy is applied to custody, configure, and build/reconfigure alike.
// The explicit CLI flag also refuses older Git that cannot disable lazy fetch.
const GIT_ENVIRONMENT: &[(&str, &str)] = &[
    ("GIT_CONFIG_NOSYSTEM", "1"),
    ("GIT_CONFIG_SYSTEM", "/dev/null"),
    ("GIT_CONFIG_GLOBAL", "/dev/null"),
    ("GIT_NO_REPLACE_OBJECTS", "1"),
    ("GIT_NO_LAZY_FETCH", "1"),
    ("GIT_OPTIONAL_LOCKS", "0"),
    ("GIT_CONFIG_COUNT", "2"),
    ("GIT_CONFIG_KEY_0", "log.showSignature"),
    ("GIT_CONFIG_VALUE_0", "false"),
    ("GIT_CONFIG_KEY_1", "core.fsmonitor"),
    ("GIT_CONFIG_VALUE_1", "false"),
];

#[derive(Debug)]
pub(crate) struct GitContext {
    executable: PathBuf,
    executable_digest: [u8; 32],
    repository_ceiling: PathBuf,
    inherited_git_variables: Vec<OsString>,
}

impl GitContext {
    fn discover(root: &Path) -> io::Result<Self> {
        let path =
            std::env::var_os("PATH").ok_or_else(|| invalid("PATH is required to resolve Git"))?;
        let executable = std::env::split_paths(&path)
            .map(|part| part.join("git"))
            .find(|path| {
                path.metadata()
                    .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
            })
            .ok_or_else(|| invalid("Git executable is unavailable"))?;
        let executable = fs::canonicalize(executable)?;
        let executable_digest = Sha256::digest(fs::read(&executable)?).into();
        let inherited_git_variables = std::env::vars_os()
            .map(|(key, _)| key)
            .filter(|key| key.as_encoded_bytes().starts_with(b"GIT_"))
            .collect();
        let repository_ceiling = fs::canonicalize(root)?
            .parent()
            .ok_or_else(|| invalid("source root has no Git discovery ceiling"))?
            .to_owned();
        Ok(Self {
            executable,
            executable_digest,
            repository_ceiling,
            inherited_git_variables,
        })
    }

    pub(crate) fn executable(&self) -> &Path {
        &self.executable
    }

    pub(crate) fn apply_environment(&self, command: &mut Command) {
        // In particular, inherited GIT_DIR/WORK_TREE/COMMON_DIR/INDEX_FILE,
        // configuration injection, replacement namespaces and trace settings
        // cannot select a different context for CMake than for custody.
        for key in &self.inherited_git_variables {
            command.env_remove(key);
        }
        command.envs(GIT_ENVIRONMENT.iter().copied());
        command.env("GIT_CEILING_DIRECTORIES", &self.repository_ceiling);
    }

    pub(crate) fn configure(&self, command: &mut Command) {
        self.apply_environment(command);
        let mut selected = OsString::from("-DGIT:FILEPATH=");
        selected.push(self.executable.as_os_str());
        command.arg(selected);
    }

    fn verify_executable(&self) -> io::Result<()> {
        let actual: [u8; 32] = Sha256::digest(fs::read(&self.executable)?).into();
        if actual != self.executable_digest {
            return Err(invalid("Git executable changed during SDK preparation"));
        }
        Ok(())
    }

    fn command(&self, root: &Path, operation: GitRead) -> Command {
        let mut command = Command::new(&self.executable);
        self.apply_environment(&mut command);
        command
            .args([
                "--no-replace-objects",
                "--no-lazy-fetch",
                "--no-optional-locks",
                "-C",
            ])
            .arg(root)
            .args(operation.args());
        command
    }
}

fn git_output(git: &GitContext, root: &Path, operation: GitRead) -> io::Result<Vec<u8>> {
    let output = git.command(root, operation).output()?;
    if !output.status.success() || !output.stderr.is_empty() {
        return Err(invalid(format!(
            "Git custody {:?} failed at {} ({}): {}",
            operation.args(),
            root.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(output.stdout)
}

fn git_line(git: &GitContext, root: &Path, operation: GitRead) -> io::Result<String> {
    let value = String::from_utf8(git_output(git, root, operation)?)
        .map_err(|_| invalid("Git custody returned a non-UTF-8 identity"))?;
    let value = value
        .strip_suffix('\n')
        .ok_or_else(|| invalid("truncated Git identity"))?;
    if value.is_empty() || value.contains('\n') || value.contains('\r') {
        return Err(invalid("invalid Git identity line"));
    }
    Ok(value.to_owned())
}

fn gitdir_pointer(contents: &str) -> io::Result<&str> {
    let value = contents
        .strip_prefix("gitdir: ")
        .and_then(|value| value.strip_suffix('\n'))
        .ok_or_else(|| invalid("invalid .git administration pointer"))?;
    if value.is_empty() || value.contains('\n') || value.contains('\r') {
        return Err(invalid("invalid .git administration pointer"));
    }
    Ok(value)
}

fn verify_administration(git: &GitContext, root: &Path) -> io::Result<()> {
    let entry = root.join(".git");
    let metadata = fs::symlink_metadata(&entry).map_err(|error| {
        invalid(format!(
            "Git checkout required at {}: {error}; source archives are unsupported",
            root.display()
        ))
    })?;
    let declared = if metadata.is_dir() {
        entry
    } else if metadata.is_file() {
        root.join(gitdir_pointer(&fs::read_to_string(&entry)?)?)
    } else {
        return Err(invalid(
            "Git administration must be a real directory or regular pointer file",
        ));
    };
    let actual = PathBuf::from(git_line(git, root, GitRead::GitDir)?);
    if fs::canonicalize(declared)? != fs::canonicalize(actual)?
        || fs::canonicalize(root)? != fs::canonicalize(git_line(git, root, GitRead::TopLevel)?)?
    {
        return Err(invalid(
            "Git administration does not belong to the declared source root",
        ));
    }
    Ok(())
}

#[derive(Debug, Eq, PartialEq)]
struct Entry {
    mode: u32,
    oid: String,
}

fn parse_entries(contents: &[u8], index: bool) -> io::Result<BTreeMap<PathBuf, Entry>> {
    if !contents.is_empty() && contents.last() != Some(&0) {
        return Err(invalid("truncated NUL-delimited Git inventory"));
    }
    let mut entries = BTreeMap::new();
    for record in contents
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        let tab = record
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or_else(|| invalid("Git inventory has no path separator"))?;
        let fields = std::str::from_utf8(&record[..tab])
            .map_err(|_| invalid("invalid Git inventory metadata"))?
            .split(' ')
            .collect::<Vec<_>>();
        if fields.len() != 3 {
            return Err(invalid("invalid Git inventory fields"));
        }
        let mode = match fields[0] {
            "100644" => 0o100644,
            "100755" => 0o100755,
            "120000" => 0o120000,
            "160000" => 0o160000,
            _ => return Err(invalid("unknown Git source mode")),
        };
        let oid = if index {
            if fields[2] != "0" {
                return Err(invalid("unmerged source index"));
            }
            fields[1]
        } else {
            if fields[1] != if mode == 0o160000 { "commit" } else { "blob" } {
                return Err(invalid("Git source mode/type mismatch"));
            }
            fields[2]
        };
        if oid.len() != 40
            || !oid
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(invalid("invalid Git source object ID"));
        }
        let name = &record[tab + 1..];
        if name.is_empty()
            || name
                .split(|byte| *byte == b'/')
                .any(|part| part.is_empty() || part == b"." || part == b".." || part == b".git")
        {
            return Err(invalid("unsafe Git source path"));
        }
        let path = PathBuf::from(OsString::from_vec(name.to_vec()));
        if path.is_absolute()
            || entries
                .insert(
                    path,
                    Entry {
                        mode,
                        oid: oid.to_owned(),
                    },
                )
                .is_some()
        {
            return Err(invalid("duplicate or absolute Git source path"));
        }
    }
    Ok(entries)
}

fn verify_gitlinks(entries: &BTreeMap<PathBuf, Entry>, top: bool) -> io::Result<()> {
    let actual = entries
        .iter()
        .filter(|(_, entry)| entry.mode == 0o160000)
        .map(|(path, entry)| (path.clone(), entry.oid.as_str()))
        .collect::<BTreeMap<_, _>>();
    let expected = if top { &PINS[1..] } else { &[] };
    let expected = expected
        .iter()
        .map(|pin| (PathBuf::from(pin.path), pin.commit))
        .collect::<BTreeMap<_, _>>();
    if actual != expected {
        return Err(invalid("missing, changed or unknown recursive source pin"));
    }
    Ok(())
}

struct Blobs {
    child: Child,
    input: Option<ChildStdin>,
    output: BufReader<ChildStdout>,
    finished: bool,
}

impl Blobs {
    fn new(git: &GitContext, root: &Path) -> io::Result<Self> {
        let mut child = git
            .command(root, GitRead::Blobs)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        Ok(Self {
            input: Some(
                child
                    .stdin
                    .take()
                    .ok_or_else(|| invalid("missing Git input pipe"))?,
            ),
            output: BufReader::new(
                child
                    .stdout
                    .take()
                    .ok_or_else(|| invalid("missing Git output pipe"))?,
            ),
            child,
            finished: false,
        })
    }

    fn require_bytes(&mut self, oid: &str, actual: &[u8]) -> io::Result<()> {
        let input = self
            .input
            .as_mut()
            .ok_or_else(|| invalid("closed Git input"))?;
        writeln!(input, "{oid}")?;
        input.flush()?;
        let mut header = String::new();
        self.output.read_line(&mut header)?;
        if header != format!("{oid} blob {}\n", actual.len()) {
            return Err(invalid("missing or differently sized pinned source blob"));
        }
        let mut expected = vec![0; actual.len()];
        self.output.read_exact(&mut expected)?;
        let mut newline = [0];
        self.output.read_exact(&mut newline)?;
        if expected != actual || newline != [b'\n'] {
            return Err(invalid(
                "physical source bytes differ from the pinned Git blob",
            ));
        }
        Ok(())
    }

    fn finish(mut self) -> io::Result<()> {
        drop(self.input.take());
        let status = self.child.wait()?;
        self.finished = true;
        if !status.success() {
            return Err(invalid(format!("Git blob reader failed: {status}")));
        }
        Ok(())
    }
}

impl Drop for Blobs {
    fn drop(&mut self) {
        if !self.finished {
            drop(self.input.take());
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn physical_bytes(path: &Path, mode: u32) -> io::Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    if mode == 0o120000 && metadata.is_symlink() {
        return Ok(fs::read_link(path)?.as_os_str().as_encoded_bytes().to_vec());
    }
    let actual_mode = if metadata.permissions().mode() & 0o111 != 0 {
        0o100755
    } else {
        0o100644
    };
    if !metadata.is_file() || metadata.is_symlink() || mode != actual_mode {
        return Err(invalid(format!(
            "physical source mode differs at {}",
            path.display()
        )));
    }
    fs::read(path)
}

fn hash_entry(hasher: &mut Sha256, path: &Path, mode: u32, contents: &[u8]) {
    let name = path.as_os_str().as_encoded_bytes();
    hasher.update(b"source-entry\0");
    hasher.update(name.len().to_le_bytes());
    hasher.update(name);
    hasher.update(mode.to_le_bytes());
    hasher.update(contents.len().to_le_bytes());
    hasher.update(contents);
}

fn verify_universe(
    root: &Path,
    files: &BTreeSet<PathBuf>,
    administration: &BTreeSet<PathBuf>,
) -> io::Result<()> {
    fn walk(
        root: &Path,
        directory: &Path,
        files: &BTreeSet<PathBuf>,
        administration: &BTreeSet<PathBuf>,
        seen: &mut BTreeSet<PathBuf>,
    ) -> io::Result<()> {
        for entry in fs::read_dir(directory)? {
            let path = entry?.path();
            let relative = path.strip_prefix(root).map_err(io::Error::other)?;
            if administration.contains(relative) {
                continue;
            }
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.is_dir() {
                if !files.iter().any(|file| file.starts_with(relative)) {
                    return Err(invalid(format!(
                        "unknown source directory {}",
                        relative.display()
                    )));
                }
                walk(root, &path, files, administration, seen)?;
            } else if files.contains(relative) {
                seen.insert(relative.to_path_buf());
            } else {
                return Err(invalid(format!(
                    "unknown source entry {}",
                    relative.display()
                )));
            }
        }
        Ok(())
    }
    let mut seen = BTreeSet::new();
    walk(root, root, files, administration, &mut seen)?;
    if &seen != files {
        return Err(invalid("missing physical source entries"));
    }
    Ok(())
}

/// Verify exact commits, indexes, bytes, modes and symlinks before cache lookup.
/// Missing Git, source archives, dirty or incomplete children all refuse.
pub(crate) fn verify_and_hash(root: &Path) -> io::Result<VerifiedSource> {
    let administration = fs::symlink_metadata(root.join(".git")).map_err(|error| {
        invalid(format!(
            "Git checkout required at {}: {error}; source archives are unsupported",
            root.display()
        ))
    })?;
    if !administration.is_file() && !administration.is_dir() {
        return Err(invalid("invalid source Git administration"));
    }
    let git = GitContext::discover(root)?;
    let mut hasher = Sha256::new();
    let mut files = BTreeSet::new();
    let mut administration = BTreeSet::new();
    for pin in PINS {
        let repository = root.join(pin.path);
        verify_administration(&git, &repository)?;
        let commit = git_line(&git, &repository, GitRead::Head)?;
        let tree = git_line(&git, &repository, GitRead::Tree)?;
        if commit != pin.commit || tree != pin.tree {
            return Err(invalid(format!(
                "wrong source at {}: expected {}/{}, observed {commit}/{tree}",
                pin.path, pin.commit, pin.tree
            )));
        }
        let entries = parse_entries(&git_output(&git, &repository, GitRead::Entries)?, false)?;
        if entries != parse_entries(&git_output(&git, &repository, GitRead::Index)?, true)? {
            return Err(invalid(format!("dirty source index at {}", pin.path)));
        }
        verify_gitlinks(&entries, pin.path.is_empty())?;
        administration.insert(Path::new(pin.path).join(".git"));
        hash_entry(
            &mut hasher,
            Path::new(pin.path),
            0o160000,
            pin.commit.as_bytes(),
        );
        hash_entry(&mut hasher, Path::new(pin.path), 0, pin.tree.as_bytes());
        let mut blobs = Blobs::new(&git, &repository)?;
        for (path, entry) in &entries {
            let relative = Path::new(pin.path).join(path);
            if entry.mode == 0o160000 {
                hash_entry(&mut hasher, &relative, entry.mode, entry.oid.as_bytes());
                continue;
            }
            let contents = physical_bytes(&repository.join(path), entry.mode)?;
            blobs
                .require_bytes(&entry.oid, &contents)
                .map_err(|error| invalid(format!("source {}: {error}", relative.display())))?;
            hash_entry(&mut hasher, &relative, entry.mode, &contents);
            files.insert(relative);
        }
        blobs.finish()?;
        git_output(&git, &repository, GitRead::Dirty)?;
    }
    verify_universe(root, &files, &administration)?;
    let raw_commit = git_output(&git, root, GitRead::Commit)?;
    let timestamp = raw_committer_timestamp(&raw_commit)?;
    let observed = git_line(&git, root, GitRead::Timestamp)?;
    if observed != timestamp.to_string() {
        return Err(invalid(
            "Git log timestamp differs from the actual pinned commit",
        ));
    }
    let major_minor = fs::read_to_string(root.join("make/version.txt"))?;
    let components = major_minor.trim().split('.').collect::<Vec<_>>();
    if components.len() != 2
        || components
            .iter()
            .any(|value| value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return Err(invalid("invalid tracked DynamoRIO major/minor version"));
    }
    let major: u32 = components[0]
        .parse()
        .map_err(|_| invalid("version major overflow"))?;
    let minor: u32 = components[1]
        .parse()
        .map_err(|_| invalid("version minor overflow"))?;
    let patch = timestamp / 86400;
    if patch == 0 {
        return Err(invalid(
            "zero Git patchlevel would select CMake's wall-clock fallback",
        ));
    }
    let version = format!("{major}.{minor}.{patch}");
    hash_entry(
        &mut hasher,
        Path::new("<resolved-git>"),
        0,
        git.executable.as_os_str().as_encoded_bytes(),
    );
    hash_entry(
        &mut hasher,
        Path::new("<git-executable-sha256>"),
        0,
        &git.executable_digest,
    );
    hash_entry(
        &mut hasher,
        Path::new("<raw-commit-timestamp>"),
        0,
        &timestamp.to_le_bytes(),
    );
    hash_entry(
        &mut hasher,
        Path::new("<configured-version>"),
        0,
        version.as_bytes(),
    );
    git.verify_executable()?;
    Ok(VerifiedSource {
        digest: hasher.finalize().into(),
        git,
        timestamp,
        major,
        minor,
        patch,
        version,
    })
}

#[derive(Debug)]
pub(crate) struct VerifiedSource {
    pub(crate) digest: [u8; 32],
    pub(crate) git: GitContext,
    timestamp: u64,
    major: u32,
    minor: u32,
    patch: u64,
    version: String,
}

fn raw_committer_timestamp(raw: &[u8]) -> io::Result<u64> {
    let header_end = raw
        .windows(2)
        .position(|bytes| bytes == b"\n\n")
        .ok_or_else(|| invalid("pinned commit has no complete header"))?;
    let mut committers = raw[..header_end]
        .split(|byte| *byte == b'\n')
        .filter(|line| line.starts_with(b"committer "));
    let line = committers
        .next()
        .ok_or_else(|| invalid("pinned commit has no committer"))?;
    if committers.next().is_some() {
        return Err(invalid("duplicate pinned committer"));
    }
    let fields = line
        .rsplit(|byte| *byte == b' ')
        .take(2)
        .collect::<Vec<_>>();
    if fields.len() != 2 || fields[1].is_empty() || !fields[1].iter().all(u8::is_ascii_digit) {
        return Err(invalid("invalid pinned committer timestamp"));
    }
    std::str::from_utf8(fields[1])
        .map_err(|_| invalid("invalid timestamp encoding"))?
        .parse()
        .map_err(|_| invalid("pinned committer timestamp overflow"))
}

fn require_unique_line(contents: &str, expected: &str) -> io::Result<()> {
    let prefix = if expected.starts_with("#define ") {
        format!(
            "#define {} ",
            expected
                .split_whitespace()
                .nth(1)
                .ok_or_else(|| invalid("invalid expected define"))?
        )
    } else if expected.starts_with("set(") {
        format!(
            "{} ",
            expected
                .split_whitespace()
                .next()
                .ok_or_else(|| invalid("invalid expected CMake set"))?
        )
    } else {
        format!(
            "{}=",
            expected
                .split_once('=')
                .ok_or_else(|| invalid("invalid expected cache entry"))?
                .0
        )
    };
    if contents
        .lines()
        .filter(|line| line.starts_with(&prefix))
        .collect::<Vec<_>>()
        != [expected]
    {
        return Err(invalid(format!(
            "missing, duplicate, or mismatched generated version line: {expected}"
        )));
    }
    Ok(())
}

impl VerifiedSource {
    fn expected_config_version(&self, root: &Path) -> io::Result<String> {
        let mut expected = fs::read_to_string(root.join("make/DynamoRIOConfigVersion.cmake.in"))?;
        for (key, value) in [
            ("VERSION_NUMBER", self.version.clone()),
            ("VERSION_NUMBER_MAJOR", self.major.to_string()),
            ("VERSION_NUMBER_MINOR", self.minor.to_string()),
            ("VERSION_NUMBER_PATCHLEVEL", self.patch.to_string()),
        ] {
            expected = expected.replace(&format!("@{key}@"), &value);
        }
        Ok(expected)
    }

    pub(crate) fn verify_generated(&self, root: &Path, build: &Path) -> io::Result<()> {
        self.git.verify_executable()?;
        // Refuse failed rechecks and compare all generated values below; a wall-clock
        // fallback must not publish a version different from the bound commit.
        if git_line(&self.git, root, GitRead::Timestamp)? != self.timestamp.to_string() {
            return Err(invalid("Git timestamp changed during configure/build"));
        }
        self.verify_version_files(root, build)
    }

    fn verify_version_files(&self, root: &Path, build: &Path) -> io::Result<()> {
        let version = fs::read_to_string(build.join("cmake/DynamoRIOConfigVersion.cmake"))?;
        if version != self.expected_config_version(root)? {
            return Err(invalid(
                "generated SDK version/patchlevel differs from pinned commit",
            ));
        }
        let header = fs::read_to_string(build.join("configure.h"))?;
        for line in [
            format!("#define VERSION_NUMBER {}", self.version),
            format!(
                "#define VERSION_COMMA_DELIMITED {},{},{}",
                self.major, self.minor, self.patch
            ),
            format!(
                "#define VERSION_NUMBER_INTEGER {}",
                self.major
                    .checked_mul(100)
                    .and_then(|n| n.checked_add(self.minor))
                    .ok_or_else(|| invalid("version integer overflow"))?
            ),
        ] {
            require_unique_line(&header, &line)?;
        }
        let cache = fs::read_to_string(build.join("CMakeCache.txt"))?;
        require_unique_line(
            &cache,
            &format!(
                "GIT:FILEPATH={}",
                self.git
                    .executable
                    .to_str()
                    .ok_or_else(|| invalid("Git path is not UTF-8 for CMake"))?
            ),
        )?;
        require_unique_line(&cache, "VERSION_NUMBER:STRING=")?;
        for path in ["CPackConfig.cmake", "CPackSourceConfig.cmake"] {
            let package = fs::read_to_string(build.join(path))?;
            for (key, value) in [
                ("CPACK_PACKAGE_VERSION", self.version.clone()),
                ("CPACK_PACKAGE_VERSION_MAJOR", self.major.to_string()),
                ("CPACK_PACKAGE_VERSION_MINOR", self.minor.to_string()),
                ("CPACK_PACKAGE_VERSION_PATCH", self.patch.to_string()),
            ] {
                require_unique_line(&package, &format!("set({key} \"{value}\")"))?;
            }
        }
        Ok(())
    }

    pub(crate) fn verify_installed(&self, root: &Path, install: &Path) -> io::Result<()> {
        let version = fs::read_to_string(install.join("cmake/DynamoRIOConfigVersion.cmake"))?;
        if version != self.expected_config_version(root)? {
            return Err(invalid(
                "installed SDK version/patchlevel differs from pinned commit",
            ));
        }
        Ok(())
    }
}

// Pure file measurement for the pre-existing cache-sensitivity unit fixtures.
// This is absent from production and cannot act as an archive/source fallback.
#[cfg(test)]
pub(crate) fn fixture_digest(root: &Path) -> [u8; 32] {
    fn walk(root: &Path, directory: &Path, hasher: &mut Sha256) {
        let mut entries = fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        entries.sort();
        for path in entries {
            let metadata = fs::symlink_metadata(&path).unwrap();
            if metadata.is_dir() {
                walk(root, &path, hasher);
                continue;
            }
            let mode = if metadata.is_symlink() {
                0o120000
            } else if metadata.permissions().mode() & 0o111 != 0 {
                0o100755
            } else {
                0o100644
            };
            hash_entry(
                hasher,
                path.strip_prefix(root).unwrap(),
                mode,
                &physical_bytes(&path, mode).unwrap(),
            );
        }
    }
    let mut hasher = Sha256::new();
    walk(root, root, &mut hasher);
    hasher.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inventory_parser_keeps_nul_delimited_path_bytes() {
        let row = b"100644 blob 0123456789012345678901234567890123456789\twith\ttab\nand-newline\0";
        let parsed = parse_entries(row, false).unwrap();
        assert!(parsed.contains_key(Path::new("with\ttab\nand-newline")));
        assert!(parse_entries(&row[..row.len() - 1], false).is_err());
        for path in ["/absolute", "../escape", "a/../b", "a//b", ".git/config"] {
            let row = format!("100644 blob 0123456789012345678901234567890123456789\t{path}\0");
            assert!(parse_entries(row.as_bytes(), false).is_err(), "{path}");
        }
    }

    #[test]
    fn inventory_parser_refuses_unknown_modes_types_stages_and_duplicates() {
        for metadata in ["100600 blob", "100644 commit", "160000 blob"] {
            let row = format!("{metadata} 0123456789012345678901234567890123456789\tfile\0");
            assert!(parse_entries(row.as_bytes(), false).is_err());
        }
        let row = b"100644 0123456789012345678901234567890123456789 1\tfile\0";
        assert!(parse_entries(row, true).is_err());
        let row = b"100644 blob 0123456789012345678901234567890123456789\tfile\0";
        assert!(parse_entries(&[row.as_slice(), row.as_slice()].concat(), false).is_err());
    }

    #[test]
    fn recursive_pin_comparison_refuses_missing_wrong_and_unknown_children() {
        let mut entries = PINS[1..]
            .iter()
            .map(|pin| {
                (
                    PathBuf::from(pin.path),
                    Entry {
                        mode: 0o160000,
                        oid: pin.commit.to_owned(),
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        verify_gitlinks(&entries, true).unwrap();
        assert!(verify_gitlinks(&entries, false).is_err());
        entries.insert(
            PathBuf::from("unknown"),
            Entry {
                mode: 0o160000,
                oid: PINS[1].commit.to_owned(),
            },
        );
        assert!(verify_gitlinks(&entries, true).is_err());
        entries.remove(Path::new("unknown"));
        entries.get_mut(Path::new(PINS[1].path)).unwrap().oid = PINS[2].commit.to_owned();
        assert!(verify_gitlinks(&entries, true).is_err());
        entries.remove(Path::new(PINS[1].path));
        assert!(verify_gitlinks(&entries, true).is_err());
    }

    #[test]
    fn measured_source_binds_bytes_executable_mode_and_symlink_target() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("file");
        fs::write(&file, "first").unwrap();
        let first = fixture_digest(directory.path());
        fs::write(&file, "second").unwrap();
        let second = fixture_digest(directory.path());
        assert_ne!(first, second);
        fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).unwrap();
        let executable = fixture_digest(directory.path());
        assert_ne!(second, executable);
        let link = directory.path().join("link");
        std::os::unix::fs::symlink("file", &link).unwrap();
        let linked = fixture_digest(directory.path());
        fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink("missing", &link).unwrap();
        assert_ne!(linked, fixture_digest(directory.path()));
        assert!(physical_bytes(&link, 0o100644).is_err());
    }

    #[test]
    fn physical_universe_has_no_untracked_or_administration_name_exemption() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("known"), "data").unwrap();
        let files = BTreeSet::from([PathBuf::from("known")]);
        let no_administration = BTreeSet::new();
        verify_universe(directory.path(), &files, &no_administration).unwrap();
        fs::create_dir(directory.path().join(".git")).unwrap();
        assert!(verify_universe(directory.path(), &files, &no_administration).is_err());
        fs::remove_dir(directory.path().join(".git")).unwrap();
        fs::write(directory.path().join("ignored-output"), "data").unwrap();
        assert!(verify_universe(directory.path(), &files, &no_administration).is_err());
        fs::remove_file(directory.path().join("ignored-output")).unwrap();
        fs::remove_file(directory.path().join("known")).unwrap();
        assert!(verify_universe(directory.path(), &files, &no_administration).is_err());
    }

    #[test]
    fn git_administration_pointer_is_exact_and_archives_refuse() {
        assert_eq!(
            gitdir_pointer("gitdir: ../modules/fork\n").unwrap(),
            "../modules/fork"
        );
        for input in [
            "../modules/fork\n",
            "gitdir: \n",
            "gitdir: path",
            "gitdir: path\nextra\n",
        ] {
            assert!(gitdir_pointer(input).is_err());
        }
        let directory = tempfile::tempdir().unwrap();
        assert!(
            verify_and_hash(directory.path())
                .unwrap_err()
                .to_string()
                .contains("source archives are unsupported")
        );
    }
    #[test]
    fn raw_committer_timestamp_rejects_malformed_or_duplicate_input() {
        let commit = b"tree deadbeef\ncommitter Example <example@example.invalid> 1793142692 -0700\n\nmessage\n";
        assert_eq!(raw_committer_timestamp(commit).unwrap(), 1793142692);
        for invalid_commit in [
            b"committer Example <example@example.invalid> invalid -0700\n\nmessage".as_slice(),
            b"committer Example <example@example.invalid> 18446744073709551616 -0700\n\nmessage",
            b"tree deadbeef\n\nno committer",
            b"committer X 1793142692 -0700\ncommitter Y 1793142692 -0700\n\nmessage",
            b"committer X 1793142692 -0700\nmissing header terminator",
        ] {
            assert!(raw_committer_timestamp(invalid_commit).is_err());
        }
    }

    fn fixture_source() -> VerifiedSource {
        VerifiedSource {
            digest: [0; 32],
            git: GitContext {
                executable: PathBuf::from("/fixture/git"),
                executable_digest: [0; 32],
                repository_ceiling: PathBuf::from("/fixture"),
                inherited_git_variables: [
                    "GIT_DIR",
                    "GIT_WORK_TREE",
                    "GIT_COMMON_DIR",
                    "GIT_INDEX_FILE",
                    "GIT_CONFIG_COUNT",
                    "GIT_REPLACE_REF_BASE",
                    "GIT_TRACE",
                ]
                .into_iter()
                .map(OsString::from)
                .collect(),
            },
            timestamp: 1793142692,
            major: 11,
            minor: 91,
            patch: 20753,
            version: "11.91.20753".to_owned(),
        }
    }

    #[test]
    fn git_context_is_shared_and_disables_replacement_and_lazy_fetch() {
        let source = fixture_source();
        let custody = source
            .git
            .command(Path::new("/fixture/source"), GitRead::Timestamp);
        let mut configure = Command::new("cmake");
        source.git.configure(&mut configure);
        let mut build = Command::new("cmake");
        source.git.apply_environment(&mut build);
        assert_eq!(custody.get_program(), source.git.executable.as_os_str());
        assert!(custody.get_args().any(|arg| arg == "--no-lazy-fetch"));
        assert!(custody.get_args().any(|arg| arg == "--no-replace-objects"));
        assert!(
            configure
                .get_args()
                .any(|arg| arg == "-DGIT:FILEPATH=/fixture/git")
        );
        for command in [&custody, &configure, &build] {
            let environment = command
                .get_envs()
                .map(|(key, value)| (key.to_os_string(), value.map(|value| value.to_os_string())))
                .collect::<BTreeMap<_, _>>();
            assert_eq!(
                environment.get(&OsString::from("GIT_CEILING_DIRECTORIES")),
                Some(&Some(OsString::from("/fixture")))
            );
            for &(key, value) in GIT_ENVIRONMENT {
                assert_eq!(
                    environment.get(&OsString::from(key)),
                    Some(&Some(OsString::from(value)))
                );
            }
            for key in [
                "GIT_DIR",
                "GIT_WORK_TREE",
                "GIT_COMMON_DIR",
                "GIT_INDEX_FILE",
                "GIT_REPLACE_REF_BASE",
                "GIT_TRACE",
            ] {
                assert_eq!(environment.get(&OsString::from(key)), Some(&None));
            }
        }
    }

    #[test]
    fn generated_version_contract_refuses_fallback_and_mixed_patchlevels() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("source");
        let build = directory.path().join("build");
        fs::create_dir_all(root.join("make")).unwrap();
        fs::create_dir_all(build.join("cmake")).unwrap();
        fs::write(root.join("make/DynamoRIOConfigVersion.cmake.in"), "set(PACKAGE_VERSION @VERSION_NUMBER@)\n# @VERSION_NUMBER_MAJOR@ @VERSION_NUMBER_MINOR@ @VERSION_NUMBER_PATCHLEVEL@\n").unwrap();
        let source = fixture_source();
        let version = source.expected_config_version(&root).unwrap();
        fs::write(build.join("cmake/DynamoRIOConfigVersion.cmake"), &version).unwrap();
        let header = "#define VERSION_NUMBER 11.91.20753\n#define VERSION_COMMA_DELIMITED 11,91,20753\n#define VERSION_NUMBER_INTEGER 1191\n";
        fs::write(build.join("configure.h"), header).unwrap();
        let cache = "GIT:FILEPATH=/fixture/git\nVERSION_NUMBER:STRING=\n";
        fs::write(build.join("CMakeCache.txt"), cache).unwrap();
        let package = "set(CPACK_PACKAGE_VERSION \"11.91.20753\")\nset(CPACK_PACKAGE_VERSION_MAJOR \"11\")\nset(CPACK_PACKAGE_VERSION_MINOR \"91\")\nset(CPACK_PACKAGE_VERSION_PATCH \"20753\")\n";
        for name in ["CPackConfig.cmake", "CPackSourceConfig.cmake"] {
            fs::write(build.join(name), package).unwrap();
        }
        source.verify_version_files(&root, &build).unwrap();
        for name in [
            "cmake/DynamoRIOConfigVersion.cmake",
            "configure.h",
            "CPackConfig.cmake",
            "CPackSourceConfig.cmake",
        ] {
            let path = build.join(name);
            let original = fs::read_to_string(&path).unwrap();
            fs::write(&path, original.replace("20753", "20754")).unwrap();
            assert!(
                source.verify_version_files(&root, &build).is_err(),
                "{name}"
            );
            fs::write(path, original).unwrap();
        }
        fs::write(
            build.join("CMakeCache.txt"),
            cache.replace("/fixture/git", "/other/git"),
        )
        .unwrap();
        assert!(source.verify_version_files(&root, &build).is_err());
        fs::write(build.join("CMakeCache.txt"), cache).unwrap();
        fs::write(
            build.join("configure.h"),
            format!("{header}#define VERSION_NUMBER 11.91.20754\n"),
        )
        .unwrap();
        assert!(source.verify_version_files(&root, &build).is_err());
        assert!(require_unique_line("", "GIT:FILEPATH=/fixture/git").is_err());
    }
}
