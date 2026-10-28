/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Backend-owned launch image and runtime fingerprint checks.

use anyhow::anyhow;
use goblin::elf::Elf;
use hermit::{DetConfig, Error};
use std::fs;
use std::path::PathBuf;

/// The program a DYNAMORIO launch runs and the working directory it runs from, as
/// reverie-dynamorio's launcher finds them: a name without a `/` through `path`,
/// whose relative entries are taken from the guest's working directory;
/// `cwd` relative to `current` (the launcher's own directory) when it is
/// relative, and `current` when there is none. `None` where the search finds
/// no executable file.
fn dynamorio_launched_program(
    program: &std::path::Path,
    cwd: Option<&std::path::Path>,
    path: Option<&std::ffi::OsStr>,
    current: &std::path::Path,
) -> Option<(PathBuf, PathBuf)> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;
    let cwd = cwd.map_or_else(|| current.to_owned(), |cwd| current.join(cwd));
    let spelled = if program.as_os_str().as_bytes().contains(&b'/') {
        program.to_owned()
    } else {
        std::env::split_paths(path?)
            .map(|directory| cwd.join(directory).join(program))
            .find(|candidate| {
                candidate.metadata().is_ok_and(|metadata| {
                    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
                })
            })?
    };
    Some((spelled, cwd))
}

#[cfg(test)]
mod dynamorio_launched_program_tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn executable(path: &std::path::Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn a_relative_path_entry_is_taken_from_the_guests_directory_not_the_launchers() {
        let root = tempfile::tempdir().unwrap();
        let launcher_dir = root.path().join("launcher");
        // A decoy where the launcher's own directory would find it, and the
        // program where the guest's directory does.
        executable(&launcher_dir.join("bin/prog"));
        executable(&root.path().join("guest/bin/prog"));
        std::fs::write(root.path().join("guest/plain"), b"").unwrap();
        let path = std::ffi::OsStr::new("bin");
        let found = |program: &str, cwd: Option<&str>| {
            dynamorio_launched_program(
                std::path::Path::new(program),
                cwd.map(std::path::Path::new),
                Some(path),
                &launcher_dir,
            )
        };
        let guest = root.path().join("guest");
        // An absolute and a relative working directory find the same file.
        let expected = Some((guest.join("bin/prog"), guest.clone()));
        assert_eq!(found("prog", Some(guest.to_str().unwrap())), expected);
        assert_eq!(
            found("prog", Some("../guest")),
            Some((
                launcher_dir.join("../guest/bin/prog"),
                launcher_dir.join("../guest")
            ))
        );
        // Without a working directory, the launcher's own is the guest's.
        assert_eq!(
            found("prog", None),
            Some((launcher_dir.join("bin/prog"), launcher_dir.clone()))
        );
        // A name with a `/` is not searched for.
        assert_eq!(
            found("./plain", Some(guest.to_str().unwrap())),
            Some((std::path::PathBuf::from("./plain"), guest.clone()))
        );
        // A file that is not executable is not found.
        assert_eq!(found("plain", Some(guest.to_str().unwrap())), None);
    }
}

/// Records, for a DYNAMORIO run, the guest's first image: `program`, found and
/// run from `cwd` with the guest's `PATH`, `path`, as the launch finds it
/// (`dynamorio_launched_program`), then resolved as the kernel resolves an
/// `execve`. Both DYNAMORIO launch paths call it in the container, whose root is
/// the guest's, in place of `launched_image`'s search, which takes a
/// relative `PATH` entry from the coordinator's directory rather than the
/// guest's. Where nothing is found the record stays unset, and the image's
/// own `AT_EXECFN` gives it after the exec.
pub fn record_dynamorio_launched_image(
    config: &mut DetConfig,
    program: &std::path::Path,
    cwd: Option<&std::path::Path>,
    path: Option<&std::ffi::OsStr>,
) {
    let exe = std::env::current_dir()
        .ok()
        .and_then(|current| dynamorio_launched_program(program, cwd, path, &current))
        .and_then(|(spelled, cwd)| {
            detcore::exec_record::ExecRecord::resolve(std::path::Path::new("/"), &cwd, &spelled)
        })
        .map(|record| record.exe);
    config
        .launch_image
        .get_or_insert(detcore::LaunchImage {
            exe: None,
            launcher: None,
        })
        .exe = exe;
}

/// Refuses the DYNAMORIO runtime at `runtime` unless it exports
/// [`detcore_dynamorio::FINGERPRINT_CHECK_SYMBOL`], so unless it refuses a
/// coordinator from another tree itself
/// (`detcore_dynamorio::CONFIG_FINGERPRINT_ENV`). A runtime built before that check
/// takes this Hermit's configuration whatever its own build, and with the
/// global RPC changed since, it would keep a host value Detcore now replaces
/// (an inotify rename cookie, for one) rather than be refused by name.
#[cfg(test)]
pub fn require_dynamorio_runtime_fingerprint_check(runtime: &std::path::Path) -> Result<(), Error> {
    let symbols = defined_dynamic_symbols(runtime)
        .map_err(|error| error.context("failed to read the Detcore DYNAMORIO runtime's exports"))?;
    if symbols
        .iter()
        .any(|symbol| symbol == detcore_dynamorio::FINGERPRINT_CHECK_SYMBOL)
    {
        Ok(())
    } else {
        Err(anyhow!(
            "the Detcore DYNAMORIO runtime {} predates its check of the coordinator's fingerprint (it \
             exports no {}), so it would run with this hermit whatever tree it was built from. \
             Rebuild it from Hermit's tree (`cargo build -p detcore-dynamorio-cdylib`)",
            runtime.display(),
            detcore_dynamorio::FINGERPRINT_CHECK_SYMBOL
        ))
    }
}

/// The names of the symbols the ELF image at `path` defines in its dynamic
/// symbol table, the table the dynamic loader binds from. This reads only the
/// parts of the file the loader uses for them -- the ELF and program headers,
/// the dynamic segment, the symbol hash table, and the dynamic symbol and
/// string tables: 6,467 bytes of a 19.5 MB release DYNAMORIO runtime -- and so does
/// not need the section headers or `.symtab`, which stripping can remove.
#[cfg(test)]
fn defined_dynamic_symbols(path: &std::path::Path) -> Result<Vec<String>, Error> {
    let file = fs::File::open(path)
        .map_err(|error| anyhow!("failed to open {}: {error}", path.display()))?;
    defined_dynamic_symbols_in_file(&file, path)
}

/// Read the existing loader-visible symbol table from an already held inode.
pub(crate) fn defined_dynamic_symbols_in_file(
    file: &fs::File,
    path: &std::path::Path,
) -> Result<Vec<String>, Error> {
    dynamic_symbols_in_file(file, path, true)
}

/// Read actual undefined callback imports through the same bounded ELF reader.
pub(crate) fn undefined_dynamic_symbols_in_file(
    file: &fs::File,
    path: &std::path::Path,
) -> Result<Vec<String>, Error> {
    dynamic_symbols_in_file(file, path, false)
}

fn dynamic_symbols_in_file(
    file: &fs::File,
    path: &std::path::Path,
    defined: bool,
) -> Result<Vec<String>, Error> {
    use std::os::unix::fs::FileExt;

    use goblin::container::Ctx;
    use goblin::elf::dynamic::Dynamic;
    use goblin::elf::dynamic::DynamicInfo;
    use goblin::elf::program_header::PT_DYNAMIC;
    use goblin::elf::program_header::ProgramHeader;
    use goblin::elf::section_header::SHN_UNDEF;
    use goblin::elf::sym::Sym;
    use goblin::elf::sym::Symtab;
    use goblin::strtab::Strtab;

    let length = file
        .metadata()
        .map_err(|error| anyhow!("failed to stat {}: {error}", path.display()))?
        .len();
    let not_elf = |error: &dyn std::fmt::Display| anyhow!("{} is not ELF: {error}", path.display());
    let read = |offset: u64, size: usize| -> Result<Vec<u8>, Error> {
        if offset
            .checked_add(size as u64)
            .is_none_or(|end| end > length)
        {
            return Err(not_elf(&format_args!(
                "{size} bytes at {offset:#x} would end past its {length} bytes"
            )));
        }
        let mut bytes = vec![0; size];
        file.read_exact_at(&mut bytes, offset)
            .map_err(|error| anyhow!("failed to read {}: {error}", path.display()))?;
        Ok(bytes)
    };

    let header = Elf::parse_header(&read(0, goblin::elf64::header::SIZEOF_EHDR)?)
        .map_err(|error| not_elf(&error))?;
    let ctx = Ctx::new(
        header.container().map_err(|error| not_elf(&error))?,
        header.endianness().map_err(|error| not_elf(&error))?,
    );
    let count = usize::from(header.e_phnum);
    let program_headers = ProgramHeader::parse(
        &read(header.e_phoff, count * ProgramHeader::size(ctx))?,
        0,
        count,
        ctx,
    )
    .map_err(|error| not_elf(&error))?;
    // An image without a dynamic segment exports nothing.
    let Some(segment) = program_headers
        .iter()
        .find(|header| header.p_type == PT_DYNAMIC)
    else {
        return Ok(Vec::new());
    };
    // `Dynamic::parse` reads the segment from the bytes it is given at the
    // segment's file offset, so it is given the segment alone, at offset 0.
    // Its addresses are then mapped to file offsets with every header.
    let entries = read(
        segment.p_offset,
        usize::try_from(segment.p_filesz).map_err(|error| not_elf(&error))?,
    )?;
    let alone = ProgramHeader {
        p_offset: 0,
        ..segment.clone()
    };
    let Some(dynamic) = Dynamic::parse(&entries, &[alone], ctx).map_err(|error| not_elf(&error))?
    else {
        return Ok(Vec::new());
    };
    let info = DynamicInfo::new(&dynamic.dyns, &program_headers);
    let count = dynamic_symbol_count(&read, &info, ctx)?;
    if count == 0 || info.symtab == 0 || info.strtab == 0 {
        return Ok(Vec::new());
    }
    let symbols = read(
        info.symtab as u64,
        count.saturating_mul(Sym::size(ctx.container)),
    )?;
    let symbols = Symtab::parse(&symbols, 0, count, ctx).map_err(|error| not_elf(&error))?;
    let names = read(info.strtab as u64, info.strsz)?;
    let names = Strtab::parse(&names, 0, info.strsz, 0).map_err(|error| not_elf(&error))?;
    Ok(symbols
        .iter()
        .filter(|symbol| (symbol.st_shndx != SHN_UNDEF as usize) == defined)
        .filter_map(|symbol| names.get_at(symbol.st_name).map(str::to_owned))
        .collect())
}

/// How many entries an ELF image's dynamic symbol table has, which only the
/// section headers record; without them it comes from the symbol hash table,
/// as goblin and readelf find it. A GNU hash table leaves out the first
/// `symoffset` symbols and chains every later one, so the count is one past
/// the end of the chain that starts last (a chain's final entry has its low
/// bit set); a SysV hash table's chain count is the symbol count.
fn dynamic_symbol_count(
    read: &dyn Fn(u64, usize) -> Result<Vec<u8>, Error>,
    info: &goblin::elf::dynamic::DynamicInfo,
    ctx: goblin::container::Ctx,
) -> Result<usize, Error> {
    let word = |bytes: &[u8], index: usize| {
        let bytes: [u8; 4] = bytes[4 * index..4 * index + 4]
            .try_into()
            .expect("four bytes");
        if ctx.is_little_endian() {
            u32::from_le_bytes(bytes)
        } else {
            u32::from_be_bytes(bytes)
        }
    };
    if let Some(table) = info.gnu_hash {
        // nbuckets, symoffset, bloom_size and bloom_shift; then the bloom
        // filter's address-sized words, the buckets, and the chains.
        let header = read(table, 16)?;
        let buckets = word(&header, 0) as usize;
        let first = word(&header, 1) as usize;
        let starts_at = table + 16 + u64::from(word(&header, 2)) * ctx.size() as u64;
        let starts = read(starts_at, 4 * buckets)?;
        let Some(last) = (0..buckets)
            .map(|bucket| word(&starts, bucket) as usize)
            .max()
            .filter(|&last| last >= first)
        else {
            return Ok(first);
        };
        let chains_at = starts_at + 4 * buckets as u64;
        let mut symbol = last;
        while word(&read(chains_at + 4 * (symbol - first) as u64, 4)?, 0) & 1 == 0 {
            symbol += 1;
        }
        Ok(symbol + 1)
    } else if let Some(table) = info.hash {
        // nbucket, then nchain.
        Ok(word(&read(table, 8)?, 1) as usize)
    } else {
        Ok(0)
    }
}

/// Declares, for a DYNAMORIO run through `drrun`, the image the kernel execs in the
/// guest's place: DynamoRIO's library (`lib64/release/libdynamorio.so` beside
/// `drrun`'s `bin64`), which loads the guest itself. So the kernel's `exe`
/// link and program break are DynamoRIO's (`detcore::LaunchImage::launcher`),
/// and Detcore answers the guest's brk from its own model; the runner tells
/// DynamoRIO to reserve nothing for the break
/// (`reverie_dynamorio::DynamoRioRunner::tool_answers_brk`).
/// An error where that library is missing: the run would otherwise
/// declare a launcher the kernel never execs, and refuse every brk for want
/// of a record the launcher check can accept.
pub fn declare_dynamorio_launcher(
    config: &mut DetConfig,
    drrun: &std::path::Path,
) -> Result<(), Error> {
    let library = drrun
        .parent()
        .and_then(std::path::Path::parent)
        .unwrap_or(std::path::Path::new("/"))
        .join("lib64/release/libdynamorio.so");
    if !library.is_file() {
        return Err(anyhow!(
            "DynamoRIO's library is not at {} beside drrun {}",
            library.display(),
            drrun.display()
        ));
    }
    config
        .launch_image
        .get_or_insert(detcore::LaunchImage {
            exe: None,
            launcher: None,
        })
        .launcher = Some(library);
    Ok(())
}

#[cfg(test)]
mod runtime_export_tests {
    use super::*;
    #[test]
    fn a_dynamorio_runtime_without_the_fingerprint_check_is_refused() {
        let image = std::env::current_exe().unwrap();
        let refusal = super::require_dynamorio_runtime_fingerprint_check(&image)
            .expect_err("an image without the symbol is refused");
        let message = format!("{refusal:#}");
        assert!(
            message.contains("predates its check of the coordinator's fingerprint")
                && message.contains(detcore_dynamorio::FINGERPRINT_CHECK_SYMBOL),
            "{message}"
        );
        let refusal = super::require_dynamorio_runtime_fingerprint_check(std::path::Path::new(
            "/proc/self/cmdline",
        ))
        .expect_err("a file that is not ELF is refused");
        assert!(format!("{refusal:#}").contains("is not ELF"), "{refusal:#}");
    }

    #[test]
    fn defined_dynamic_symbols_are_read_from_the_dynamic_segment_alone() {
        // The libc this test runs with, against goblin's parse of all of it.
        let maps = fs::read_to_string("/proc/self/maps").unwrap();
        let libc = maps
            .lines()
            .filter_map(|line| line.split_whitespace().nth(5))
            .find(|path| {
                path.rsplit('/')
                    .next()
                    .is_some_and(|name| name.starts_with("libc.so"))
            })
            .expect("this test runs with a dynamically linked libc");
        let ours = super::defined_dynamic_symbols(std::path::Path::new(libc)).unwrap();
        let bytes = fs::read(libc).unwrap();
        let elf = goblin::elf::Elf::parse(&bytes).unwrap();
        let named = |symbol: &goblin::elf::sym::Sym| elf.dynstrtab.get_at(symbol.st_name);
        assert!(
            elf.dynsyms
                .iter()
                .any(|symbol| symbol.st_shndx == 0 && named(&symbol).is_some_and(|n| !n.is_empty())),
            "{libc} imports a symbol, which is not one of its exports"
        );
        let theirs: Vec<String> = elf
            .dynsyms
            .iter()
            .filter(|symbol| symbol.st_shndx != 0)
            .filter_map(|symbol| named(&symbol).map(str::to_owned))
            .collect();
        assert!(ours.iter().any(|name| name == "malloc"), "{ours:?}");
        let held = fs::File::open(libc).unwrap();
        let undefined =
            super::undefined_dynamic_symbols_in_file(&held, std::path::Path::new(libc)).unwrap();
        let expected_undefined = elf
            .dynsyms
            .iter()
            .filter(|symbol| symbol.st_shndx == 0)
            .filter_map(|symbol| named(&symbol).map(str::to_owned))
            .collect::<Vec<_>>();
        assert_eq!(
            undefined, expected_undefined,
            "actual undefined dynamic imports"
        );
        assert_eq!(ours, theirs);
    }

    #[test]
    fn the_dynamic_symbol_count_comes_from_the_hash_table() {
        use goblin::container::Container;
        use goblin::container::Ctx;
        use goblin::container::Endian;
        use goblin::elf::dynamic::DynamicInfo;

        let ctx = Ctx::new(Container::Big, Endian::Little);
        let image = |words: &[u32]| -> Vec<u8> {
            words.iter().flat_map(|word| word.to_le_bytes()).collect()
        };
        let reader = |image: Vec<u8>| {
            move |offset: u64, size: usize| -> Result<Vec<u8>, anyhow::Error> {
                let start = offset as usize;
                image
                    .get(start..start + size)
                    .map(<[u8]>::to_vec)
                    .ok_or_else(|| anyhow::anyhow!("{size} bytes at {offset} are past the end"))
            }
        };
        let gnu = DynamicInfo {
            gnu_hash: Some(0),
            ..Default::default()
        };
        // nbuckets 2, symoffset 1, bloom_size 1, bloom_shift 6; one 8-byte
        // bloom word; buckets starting at symbols 1 and 3; the chains of
        // symbols 1 to 4, where a set low bit ends a chain. Symbol 0 is not
        // hashed, so the table has 5 symbols.
        let table = image(&[2, 1, 1, 6, 0, 0, 1, 3, 0x10, 0x21, 0x30, 0x41]);
        assert_eq!(
            super::dynamic_symbol_count(&reader(table), &gnu, ctx).unwrap(),
            5
        );
        // With every bucket empty, only the 3 symbols before symoffset.
        let table = image(&[1, 3, 1, 6, 0, 0, 0]);
        assert_eq!(
            super::dynamic_symbol_count(&reader(table), &gnu, ctx).unwrap(),
            3
        );
        // A chain that runs past the image is refused, not counted.
        let table = image(&[1, 1, 1, 6, 0, 0, 1, 0x10]);
        super::dynamic_symbol_count(&reader(table), &gnu, ctx).unwrap_err();
        // A SysV table: nbucket, then nchain.
        let sysv = DynamicInfo {
            hash: Some(0),
            ..Default::default()
        };
        let table = image(&[1, 7, 0]);
        assert_eq!(
            super::dynamic_symbol_count(&reader(table), &sysv, ctx).unwrap(),
            7
        );
        // With neither, the loader can bind nothing.
        assert_eq!(
            super::dynamic_symbol_count(&reader(Vec::new()), &DynamicInfo::default(), ctx).unwrap(),
            0
        );
    }
}
