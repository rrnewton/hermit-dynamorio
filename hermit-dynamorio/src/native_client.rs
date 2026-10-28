/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

// AUTONOMOUS-BOT-IMPLEMENTED

//! Host-only CMake publication for the explicitly selected shared engine.

use std::fs;
use std::io;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::Command;

fn lock_native_client_build(directory: &std::path::Path) -> io::Result<fs::File> {
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(directory.join("build.lock"))?;
    loop {
        // SAFETY: lock owns this valid file descriptor for the lifetime of the lock.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } == 0 {
            return Ok(lock);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

/// Build the native mechanism client against an explicitly verified core engine.
///
/// The CLI owns the measured manifest, source/header hashes, held objects and
/// pre/post checks. These build inputs grant no runtime or private-image
/// authority. There is no install-directory, sibling or legacy-runtime fallback.
pub fn prepare_native_client_for_engine(
    engine: &Path,
    native_header: &Path,
    bridge_source: &Path,
    build_identity: &str,
) -> io::Result<(PathBuf, PathBuf)> {
    if build_identity.len() != 64
        || !build_identity
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "native client requires a complete source/input identity",
        ));
    }
    let value = |path: &Path| -> io::Result<String> {
        let text = path.to_str().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "CMake core-engine input pathname is not UTF-8",
            )
        })?;
        if !path.is_absolute()
            || text
                .bytes()
                .any(|byte| matches!(byte, b';' | b'\\' | b'\n' | b'\r'))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unsupported CMake core-engine pathname",
            ));
        }
        Ok(text.to_owned())
    };
    let engine_value = value(engine)?;
    if native_header.file_name() != Some(std::ffi::OsStr::new("hermit-engine-native-v1.h")) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unexpected core native-header role",
        ));
    }
    let include = native_header.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "native header has no installed directory",
        )
    })?;
    let include_value = value(include)?;
    let bridge_value = value(bridge_source)?;
    let source = fs::canonicalize(reverie_dynamorio::native_client_source_dir())?;
    let directory = client_directory_for_engine(engine, build_identity)?;
    let jobs = std::env::var_os("THIRD_PARTY_BUILD_JOBS")
        .map(|value| {
            value.into_string().map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "THIRD_PARTY_BUILD_JOBS is not UTF-8",
                )
            })
        })
        .transpose()?
        .unwrap_or_else(|| "1".to_owned());
    if jobs.is_empty()
        || !jobs.bytes().all(|byte| byte.is_ascii_digit())
        || jobs.parse::<usize>().ok().is_none_or(|jobs| jobs == 0)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "THIRD_PARTY_BUILD_JOBS must be a positive integer",
        ));
    }
    fs::create_dir_all(&directory)?;
    let _build_lock = lock_native_client_build(&directory)?;
    let build_directory = directory.join("build");
    let configure = Command::new("cmake")
        .arg("-S")
        .arg(&source)
        .arg("-B")
        .arg(&build_directory)
        .arg("-DCMAKE_BUILD_TYPE=Release")
        .arg(format!(
            "-DDynamoRIO_DIR={}",
            reverie_dynamorio::bundled_dynamorio_cmake_dir().display()
        ))
        .arg(format!("-DHERMIT_ENGINE_LIBRARY={engine_value}"))
        .arg(format!(
            "-DHERMIT_ENGINE_NATIVE_INCLUDE_DIRECTORY={include_value}"
        ))
        .arg(format!("-DHERMIT_ENGINE_BRIDGE_SOURCE={bridge_value}"))
        .output()?;
    if !configure.status.success() {
        return Err(io::Error::other(format!(
            "failed to configure the core-engine DynamoRIO client: {}",
            String::from_utf8_lossy(&configure.stderr)
        )));
    }
    let build = Command::new("cmake")
        .arg("--build")
        .arg(&build_directory)
        .arg("--parallel")
        .arg(jobs)
        .output()?;
    if !build.status.success() {
        return Err(io::Error::other(format!(
            "failed to link the core-engine DynamoRIO client: {}",
            String::from_utf8_lossy(&build.stderr)
        )));
    }
    let client = publish_native_client(&build_directory, &directory)?;
    Ok((
        reverie_dynamorio::bundled_drrun_path().to_path_buf(),
        client,
    ))
}

/// The ordinary publication path for this exact computed source identity.
/// This helper does not create, build, discover, or accept a substitute client.
pub(crate) fn published_client_for_engine(
    engine: &Path,
    build_identity: &str,
) -> io::Result<PathBuf> {
    Ok(client_directory_for_engine(engine, build_identity)?.join("libreverie_dynamorio_client.so"))
}

fn client_directory_for_engine(engine: &Path, build_identity: &str) -> io::Result<PathBuf> {
    Ok(engine
        .parent()
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "core engine has no cohort directory",
            )
        })?
        .join(format!("dynamorio-client-{build_identity}")))
}

fn publish_native_client(build_directory: &Path, directory: &Path) -> io::Result<PathBuf> {
    let built_client = build_directory.join("libreverie_dynamorio_client.so");
    if !built_client.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "Detcore DYNAMORIO client was not built at {}",
                built_client.display()
            ),
        ));
    }
    let client = directory.join("libreverie_dynamorio_client.so");
    let temporary = directory.join(format!(
        ".libreverie_dynamorio_client-{}-{:016x}.tmp",
        std::process::id(),
        rand::random::<u64>()
    ));
    let mut input = fs::File::open(&built_client)?;
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    // Copy, never hardlink: a subsequent linker may truncate its output inode.
    // The temporary is on the publication filesystem, so rename is atomic.
    let copied = (|| {
        io::copy(&mut input, &mut output)?;
        output.set_permissions(input.metadata()?.permissions())
    })();
    drop(output);
    let published = copied.and_then(|()| fs::rename(&temporary, &client));
    if published.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    published?;
    Ok(client)
}

#[cfg(test)]
fn build_native_client(
    source: &Path,
    directory: &Path,
    dynamorio: &Path,
    runtime: &Path,
) -> io::Result<PathBuf> {
    fs::create_dir_all(directory)?;
    let _build_lock = lock_native_client_build(directory)?;
    // Readers keep using the published inode while CMake relinks its private
    // output. The writer lock must cover both the build and publication.
    let build_directory = directory.join("build");

    let configure = Command::new("cmake")
        .arg("-S")
        .arg(source)
        .arg("-B")
        .arg(&build_directory)
        .arg("-DCMAKE_BUILD_TYPE=Release")
        .arg(format!("-DDynamoRIO_DIR={}", dynamorio.display()))
        .arg(format!("-DREVERIE_DYNAMORIO_RUNTIME={}", runtime.display()))
        .output()?;
    if !configure.status.success() {
        return Err(io::Error::other(format!(
            "failed to configure Detcore DYNAMORIO client: {}",
            String::from_utf8_lossy(&configure.stderr)
        )));
    }

    let build = Command::new("cmake")
        .arg("--build")
        .arg(&build_directory)
        .arg("--parallel")
        .output()?;
    if !build.status.success() {
        return Err(io::Error::other(format!(
            "failed to build Detcore DYNAMORIO client: {}",
            String::from_utf8_lossy(&build.stderr)
        )));
    }

    publish_native_client(&build_directory, directory)
}

#[cfg(test)]
#[path = "native_client_publication_test.rs"]
mod publication_tests;
