//! Feature-selected observation of the actual runner exec, never a replacement.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;

const INPUT: &str = "DYNAMORIO_TEST_LAUNCH_OBSERVATION";
const LIMIT: usize = 1_048_576;

fn hex(record: &mut Vec<u8>, kind: u8, argument: &[u8]) {
    const DIGITS: &[u8] = b"0123456789abcdef";
    record.push(kind);
    for byte in argument {
        record.push(DIGITS[(byte >> 4) as usize]);
        record.push(DIGITS[(byte & 15) as usize]);
    }
    record.push(b'\n');
}

/// Install only after the real command and all its arguments are complete.
pub(super) fn attach(command: &mut Command) -> io::Result<()> {
    let selection = command
        .get_envs()
        .find(|(key, _)| *key == INPUT)
        .map_or_else(
            || std::env::var_os(INPUT),
            |(_, value)| value.map(ToOwned::to_owned),
        );
    command.env_remove(INPUT);
    let Some(path) = selection else {
        return Ok(());
    };
    let path = PathBuf::from(path);
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "absolute launch observation required",
        ));
    }
    let mut arguments = vec![command.get_program().as_bytes().to_vec()];
    arguments.extend(command.get_args().map(|value| value.as_bytes().to_vec()));
    let client = arguments
        .iter()
        .position(|value| value == b"-c")
        .ok_or_else(|| io::Error::other("launch observation has no client boundary"))?;
    let guest = arguments
        .iter()
        .enumerate()
        .skip(client + 2)
        .find(|(_, value)| value.as_slice() == b"--")
        .map(|(index, _)| index)
        .ok_or_else(|| io::Error::other("launch observation has no guest boundary"))?;
    let mut record = b"BEGIN\n".to_vec();
    hex(&mut record, b'I', &arguments[0]);
    let mut index = 0;
    while index < arguments.len() {
        hex(&mut record, b'A', &arguments[index]);
        if index >= client + 2
            && index < guest
            && (arguments[index] == b"-evidence-token" || arguments[index] == b"-evidence-socket")
        {
            let token = arguments[index] == b"-evidence-token";
            index += 1;
            let value = arguments
                .get(index)
                .filter(|_| index < guest)
                .ok_or_else(|| io::Error::other("launch observation has no protected value"))?;
            if value.len() != if token { 64 } else { 32 }
                || !value
                    .iter()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
            {
                return Err(io::Error::other(
                    "launch observation has invalid protected value",
                ));
            }
            record.extend_from_slice(if token { b"TOKEN64\n" } else { b"SOCKET32\n" });
        }
        index += 1;
    }
    record.extend_from_slice(b"END\n");
    if record.len() > LIMIT {
        return Err(io::Error::other("launch observation exceeds its bound"));
    }
    let source = std::fs::OpenOptions::new()
        .append(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)?;
    if !source.metadata()?.is_file() {
        return Err(io::Error::other("launch observation is not a regular file"));
    }
    let fd = unsafe { libc::fcntl(source.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 256) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let held = unsafe { std::fs::File::from_raw_fd(fd) };
    // Encoding and allocation happened before fork. This pre-exec callback uses
    // only raw flock/write syscalls; CLOEXEC closes the held FD before drrun.
    unsafe {
        command.pre_exec(move || {
            let fd = held.as_raw_fd();
            while libc::flock(fd, libc::LOCK_EX) != 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::EINTR) {
                    return Err(error);
                }
            }
            let result = (|| {
                let mut offset = 0;
                while offset < record.len() {
                    let count =
                        libc::write(fd, record[offset..].as_ptr().cast(), record.len() - offset);
                    if count < 0 {
                        let error = io::Error::last_os_error();
                        if error.raw_os_error() == Some(libc::EINTR) {
                            continue;
                        }
                        return Err(error);
                    }
                    if count == 0 {
                        return Err(io::Error::from_raw_os_error(libc::EIO));
                    }
                    offset += count as usize;
                }
                Ok(())
            })();
            let unlock = libc::flock(fd, libc::LOCK_UN);
            result?;
            if unlock != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(())
}
