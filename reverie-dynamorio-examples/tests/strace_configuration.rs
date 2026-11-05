/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

#[cfg(test)]
mod typed_configuration_tests {
    use std::cell::Cell;
    use std::io::Read;
    use std::io::Write;
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixListener;
    use std::os::unix::net::UnixStream;
    use std::process::Command;
    use std::process::Stdio;
    use std::sync::Mutex;
    use std::time::Duration;
    use std::time::Instant;

    use reverie::GlobalRPC;
    use reverie::GlobalTool;
    use reverie::Guest;
    use reverie::InjectionPurpose;
    use reverie::Pid;
    use reverie::Tool;
    use reverie::ToolConfiguration;
    use reverie::syscalls::Displayable;
    use reverie::syscalls::Syscall;
    use reverie::syscalls::SyscallArgs;
    use reverie::syscalls::Sysno;
    use reverie_dynamorio::DynamoRioGuest;
    use reverie_dynamorio::DynamoRioSyscallOutcome;
    use reverie_dynamorio::read_tool_configuration;
    use reverie_dynamorio::run_tool_syscall_with_guest;
    use reverie_rpc_transport::codec::encode;

    use clap::Parser;
    use reverie_examples::tools::strace as global_state;
    use reverie_examples::tools::strace as tool;
    use reverie_examples::tools::strace::{Config, Filter, Opts, Strace};

    struct Endpoint<'a> {
        configuration: &'a ToolConfiguration<Strace>,
        expected: Mutex<Option<global_state::OutputEvent>>,
        events: Mutex<Vec<global_state::OutputEvent>>,
    }
    #[reverie::tool]
    impl GlobalRPC<global_state::GlobalState> for Endpoint<'_> {
        async fn send_rpc(&self, event: global_state::OutputEvent) -> Result<(), String> {
            assert_eq!(
                Some(&event),
                self.expected.lock().unwrap().as_ref(),
                "only the exact post-construction write diagnostic is expected"
            );
            self.events.lock().unwrap().push(event.clone());
            global_state::GlobalState
                .receive_rpc(Pid::from_raw(unsafe { libc::getpid() }), event)
                .await
        }
        fn config(&self) -> &Config {
            self.configuration.config()
        }
    }

    unsafe extern "C" fn invoke(
        context: usize,
        number: i64,
        args: *const u64,
        purpose: u32,
    ) -> i64 {
        let calls = unsafe { &*(context as *const Cell<usize>) };
        assert_eq!(number, libc::SYS_write);
        assert_eq!(purpose, InjectionPurpose::Application as u32);
        let args = unsafe { &*args.cast::<[u64; 6]>() };
        assert_eq!((args[0], args[2]), (1, 6));
        assert_eq!(
            unsafe { std::slice::from_raw_parts(args[1] as *const u8, 6) },
            b"hello\n"
        );
        calls.set(calls.get() + 1);
        assert_eq!(calls.get(), 1, "duplicate returning injection");
        unsafe { libc::syscall(number, args[0], args[1], args[2]) }
    }
    unsafe extern "C" fn read_regs(_: usize, _: *mut libc::user_regs_struct) -> i32 {
        panic!("write handler does not read registers")
    }
    unsafe extern "C" fn write_regs(_: usize, _: *const libc::user_regs_struct) -> i32 {
        panic!("write handler does not replace registers")
    }

    #[test]
    fn retained_configuration_drives_the_real_strace_write_handler() {
        const CHILD: &str = "REVERIE_STRACE_CONFIGURATION_CHILD";
        // This file is also included by the examples library. Preserve the
        // actual libtest identity in either compilation, not a guessed prefix.
        let test = concat!(
            module_path!(),
            "::retained_configuration_drives_the_real_strace_write_handler"
        )
        .split_once("::")
        .unwrap()
        .1;
        if std::env::var_os(CHILD).is_none() {
            // Keep libtest's own output separate from the application's exact
            // stdout bytes. The child redirects this inherited stream only
            // once it enters the test body, before the real write syscall.
            let (mut application_stdout, child_stream) = UnixStream::pair().unwrap();
            application_stdout
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", test, "--nocapture", "--test-threads=1"])
                .env(CHILD, "1")
                .stdin(Stdio::from(OwnedFd::from(child_stream)))
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if child.try_wait().unwrap().is_some() {
                    break;
                }
                if Instant::now() >= deadline {
                    child.kill().unwrap();
                    child.wait().unwrap();
                    panic!("Strace library control exceeded ten seconds");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            let output = child.wait_with_output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let mut stdout = Vec::new();
            application_stdout.read_to_end(&mut stdout).unwrap();
            assert_eq!(stdout, b"hello\n");
            let stderr = String::from_utf8(output.stderr).unwrap();
            assert!(stderr.contains("write(1,"), "{stderr}");
            assert!(stderr.contains(" = 6"), "{stderr}");
            return;
        }

        assert_eq!(unsafe { libc::dup2(0, 1) }, 1);
        let options =
            Opts::try_parse_from(["strace", "--trace", "write", "--", "/bin/echo", "hello"])
                .unwrap();
        let config = Config {
            filters: options.trace,
        };
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("configuration.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let payload = encode(&config).unwrap();
            stream
                .write_all(&(payload.len() as u32).to_be_bytes())
                .unwrap();
            stream.write_all(&payload).unwrap();
            assert_eq!(
                stream.read(&mut [0_u8; 1]).unwrap(),
                0,
                "bootstrap sent an application request"
            );
        });
        let owner = read_tool_configuration::<Strace>(&path).unwrap();
        server.join().unwrap();
        assert_eq!(owner.config().filters, ["write".parse::<Filter>().unwrap()]);
        assert_eq!(
            owner.subscriptions().iter_syscalls().collect::<Vec<_>>(),
            [Sysno::write]
        );
        assert!(
            !owner
                .subscriptions()
                .iter_syscalls()
                .any(|s| s == Sysno::read)
        );
        let endpoint = Endpoint {
            configuration: &owner,
            expected: Mutex::new(None),
            events: Mutex::new(Vec::new()),
        };
        let pid = Pid::from_raw(unsafe { libc::getpid() });
        let tid = Pid::from_raw(unsafe { libc::syscall(libc::SYS_gettid) } as i32);
        let tool = owner.new_process(pid);
        tool.init_thread_state(tid, None);
        let state = &mut ();
        let calls = Cell::new(0_usize);
        let guest = DynamoRioGuest::<Strace>::new_with_rpc(
            &calls as *const Cell<usize> as usize,
            tid,
            pid,
            None,
            0,
            state,
            &endpoint,
            invoke,
            read_regs,
            write_regs,
        );
        assert!(std::ptr::eq(guest.config(), owner.config()));
        let message = b"hello\n";
        let syscall = Syscall::from_raw(
            Sysno::write,
            SyscallArgs::new(1, message.as_ptr() as usize, message.len(), 0, 0, 0),
        );
        let expected = global_state::OutputEvent::Syscall(format!(
            "[pid {}] {} = 6",
            tid.colored(),
            syscall.display_with_outputs(&guest.memory())
        ));
        *endpoint.expected.lock().unwrap() = Some(expected.clone());
        assert!(matches!(
            run_tool_syscall_with_guest(&tool, guest, syscall).unwrap(),
            DynamoRioSyscallOutcome::Suppress(6)
        ));
        assert_eq!(calls.get(), 1);
        assert_eq!(tool::take_handled_syscalls(), 1);
        assert_eq!(*endpoint.events.lock().unwrap(), [expected]);
        // Avoid writing libtest's completion line onto application stdout.
        std::process::exit(0);
    }
}
