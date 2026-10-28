#[allow(dead_code)]
#[path = "../build.rs"]
mod build_script;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::path::PathBuf;

use sha2::Digest;
use sha2::Sha256;

fn recursive_source() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../third-party/dynamorio");
    // Exercise the production custody path, not an independent marker-only
    // approximation. Archives, unknown/dirty/missing children, extra files,
    // executable-mode changes and symlink-target changes must all refuse.
    build_script::source_checkout::verify_and_hash(&root)
        .expect("the complete pinned recursive Git source is required");
    root
}

#[test]
fn recursive_dynamorio_source_is_complete() {
    let source = recursive_source();
    assert_eq!(
        std::fs::read_to_string(source.join("REVISION"))
            .unwrap()
            .trim(),
        "929840ad9190e5086775e8debc0f0b79b4208d59"
    );
    for required in [
        "CMakeLists.txt",
        "core/lib/globals_shared.h",
        "core/unix/memcache.c",
        "tools/drdeploy.c",
        "ext/drmgr/drmgr.c",
        "ext/drreg/drreg.c",
        "ext/drwrap/drwrap.c",
        "ext/drx/drx.c",
    ] {
        assert!(source.join(required).is_file(), "missing {required}");
    }
}

#[test]
fn build_script_never_populates_source_over_the_network() {
    let build_script = include_str!("../build.rs");
    let custody = include_str!("../source_checkout.rs");
    let production_custody = custody.split("#[cfg(test)]").next().unwrap();
    for forbidden in [
        "Command::new(\"git\")",
        "git submodule",
        "DYNAMORIO_URL",
        "clone_dynamorio",
        "populate_dynamorio",
    ] {
        assert!(
            !build_script.contains(forbidden) && !custody.contains(forbidden),
            "build or custody source contains forbidden source-fetch path {forbidden:?}"
        );
    }
    // Git is now an explicit read-only build input. These are the only ten
    // operations; no branch, tag, network, checkout or population operation is
    // allowed. The one executable is also supplied to CMake under the same
    // controlled environment, with automatic lazy fetch disabled.
    for required in [
        "enum GitRead {",
        "Self::TopLevel => &[\"rev-parse\", \"--show-toplevel\"]",
        "Self::GitDir => &[\"rev-parse\", \"--absolute-git-dir\"]",
        "Self::Head => &[\"rev-parse\", \"--verify\", \"HEAD\"]",
        "Self::Tree => &[\"rev-parse\", \"--verify\", \"HEAD^{tree}\"]",
        "Self::Entries => &[\"ls-tree\", \"-rz\", \"--full-tree\", \"HEAD\"]",
        "Self::Index => &[\"ls-files\", \"--stage\", \"-z\"]",
        "Self::Dirty => &[\"diff\", \"--no-ext-diff\", \"--quiet\", \"--ignore-submodules=none\", \"HEAD\", \"--\"]",
        "Self::Blobs => &[\"cat-file\", \"--batch\"]",
        "Self::Commit => &[\"cat-file\", \"commit\", PINS[0].commit]",
        "Self::Timestamp => &[\"log\", \"-n\", \"1\", \"--format=%ct\"]",
        "Command::new(&self.executable)",
        "for key in &self.inherited_git_variables { command.env_remove(key); }",
        "command.envs(GIT_ENVIRONMENT.iter().copied())",
        "\"--no-replace-objects\", \"--no-lazy-fetch\", \"--no-optional-locks\"",
        "(\"GIT_NO_LAZY_FETCH\", \"1\")",
        "-DGIT:FILEPATH=",
    ] {
        assert!(
            custody.contains(required),
            "missing read-only Git contract {required:?}"
        );
    }
    assert_eq!(production_custody.matches("Command::new(").count(), 1);
    assert!(build_script.contains("verified_source.git.configure(&mut configure)"));
}

#[test]
fn build_cache_is_content_addressed_and_visible() {
    let build_script = include_str!("../build.rs");
    for required in [
        "let source_key = source_recipe_key(",
        "let cache_root = cache_root_for_out_dir(&out_dir)",
        "DynamoRIO shared cache disabled for unrecognized OUT_DIR",
        "out_dir.join(\"reverie-dynamorio-native-cache\")",
        "dynamorio-install-{source_key}",
        "fs::rename(staged_install, install_dir)",
        "CMAKE_GENERATOR",
        "DynamoRIO build cache HIT key=sha256:",
        "DynamoRIO build cache MISS key=sha256:",
    ] {
        assert!(
            build_script.contains(required),
            "build script lacks cache invariant {required:?}"
        );
    }
}

#[test]
fn ci_uses_the_recursive_content_addressed_cache() {
    let workflow = include_str!("../../.github/workflows/ci.yml");
    assert!(workflow.contains("reverie-dynamorio-native-cache/dynamorio-install-*"));
    assert!(
        workflow.contains("hashFiles('third-party/dynamorio/**', 'reverie-dynamorio/build.rs', 'reverie-dynamorio/source_checkout.rs')")
    );
    assert!(workflow.contains("submodules: recursive"));
    for obsolete in [
        "scripts/backend-submodule.sh activate dynamorio",
        "out/dynamorio-revision",
        "steps.dynamorio.outputs.rev",
    ] {
        assert!(
            !workflow.contains(obsolete),
            "CI still contains obsolete cache input {obsolete:?}"
        );
    }
}

/// Walk every regular source file after production custody has validated all
/// bytes, modes, symlinks, Git identities and the physical path universe.
fn recursive_files(root: &Path) -> Vec<PathBuf> {
    const ADMINISTRATION: [&str; 4] = [
        ".git",
        "third_party/libipt/.git",
        "third_party/zlib/.git",
        "third_party/elfutils/.git",
    ];
    fn walk(root: &Path, dir: &Path, found: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("failed to walk the recursive source") {
            let entry = entry.expect("failed to read a recursive directory entry");
            let path = entry.path();
            let relative = path.strip_prefix(root).unwrap();
            if ADMINISTRATION
                .iter()
                .any(|admin| relative == Path::new(admin))
            {
                continue;
            }
            let kind = entry.file_type().expect("failed to stat a recursive path");
            if kind.is_dir() {
                walk(root, &path, found);
            } else if kind.is_file() {
                found.push(path);
            }
        }
    }
    let mut found = Vec::new();
    walk(root, root, &mut found);
    assert!(
        found.len() > 100,
        "the recursive DynamoRIO walk found only {} files",
        found.len()
    );
    assert_eq!(
        found.len(),
        5093,
        "the pinned recursive regular-file population changed"
    );
    found.sort();
    found
}

fn expected_class(class: &str) -> String {
    include_str!("recursive_source_classes.tsv")
        .lines()
        .filter(|line| line.starts_with(&format!("{class}\t")))
        .map(|line| format!("{line}\n"))
        .collect()
}

fn actual_class(root: &Path, class: &str, selected: impl Fn(&[u8]) -> bool) -> String {
    let mut actual = String::new();
    for path in recursive_files(root) {
        let bytes = std::fs::read(&path).expect("failed to read a recursive DynamoRIO file");
        if selected(&bytes) {
            let relative = path.strip_prefix(root).unwrap().to_str().unwrap();
            let executable = path.metadata().unwrap().permissions().mode() & 0o111 != 0;
            let mode = if executable { "100755" } else { "100644" };
            actual.push_str(&format!(
                "{class}\t{mode}\t{}\t{:x}\t{relative}\n",
                bytes.len(),
                Sha256::digest(&bytes)
            ));
        }
    }
    actual
}

/// The full upstream tree retains its images and binary test inputs. Compare
/// the exact 503 NUL-containing files (paths, modes, sizes and bytes), instead
/// of presenting the old curated-tree no-binary assertion as a pass.
#[test]
fn recursive_dynamorio_binary_files_match_the_pinned_source_inventory() {
    let root = recursive_source();
    let expected = expected_class("binary");
    assert_eq!(expected.lines().count(), 503);
    assert_eq!(
        actual_class(&root, "binary", |bytes| bytes.contains(&0)),
        expected
    );
}

/// Keep the 2 MiB classifier, with an exact inventory of the ten upstream
/// files now present in the complete tree. Unknown or changed entries refuse;
/// there is no size tolerance, suffix exemption or general ignore pattern.
#[test]
fn recursive_dynamorio_large_files_match_the_pinned_source_inventory() {
    const LARGE_TEXT_LIMIT: usize = 2 * 1024 * 1024;
    let root = recursive_source();
    let expected = expected_class("large");
    assert_eq!(expected.lines().count(), 10);
    assert_eq!(
        actual_class(&root, "large", |bytes| bytes.len() > LARGE_TEXT_LIMIT),
        expected
    );
}
