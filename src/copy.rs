use crate::cache::CacheManager;
use crate::checksum::{fill_checksum, Checksum, Crc64Hasher};
use crate::progress::Progress;
use crate::utils::FileKind;
use anyhow::anyhow;
use anyhow::Context;
use digest::Digest;
use nix::errno::Errno;
use std::collections::HashSet;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::prelude::*;
use std::io::ErrorKind;
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::os::unix::io::{FromRawFd, IntoRawFd};
use std::path::Path;

#[repr(align(4096))]
struct Buffer([u8; 4096]);

/// Evaluates to a stack allocated buffer of 4096 bytes aligned to 4096. Used for Direct IO.
// Costs an extra memcpy, but oh well...
macro_rules! aligned_buffer({} => {Buffer([0; 4096]).0});

/// Tells the system that this file descriptor will be read sequentially from offset 0 to end of
/// file. The modified file descriptor is returned.
fn fadvise_sequential(f: File) -> anyhow::Result<File> {
    let fd = f.into_raw_fd();
    // by building it now, we ensure the file is closed even if posix_fadvise fails.
    let res = unsafe { File::from_raw_fd(fd) };
    nix::fcntl::posix_fadvise(
        fd,
        0, /* from offset 0 */
        0, /* full file */
        nix::fcntl::PosixFadviseAdvice::POSIX_FADV_SEQUENTIAL,
    )
    .context("posix_fadvise(SEQUENTIAL)")?;
    Ok(res)
}

/// Copies a file to another and computes the checksum of the original file
fn copy_file(
    cache_manager: &dyn CacheManager,
    progress: &Progress,
    file: &Path,
    target: &Path,
) -> anyhow::Result<Checksum> {
    let mut crc = Crc64Hasher::default();
    let orig_fd = File::open(file)
        .with_context(|| format!("Failed to open {} for copy input", file.display()))?;
    let mut orig_fd = fadvise_sequential(orig_fd)
        .with_context(|| format!("posix_fadvise({}, SEQUENTIAL)", file.display()))?;
    let meta = orig_fd
        .metadata()
        .with_context(|| format!("Failed to stat {} to copy mode", file.display()))?;
    let mode = meta.mode();
    let mut target_fd = cache_manager
        .open_no_cache(
            std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .mode(mode),
            0,
            target,
        )
        .with_context(|| format!("Failed to open {} for copy output", target.display()))?;
    let mut buffer = aligned_buffer!();
    loop {
        let n_read = orig_fd
            .read(&mut buffer)
            .with_context(|| format!("Reading from {} for copy input", file.display()))?;
        if n_read == 0 {
            break;
        };
        let data = &buffer[..n_read];
        crc.update(data);
        target_fd
            .write_all(data)
            .with_context(|| format!("writing to {} for copy output", target.display()))?;
        progress.do_bytes(data.len() as u64);
    }
    Ok(crc.into())
}

/// fixes a copy of a file, and checks that the checksum is correct. Returns if the copy was
/// modified.
fn fix_file(
    cache_manager: &dyn CacheManager,
    progress: &Progress,
    orig: &Path,
    target: &Path,
    checksum: &mut Option<Checksum>,
) -> anyhow::Result<bool> {
    let mut changed = false;
    let mut crc = Crc64Hasher::default();
    let mut target_fd = match cache_manager.open_no_cache(
        std::fs::OpenOptions::new().read(true).write(true),
        libc::O_NOFOLLOW,
        target,
    ) {
        Ok(x) => x,
        Err(e) => match e.raw_os_error().map(Errno::from_i32) {
            Some(Errno::EISDIR) | Some(Errno::ELOOP) => {
                // remove the target and copy it anew
                remove_path(progress, &target).with_context(|| {
                    format!(
                        "removing copy target {} of file {} because it is not a file",
                        target.display(),
                        orig.display()
                    )
                })?;
                let new_checksum =
                    copy_file(cache_manager, progress, orig, target).with_context(|| {
                        format!(
                            "making a fresh copy of file {} to {}",
                            orig.display(),
                            target.display(),
                        )
                    })?;

                fill_checksum(checksum, new_checksum)
                    .with_context(|| format!("Bad checksum for file {}", orig.display()))?;
                return Ok(true);
            }
            _ => {
                Err(e).with_context(|| format!("Failed to open {} for fixing", target.display()))?
            }
        },
    };
    let orig_fd = File::open(orig)
        .with_context(|| format!("Failed to open {} as fix input", orig.display()))?;
    let mut orig_fd = fadvise_sequential(orig_fd)
        .with_context(|| format!("posix_fadvise({}, SEQUENTIAL)", orig.display()))?;
    let mut reference = aligned_buffer!();
    let mut actual = aligned_buffer!();
    let mut offset = 0u64;
    loop {
        // invariant: both fd are at offset `offset` and identical up to there.
        let mut append = false;
        let n_orig = orig_fd
            .read(&mut reference)
            .with_context(|| format!("Reading from {} for comparing", orig.display()))?;
        if n_orig == 0 {
            let is_block_device = FileKind::of_file(&target_fd)? == FileKind::Device;
            if !is_block_device {
                let n_read = target_fd
                    .read(&mut actual[..1])
                    .with_context(|| format!("Reading from {} for comparing", target.display()))?;
                if n_read != 0 {
                    // target file is longer
                    target_fd
                        .set_len(offset)
                        .with_context(|| format!("Truncating {}", target.display()))?;
                    changed = true;
                }
            }
            break;
        }
        let mut n_actual = 0;
        while n_actual < n_orig {
            let n_read = target_fd
                .read(&mut actual[n_actual..n_orig])
                .with_context(|| format!("Reading from {} for comparing", target.display()))?;
            n_actual += n_read;
            if n_read == 0 {
                // orig file is longer
                append = true;
                break;
            };
        }
        let data = &reference[..n_orig];
        crc.update(data);
        if append || data != &actual[..n_orig] {
            if !changed {
                progress.set_status(format!("Fixing {}", target.display()));
            }
            changed = true;
            target_fd
                .seek(std::io::SeekFrom::Start(offset))
                .with_context(|| format!("seeking in {} for fixing output", target.display()))?;
            target_fd
                .write_all(data)
                .with_context(|| format!("writing to {} for fixing output", target.display()))?;
        }
        offset += n_orig as u64;
        progress.do_bytes(n_orig as u64);
    }
    fill_checksum(checksum, crc.into())
        .with_context(|| format!("Bad checksum for file {}", orig.display()))?;
    Ok(changed)
}

fn copy_symlink(orig: &Path, target: &Path) -> anyhow::Result<Checksum> {
    match std::fs::remove_file(target) {
        Ok(()) => (),
        Err(e) => match e.kind() {
            ErrorKind::NotFound => (),
            _ => Err(e)?,
        },
    }
    let content = std::fs::read_link(orig)
        .with_context(|| format!("reading symlink {} for copy", orig.display()))?;
    let mut hasher = Crc64Hasher::default();
    hasher.update(content.as_os_str().as_bytes());
    std::os::unix::fs::symlink(content.as_os_str(), target).with_context(|| {
        format!(
            "creating a symlink from {} to {}",
            orig.display(),
            target.display()
        )
    })?;
    Ok(hasher.into())
}

fn symlink_checksum(path: &Path) -> anyhow::Result<Checksum> {
    let content = std::fs::read_link(path)
        .with_context(|| format!("computing checksum of symlink {}", path.display()))?;
    let mut hasher = Crc64Hasher::default();
    hasher.update(content.as_os_str().as_bytes());
    Ok(hasher.into())
}

fn create_directory(target: &Path) -> anyhow::Result<()> {
    match std::fs::create_dir(target) {
        Ok(()) => Ok(()),
        Err(e) => match e.kind() {
            ErrorKind::AlreadyExists => Ok(()),
            _ => Err(e).with_context(|| format!("creating directory {}", target.display())),
        },
    }
}

fn directory_checksum(path: &Path) -> anyhow::Result<Checksum> {
    // the checksum must not depend on iteration order, so we xor the checksum of all entries
    let hasher = Crc64Hasher::default();
    let mut res = hasher.into();

    for entry in std::fs::read_dir(path)
        .with_context(|| format!("computing checksum of {}", path.display()))?
    {
        let entry = entry?;
        let mut hasher = Crc64Hasher::default();
        hasher.update(entry.file_name().as_bytes());
        res ^= hasher.into();
    }

    Ok(res)
}

fn remove_path(progress: &Progress, path: &Path) -> anyhow::Result<()> {
    progress.set_status(format!("Removing {}", path.display()));
    match FileKind::of_path(path)
        .with_context(|| format!("stat({}) for removal", path.display()))?
    {
        FileKind::Directory => std::fs::remove_dir_all(path),
        _ => std::fs::remove_file(path),
    }
    .with_context(|| format!("removing {}", path.display()))?;
    Ok(())
}

fn fix_directory(
    progress: &Progress,
    orig: &Path,
    target: &Path,
    checksum: &mut Option<Checksum>,
) -> anyhow::Result<bool> {
    // the checksum must not depend on iteration order, so we xor the checksum of all entries
    let hasher = Crc64Hasher::default();
    let mut res: Checksum = hasher.into();

    let mut orig_names = HashSet::new();
    let mut target_names = HashSet::new();

    // unfortunately, read_dir follows symlinks, so we have to stat() before
    let raw_it_target = match FileKind::of_path(target).with_context(|| {
        format!(
            "stat({}) to check if it is a directory before listing it for fixing",
            target.display(),
        )
    })? {
        FileKind::Directory => std::fs::read_dir(target),
        _ => Err(Errno::ENOTDIR.into()),
    };

    let mut it_target = match raw_it_target {
        Ok(x) => x,
        Err(e) => match e.raw_os_error().map(Errno::from_i32) {
            Some(Errno::ENOTDIR) => {
                // the target is not a directory, let's remove it and copy again
                remove_path(progress, &target).with_context(|| {
                    format!(
                        "removing copy target {} of directory {} because it is not a directory",
                        target.display(),
                        orig.display()
                    )
                })?;
                let new_checksum = copy_directory(&orig, &target).with_context(|| {
                    format!(
                        "making a fresh copy of directory {} to {}",
                        orig.display(),
                        target.display()
                    )
                })?;
                // check the checksum
                fill_checksum(checksum, new_checksum)
                    .with_context(|| format!("Bad checksum for directory {}", orig.display()))?;
                return Ok(true);
            }
            _ => Err(e)
                .with_context(|| format!("reading directory for fixing {}", target.display()))?,
        },
    };

    let it_orig = std::fs::read_dir(orig)
        .with_context(|| format!("reading directory for comparison {}", orig.display()))?;

    for entry in it_orig {
        let entry = entry?;
        let mut hasher = Crc64Hasher::default();
        let name = entry.file_name();
        let bytes = name.as_bytes();
        hasher.update(bytes);
        res ^= hasher.into();
        match it_target.next() {
            Some(Err(e)) => Err(e)?,
            None => {
                orig_names.insert(name.to_owned());
            }
            Some(Ok(entry2)) => {
                let name2 = entry2.file_name();
                if name2 != name {
                    target_names.insert(name2.to_owned());
                    orig_names.insert(name.to_owned());
                }
            }
        }
    }

    // check the checksum
    fill_checksum(checksum, res)
        .with_context(|| format!("Bad checksum for directory {}", orig.display()))?;

    // consume remaining dentries
    for entry2 in it_target {
        let entry2 = entry2?;
        target_names.insert(entry2.file_name().to_owned());
    }

    // files to be removed
    let extra = target_names.difference(&orig_names);
    let mut path = target.to_path_buf();
    let mut changed = false;
    for name in extra {
        changed = true;
        path.push(name);
        remove_path(progress, &path)
            .with_context(|| format!("removing extra directory member {}", path.display()))?;
        path.pop();
    }

    Ok(changed)
}

fn file_checksum(cache_manager: &mut dyn CacheManager, path: &Path) -> anyhow::Result<Checksum> {
    let mut hasher = Crc64Hasher::default();
    let fd = cache_manager
        .open_no_cache(OpenOptions::new().read(true), libc::O_NOFOLLOW, path)
        .with_context(|| format!("opening {} for checksum", path.display()))?;
    let mut fd = fadvise_sequential(fd)
        .with_context(|| format!("posix_fadvise({}, SEQUENTIAL)", path.display()))?;
    let mut buffer = aligned_buffer!();
    loop {
        let n_read = fd
            .read(&mut buffer)
            .with_context(|| format!("reading {} for checksum", path.display()))?;
        if n_read == 0 {
            break;
        }
        hasher.update(&buffer[..n_read]);
    }
    Ok(hasher.into())
}

fn fix_symlink(
    progress: &Progress,
    orig: &Path,
    target: &Path,
    checksum: &mut Option<Checksum>,
) -> anyhow::Result<bool> {
    let c1 = symlink_checksum(orig)?;
    fill_checksum(checksum, c1)
        .with_context(|| format!("fixing the copy of {}", orig.display()))?;

    let c2 = match symlink_checksum(target) {
        Ok(c2) => Some(c2),
        Err(e) => {
            match e.downcast::<std::io::Error>() {
                Ok(io) => {
                    match io.raw_os_error().map(Errno::from_i32) {
                        Some(Errno::EINVAL) => {
                            // target is not a symbolic link
                            remove_path(progress, target).with_context(|| format!("removing copy target {} of symlink {} because it is not a symlink", target.display(), orig.display()))?;
                            None
                        }
                        _ => Err(io)?,
                    }
                }
                Err(e) => Err(e)?,
            }
        }
    };
    if c2 != Some(c1) {
        // needs fixing
        progress.set_status(format!("Fixing {}", target.display()));
        copy_symlink(orig, target)
            .with_context(|| format!("copy symlink {} to fix", orig.display()))?;
        Ok(true)
    } else {
        Ok(false)
    }
}

pub fn copy_directory(orig: &Path, target: &Path) -> anyhow::Result<Checksum> {
    create_directory(target)?;
    directory_checksum(orig)
}

/// Copies a file or directory or symlink `orig` to `target` and returns `orig`'s checksum
pub fn copy_path(
    cache_manager: &dyn CacheManager,
    progress: &Progress,
    orig: &Path,
    target: &Path,
) -> anyhow::Result<Checksum> {
    match FileKind::of_path(orig).with_context(|| format!("stat({}) to copy", orig.display()))? {
        FileKind::Regular | FileKind::Device => copy_file(cache_manager, progress, orig, target),
        FileKind::Directory => copy_directory(orig, target),
        FileKind::Symlink => {
            copy_symlink(orig, target)?;
            symlink_checksum(orig)
        }
        FileKind::Other => Err(anyhow!(
            "cannot copy unknown fs path type {}",
            orig.display()
        )),
    }
}

/// Returns the checksum of a path, except a device file, because the length to checksum
/// is not known in advance for device files.
#[allow(unused)]
pub fn checksum_path(
    cache_manager: &mut dyn CacheManager,
    path: &Path,
) -> anyhow::Result<Checksum> {
    match FileKind::of_path(path).with_context(|| format!("stat({}) to copy", path.display()))? {
        FileKind::Regular => file_checksum(cache_manager, path),
        FileKind::Directory => directory_checksum(path),
        FileKind::Symlink => symlink_checksum(path),
        FileKind::Device => Err(anyhow!("cannot checksum device file {}", path.display())),
        FileKind::Other => Err(anyhow!(
            "cannot checksum unknown fs path type {}",
            path.display()
        )),
    }
}

/// Fixes the copy `target` of `orig` which has checksum `checksum`.
/// Returns `true` if some fixing was needed or `false` otherwise.
/// Returns an error if `orig` has changed since it has been checksummed
/// Sets checksum to `Some` if it was `None`.
pub fn fix_path(
    cache_manager: &dyn CacheManager,
    progress: &Progress,
    orig: &Path,
    target: &Path,
    checksum: &mut Option<Checksum>,
) -> anyhow::Result<bool> {
    match FileKind::of_path(orig).with_context(|| format!("stat({}) to fix", orig.display()))? {
        FileKind::Regular | FileKind::Device => {
            fix_file(cache_manager, progress, orig, target, checksum)
        }
        FileKind::Directory => fix_directory(progress, orig, target, checksum),
        FileKind::Symlink => fix_symlink(progress, orig, target, checksum),
        FileKind::Other => Err(anyhow!(
            "cannot fix unknown fs path type {}",
            orig.display()
        )),
    }
}
