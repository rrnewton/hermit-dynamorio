// Maintained DynamoRIO physical-command controls transferred from core Hbc5.
// Pure command/file fixtures: these tests do not launch Hermit or a guest.
use super::*;

fn write_executable(path: &Path, contents: &[u8]) {
    fs::write(path, contents).unwrap();
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}

#[test]
fn each_invocation_names_its_own_dynamorio_config_dir_of_one_length() {
    let first = super::dynamorio_config_dir();
    let second = super::dynamorio_config_dir();
    for dir in [&first, &second] {
        let name = dir.to_str().unwrap();
        let suffix = name.strip_prefix("/tmp/.hermit-dynamorio-").unwrap();
        assert_eq!(suffix.len(), 16, "{name}");
        assert!(
            suffix.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "{name}"
        );
    }
    assert_ne!(first, second);
}

#[test]
fn a_dynamorio_config_dir_is_private_and_removed_with_its_files() {
    use std::os::unix::fs::PermissionsExt;
    let parent = tempfile::tempdir().unwrap();
    let path = parent.path().join("config");
    let dir = super::DynamoRioConfigDir::create(&path).unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o700
    );
    // A second run cannot take over a directory that already exists.
    assert!(super::DynamoRioConfigDir::create(&path).is_err());
    std::fs::create_dir(path.join(".dynamorio")).unwrap();
    std::fs::write(path.join(".dynamorio/true.2.1config64"), b"-client").unwrap();
    drop(dir);
    assert!(!path.exists());
}

#[test]
fn dynamorio_workdir_reaches_the_dynamorio_guest_command() {
    let mut command = std::process::Command::new("/bin/true");
    super::apply_dynamorio_workdir(&mut command, Some(std::path::Path::new("/test")));
    assert_eq!(
        command.get_current_dir(),
        Some(std::path::Path::new("/test"))
    );
}

#[test]
fn dynamorio_physical_command_preserves_explicit_environment_removals() {
    let inherited = env::vars_os()
        .map(|(key, _)| key)
        .find(|key| key != "DBT_PHYSICAL_KEPT")
        .expect("native fixture requires an inherited environment entry");
    let environment = BTreeMap::from([(
        OsString::from("DBT_PHYSICAL_KEPT"),
        OsString::from("literal guest value"),
    )]);
    let mut command = StdCommand::new("/usr/bin/env");
    apply_exact_environment(&mut command, &environment);
    command.env_remove("DBT_PHYSICAL_EXPLICIT_REMOVAL");
    let original = command
        .get_envs()
        .map(|(key, value)| (key.to_owned(), value.map(OsStr::to_owned)))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(original.get(&inherited), Some(&None));
    assert_eq!(
        original.get(OsStr::new("DBT_PHYSICAL_EXPLICIT_REMOVAL")),
        Some(&None)
    );
    let prepared = prepare_dynamorio_physical_command(&command).unwrap();
    let transported = prepared
        .get_envs()
        .map(|(key, value)| (key.to_owned(), value.map(OsStr::to_owned)))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        transported, original,
        "physical command preparation must preserve each removal for DbtRunner"
    );
    assert_eq!(prepared.get_program(), command.get_program());
}

#[test]
fn dynamorio_resolves_simple_env_shebang_target_to_absolute_path() {
    let root = tempfile::tempdir().unwrap();
    let bin = root.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let python = bin.join("python3");
    write_executable(&python, b"\x7fELFplaceholder");
    let script = root.path().join("guest.py");
    write_executable(&script, b"#!/usr/bin/env python3\n");

    let prepared = prepare_dynamorio_guest_command(
        &script,
        &[OsString::from("argument")],
        Some(bin.as_os_str()),
        root.path(),
    );

    assert_eq!(prepared.program, Path::new("/usr/bin/env"));
    assert_eq!(
        prepared.args,
        [
            python.into_os_string(),
            script.into_os_string(),
            OsString::from("argument"),
        ]
    );
}

#[test]
fn dynamorio_leaves_complex_env_shebang_for_launcher() {
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("guest.py");
    write_executable(&script, b"#!/usr/bin/env -S python3 -u\n");

    let prepared =
        prepare_dynamorio_guest_command(&script, &[], Some(OsStr::new("/usr/bin")), root.path());

    assert_eq!(prepared.program, script);
    assert!(prepared.args.is_empty());
}

#[test]
fn dynamorio_relative_shebang_uses_guest_cwd_and_preserves_script_arguments() {
    let root = tempfile::Builder::new()
        .prefix("dbt-workdir-")
        .tempdir_in(".")
        .unwrap();
    let relative = PathBuf::from(root.path().file_name().unwrap()).join("guest.py");
    let guest_cwd = fs::canonicalize(root.path()).unwrap().join("guest-cwd");
    let guest_script = guest_cwd.join(&relative);
    fs::create_dir_all(guest_script.parent().unwrap()).unwrap();
    write_executable(&relative, b"#!/usr/bin/env caller-only\n");
    write_executable(&guest_script, b"#!/usr/bin/env guest-only\n");
    let relative_bin = PathBuf::from(root.path().file_name().unwrap()).join("bin");
    let guest_bin = guest_cwd.join(&relative_bin);
    fs::create_dir_all(&relative_bin).unwrap();
    fs::create_dir_all(&guest_bin).unwrap();
    write_executable(&relative_bin.join("caller-only"), b"\x7fELFcaller");
    write_executable(&guest_bin.join("guest-only"), b"\x7fELFguest");
    let mut command = StdCommand::new(&relative);
    command
        .current_dir(&guest_cwd)
        .env_clear()
        .env("PATH", &relative_bin)
        .env("KEPT", "literal value")
        .env_remove("EXPLICIT_REMOVAL")
        .args(["literal argument", ""]);
    let prepared = prepare_dynamorio_physical_command(&command).unwrap();
    assert_eq!(prepared.get_program(), OsStr::new("/usr/bin/env"));
    assert_eq!(
        prepared.get_args().map(OsStr::to_owned).collect::<Vec<_>>(),
        [
            guest_bin.join("guest-only").into_os_string(),
            relative.into_os_string(),
            OsString::from("literal argument"),
            OsString::new()
        ]
    );
    assert_eq!(prepared.get_current_dir(), Some(guest_cwd.as_path()));
    assert_eq!(
        prepared.get_envs().collect::<Vec<_>>(),
        command.get_envs().collect::<Vec<_>>()
    );
}

#[test]
fn dynamorio_path_selected_script_keeps_the_selected_script_path() {
    let root = tempfile::tempdir().unwrap();
    let bin = root.path().join("bin");
    fs::create_dir(&bin).unwrap();
    write_executable(
        &bin.join("guest.py"),
        b"#!/usr/bin/env chosen-interpreter\n",
    );
    let interpreter = bin.join("chosen-interpreter");
    write_executable(&interpreter, b"\x7fELFguest");
    for (cwd, path, script, target) in [
        (
            root.path(),
            OsStr::new("bin"),
            PathBuf::from("bin/guest.py"),
            interpreter.clone(),
        ),
        (
            root.path(),
            bin.as_os_str(),
            bin.join("guest.py"),
            interpreter.clone(),
        ),
        (
            root.path(),
            OsStr::new(":bin"),
            PathBuf::from("bin/guest.py"),
            interpreter.clone(),
        ),
        (
            bin.as_path(),
            OsStr::new(""),
            PathBuf::from("guest.py"),
            interpreter.clone(),
        ),
        (
            bin.as_path(),
            OsStr::new("."),
            PathBuf::from("./guest.py"),
            bin.join("./chosen-interpreter"),
        ),
    ] {
        let mut command = StdCommand::new("guest.py");
        command
            .current_dir(cwd)
            .env_clear()
            .env("PATH", path)
            .args(["", "two words"]);
        let prepared = prepare_dynamorio_physical_command(&command).unwrap();
        assert_eq!(prepared.get_program(), OsStr::new("/usr/bin/env"));
        assert_eq!(
            prepared.get_args().map(OsStr::to_owned).collect::<Vec<_>>(),
            [
                target.into_os_string(),
                script.into_os_string(),
                OsString::new(),
                OsString::from("two words"),
            ],
            "PATH {path:?} in {cwd:?} must identify the selected script"
        );
        assert_eq!(prepared.get_current_dir(), command.get_current_dir());
        assert_eq!(
            prepared.get_envs().collect::<Vec<_>>(),
            command.get_envs().collect::<Vec<_>>()
        );
    }
}

#[test]
fn dynamorio_empty_path_component_and_hidden_script_use_physical_cwd() {
    let root = tempfile::Builder::new()
        .prefix("dbt-physical-cwd-")
        .tempdir_in(".")
        .unwrap();
    let relative = PathBuf::from(root.path().file_name().unwrap()).join("guest.py");
    let guest_cwd = fs::canonicalize(root.path()).unwrap().join("empty-workdir");
    fs::create_dir(&guest_cwd).unwrap();
    write_executable(&relative, b"#!/usr/bin/env chosen-interpreter\n");
    let caller_bin = root.path().join("bin");
    fs::create_dir(&caller_bin).unwrap();
    write_executable(&caller_bin.join("chosen-interpreter"), b"\x7fELFcaller");
    let mut command = StdCommand::new(&relative);
    command
        .current_dir(&guest_cwd)
        .env_clear()
        .env("PATH", &caller_bin)
        .arg("argument");
    // The caller can see this relative script; the physical guest cannot.
    let absent = prepare_dynamorio_physical_command(&command).unwrap();
    assert_eq!(absent.get_program(), relative.as_os_str());
    assert_eq!(
        absent.get_args().collect::<Vec<_>>(),
        [OsStr::new("argument")]
    );
    // An empty PATH component denotes the physical guest's cwd.
    let script = guest_cwd.join("script");
    write_executable(&script, b"#!/usr/bin/env chosen-interpreter\n");
    let interpreter = guest_cwd.join("chosen-interpreter");
    write_executable(&interpreter, b"\x7fELFguest");
    let mut command = StdCommand::new(&script);
    command.current_dir(&guest_cwd).env_clear().env("PATH", "");
    let prepared = prepare_dynamorio_physical_command(&command).unwrap();
    assert_eq!(prepared.get_program(), OsStr::new("/usr/bin/env"));
    assert_eq!(
        prepared.get_args().collect::<Vec<_>>(),
        [interpreter.as_os_str(), script.as_os_str()]
    );
}

#[tokio::test]
async fn dynamorio_abnormal_cleanup_ends_an_unregistered_scheduler() {
    use std::os::unix::process::ExitStatusExt;

    use reverie::GlobalTool;

    let config = detcore::Config {
        sequentialize_threads: true,
        ..detcore::Config::default()
    };
    let mut outcomes = Vec::new();
    for (name, raw_status) in [("exit255", 255 << 8), ("SIGKILL", libc::SIGKILL)] {
        let global = detcore::GlobalState::init_global_state(&config).await;
        let status = std::process::ExitStatus::from_raw(raw_status);
        let completed = matches!(
            tokio::time::timeout(
                std::time::Duration::from_millis(100),
                super::clean_up_dynamorio_global(&status, global, None),
            )
            .await,
            Ok(Ok(()))
        );
        assert_eq!(status.into_raw(), raw_status);
        outcomes.push((name, completed));
    }
    assert_eq!(
        outcomes,
        [("exit255", true), ("SIGKILL", true)],
        "DBT cleanup waited for a scheduler whose physical guest had already failed"
    );
}

#[tokio::test]
async fn dynamorio_successful_cleanup_preserves_normal_scheduler_completion() {
    use std::os::unix::process::ExitStatusExt;

    use reverie::GlobalTool;

    let status = std::process::ExitStatus::from_raw(0);
    let config = detcore::Config {
        sequentialize_threads: false,
        ..detcore::Config::default()
    };
    let global = detcore::GlobalState::init_global_state(&config).await;
    assert!(
        matches!(
            tokio::time::timeout(
                std::time::Duration::from_millis(100),
                super::clean_up_dynamorio_global(&status, global, None),
            )
            .await,
            Ok(Ok(()))
        ),
        "cleanup without an owned scheduler did not complete"
    );
    let config = detcore::Config {
        sequentialize_threads: true,
        ..detcore::Config::default()
    };
    let global = detcore::GlobalState::init_global_state(&config).await;
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            super::clean_up_dynamorio_global(&status, global, None),
        )
        .await
        .is_err(),
        "successful DBT cleanup silently cancelled a scheduler that had not completed"
    );
    assert!(status.success());
}

#[test]
fn a_spawn_failure_still_names_the_launch() {
    let drrun = Path::new("/nonexistent/dynamorio/bin64/drrun");
    for kind in [
        std::io::ErrorKind::NotFound,
        std::io::ErrorKind::PermissionDenied,
    ] {
        let rendered = dynamorio_run_error(drrun, std::io::Error::new(kind, "boom")).to_string();
        assert!(
            rendered.contains("failed to launch drrun"),
            "a {kind:?} spawn failure must be reported as a launch failure: {rendered}"
        );
    }
}

#[test]
fn a_post_launch_failure_does_not_blame_the_binary() {
    let drrun = Path::new("/real/dynamorio/bin64/drrun");
    let inner = "DBT guest exited with status Some(1) while protected evidence failed: \
                     DBT evidence collector thread panicked";
    let rendered = dynamorio_run_error(drrun, std::io::Error::other(inner)).to_string();
    assert!(
        !rendered.contains("failed to launch drrun"),
        "a post-launch failure must not be reported as a launch failure: {rendered}"
    );
    assert!(
        rendered.contains("drrun started"),
        "the message must say the launch succeeded: {rendered}"
    );
    assert!(
        rendered.contains("NOT a launch failure"),
        "the message must rule the binary out explicitly: {rendered}"
    );
    // The cause must survive verbatim. Truncating it here is what cost the
    // original investigation its answer.
    assert!(
        rendered.contains("protected evidence failed"),
        "the underlying error must be preserved in full: {rendered}"
    );
}

// Original 207/208 native runner half. Core owns actual admitted declaration
// collision choice; these call the SAME production reset/configure operations.
#[test]
fn dynamorio_stats_mapping_replaces_each_run_at_the_selected_target() {
    let statistics = std::cell::RefCell::new(None);
    let runner = DynamoRioRunner::new("/bin/true", "/bin/true").unwrap();
    let first = dynamorio_stats_capture(&statistics).unwrap();
    let target = Path::new("/tmp/.hermit-dynamorio-stats-2");
    let configured = configure_dynamorio_stats_at(
        &statistics,
        &first,
        runner.clone(),
        Some(target.to_path_buf()),
    )
    .unwrap();
    assert_eq!(
        *statistics.borrow(),
        Some(Bind::new(first.directory(), target))
    );
    assert_eq!(
        format!("{configured:?}"),
        format!("{:?}", first.configure_at(runner.clone(), target))
    );
    // The prior sink can still exist; replacement uses the selected stable
    // guest target, never which backing directories remain alive.
    let second = dynamorio_stats_capture(&statistics).unwrap();
    assert!(statistics.borrow().is_none());
    assert_ne!(first.directory(), second.directory());
    configure_dynamorio_stats_at(&statistics, &second, runner, Some(target.to_path_buf())).unwrap();
    assert_eq!(
        *statistics.borrow(),
        Some(Bind::new(second.directory(), target))
    );
}

#[test]
fn dynamorio_stats_without_namespace_keep_the_host_address_and_no_bind() {
    let statistics = std::cell::RefCell::new(None);
    let capture = dynamorio_stats_capture(&statistics).unwrap();
    let runner = DynamoRioRunner::new("/bin/true", "/bin/true").unwrap();
    let expected = capture.configure(runner.clone());
    let actual = configure_dynamorio_stats_at(&statistics, &capture, runner, None).unwrap();
    assert_eq!(format!("{actual:?}"), format!("{expected:?}"));
    assert!(statistics.borrow().is_none());
}
