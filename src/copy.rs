use crate::checksum::{fill_checksum, Checksum, Crc64Hasher};
use crate::utils::FileKind;
use anyhow::anyhow;
use anyhow::Context;
use digest::Digest;
use nix::errno::Errno;
use std::collections::HashSet;
use std::fs::File;
use std::io::prelude::*;
use std::io::ErrorKind;
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

/// Copies a file to another and computes the checksum of the original file
fn copy_file(file: impl AsRef<Path>, target: impl AsRef<Path>) -> anyhow::Result<Checksum> {
    let mut crc = Crc64Hasher::default();
    let mut orig_fd = File::open(file.as_ref())
        .with_context(|| format!("Failed to open {} for copy input", file.as_ref().display()))?;
    let meta = orig_fd
        .metadata()
        .with_context(|| format!("Failed to stat {} to copy mode", file.as_ref().display()))?;
    let mode = meta.mode();
    let mut target_fd = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .mode(mode)
        .open(target.as_ref())
        .with_context(|| {
            format!(
                "Failed to open {} for copy output",
                target.as_ref().display()
            )
        })?;
    let mut buffer = [0; 4096];
    loop {
        let n_read = orig_fd
            .read(&mut buffer)
            .with_context(|| format!("Reading from {} for copy input", file.as_ref().display()))?;
        if n_read == 0 {
            break;
        };
        let data = &buffer[..n_read];
        crc.update(data);
        target_fd
            .write_all(data)
            .with_context(|| format!("writing to {} for copy output", target.as_ref().display()))?;
    }
    Ok(crc.into())
}

/// fixes a copy of a file, and checks that the checksum is correct. Returns if the copy was
/// modified.
fn fix_file(
    orig: impl AsRef<Path>,
    target: impl AsRef<Path>,
    checksum: &mut Option<Checksum>,
) -> anyhow::Result<bool> {
    let mut changed = false;
    let mut crc = Crc64Hasher::default();
    let mut target_fd = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(target.as_ref())
    {
        Ok(x) => x,
        Err(e) => match e.raw_os_error().map(Errno::from_i32) {
            Some(Errno::EISDIR) | Some(Errno::ELOOP) => {
                // remove the target and copy it anew
                remove_path(&target).with_context(|| {
                    format!(
                        "removing copy target {} of file {} because it is not a file",
                        target.as_ref().display(),
                        orig.as_ref().display()
                    )
                })?;
                let new_checksum =
                    copy_file(orig.as_ref(), target.as_ref()).with_context(|| {
                        format!(
                            "making a fresh copy of file {} to {}",
                            orig.as_ref().display(),
                            target.as_ref().display(),
                        )
                    })?;

                fill_checksum(checksum, new_checksum).with_context(|| {
                    format!("Bad checksum for file {}", orig.as_ref().display())
                })?;
                return Ok(true);
            }
            _ => Err(e).with_context(|| {
                format!("Failed to open {} for fixing", target.as_ref().display())
            })?,
        },
    };
    let mut orig_fd = File::open(orig.as_ref())
        .with_context(|| format!("Failed to open {} as fix input", orig.as_ref().display()))?;
    let mut reference = [0; 4096];
    let mut actual = [0; 4096];
    let mut offset = 0u64;
    loop {
        // invariant: both fd are at offset `offset` and identical up to there.
        let mut append = false;
        let n_orig = orig_fd
            .read(&mut reference)
            .with_context(|| format!("Reading from {} for comparing", orig.as_ref().display()))?;
        if n_orig == 0 {
            let is_block_device = FileKind::of_file(&target_fd)? == FileKind::Device;
            if !is_block_device {
                let n_read = target_fd.read(&mut actual[..1]).with_context(|| {
                    format!("Reading from {} for comparing", target.as_ref().display())
                })?;
                if n_read != 0 {
                    // target file is longer
                    target_fd
                        .set_len(offset)
                        .with_context(|| format!("Truncating {}", target.as_ref().display()))?;
                    changed = true;
                }
            }
            break;
        }
        let mut n_actual = 0;
        while n_actual < n_orig {
            let n_read = target_fd
                .read(&mut actual[n_actual..n_orig])
                .with_context(|| {
                    format!("Reading from {} for comparing", target.as_ref().display())
                })?;
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
                println!("fixing {}", target.as_ref().display());
            }
            changed = true;
            target_fd
                .seek(std::io::SeekFrom::Start(offset))
                .with_context(|| {
                    format!("seeking in {} for fixing output", target.as_ref().display())
                })?;
            target_fd.write_all(data).with_context(|| {
                format!("writing to {} for fixing output", target.as_ref().display())
            })?;
        }
        offset += n_orig as u64;
    }
    fill_checksum(checksum, crc.into())
        .with_context(|| format!("Bad checksum for file {}", orig.as_ref().display()))?;
    Ok(changed)
}

fn copy_symlink(orig: impl AsRef<Path>, target: impl AsRef<Path>) -> anyhow::Result<Checksum> {
    match std::fs::remove_file(target.as_ref()) {
        Ok(()) => (),
        Err(e) => match e.kind() {
            ErrorKind::NotFound => (),
            _ => Err(e)?,
        },
    }
    let content = std::fs::read_link(orig.as_ref())
        .with_context(|| format!("reading symlink {} for copy", orig.as_ref().display()))?;
    let mut hasher = Crc64Hasher::default();
    hasher.update(content.as_os_str().as_bytes());
    std::os::unix::fs::symlink(content.as_os_str(), target.as_ref()).with_context(|| {
        format!(
            "creating a symlink from {} to {}",
            orig.as_ref().display(),
            target.as_ref().display()
        )
    })?;
    Ok(hasher.into())
}

fn symlink_checksum(path: impl AsRef<Path>) -> anyhow::Result<Checksum> {
    let content = std::fs::read_link(path.as_ref())
        .with_context(|| format!("computing checksum of symlink {}", path.as_ref().display()))?;
    let mut hasher = Crc64Hasher::default();
    hasher.update(content.as_os_str().as_bytes());
    Ok(hasher.into())
}

fn create_directory(target: impl AsRef<Path>) -> anyhow::Result<()> {
    match std::fs::create_dir(target.as_ref()) {
        Ok(()) => Ok(()),
        Err(e) => match e.kind() {
            ErrorKind::AlreadyExists => Ok(()),
            _ => {
                Err(e).with_context(|| format!("creating directory {}", target.as_ref().display()))
            }
        },
    }
}

fn directory_checksum(path: impl AsRef<Path>) -> anyhow::Result<Checksum> {
    // the checksum must not depend on iteration order, so we xor the checksum of all entries
    let hasher = Crc64Hasher::default();
    let mut res = hasher.into();

    for entry in std::fs::read_dir(path.as_ref())
        .with_context(|| format!("computing checksum of {}", path.as_ref().display()))?
    {
        let entry = entry?;
        let mut hasher = Crc64Hasher::default();
        hasher.update(entry.file_name().as_bytes());
        res ^= hasher.into();
    }

    Ok(res)
}

fn remove_path(path: impl AsRef<Path>) -> anyhow::Result<()> {
    match FileKind::of_path(path.as_ref())
        .with_context(|| format!("stat({}) for removal", path.as_ref().display()))?
    {
        FileKind::Directory => std::fs::remove_dir_all(path.as_ref()),
        _ => std::fs::remove_file(path.as_ref()),
    }
    .with_context(|| format!("removing {}", path.as_ref().display()))?;
    Ok(())
}

fn fix_directory(
    orig: impl AsRef<Path>,
    target: impl AsRef<Path>,
    checksum: &mut Option<Checksum>,
) -> anyhow::Result<bool> {
    // the checksum must not depend on iteration order, so we xor the checksum of all entries
    let hasher = Crc64Hasher::default();
    let mut res: Checksum = hasher.into();

    let mut orig_names = HashSet::new();
    let mut target_names = HashSet::new();

    let mut it_target = match std::fs::read_dir(target.as_ref()) {
        Ok(x) => x,
        Err(e) => match e.raw_os_error().map(Errno::from_i32) {
            Some(Errno::ENOTDIR) => {
                // the target is not a directory, let's remove it, and let the next round fix it
                remove_path(&target).with_context(|| {
                    format!(
                        "removing copy target {} of directory {} because it is not a directory",
                        target.as_ref().display(),
                        orig.as_ref().display()
                    )
                })?;
                let new_checksum = copy_directory(&orig, &target).with_context(|| {
                    format!(
                        "making a fresh copy of directory {} to {}",
                        orig.as_ref().display(),
                        target.as_ref().display()
                    )
                })?;
                // check the checksum
                fill_checksum(checksum, new_checksum).with_context(|| {
                    format!("Bad checksum for directory {}", orig.as_ref().display())
                })?;
                return Ok(true);
            }
            _ => Err(e).with_context(|| {
                format!("reading directory for fixing {}", target.as_ref().display())
            })?,
        },
    };

    let it_orig = std::fs::read_dir(orig.as_ref()).with_context(|| {
        format!(
            "reading directory for comparison {}",
            orig.as_ref().display()
        )
    })?;

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
        .with_context(|| format!("Bad checksum for directory {}", orig.as_ref().display()))?;

    // consume remaining dentries
    for entry2 in it_target {
        let entry2 = entry2?;
        target_names.insert(entry2.file_name().to_owned());
    }

    // files to be removed
    let extra = target_names.difference(&orig_names);
    let mut path = target.as_ref().to_path_buf();
    let mut changed = false;
    for name in extra {
        changed = true;
        path.push(name);
        remove_path(&path)
            .with_context(|| format!("removing extra directory member {}", path.display()))?;
        path.pop();
    }

    Ok(changed)
}

fn file_checksum(path: impl AsRef<Path>) -> anyhow::Result<Checksum> {
    let mut hasher = Crc64Hasher::default();
    let mut fd = std::fs::File::open(path.as_ref())
        .with_context(|| format!("opening {} for checksum", path.as_ref().display()))?;
    let mut buffer = [0; 4096];
    loop {
        let n_read = fd
            .read(&mut buffer)
            .with_context(|| format!("reading {} for checksum", path.as_ref().display()))?;
        if n_read == 0 {
            break;
        }
        hasher.update(&buffer[..n_read]);
    }
    Ok(hasher.into())
}

fn fix_symlink(
    orig: impl AsRef<Path>,
    target: impl AsRef<Path>,
    checksum: &mut Option<Checksum>,
) -> anyhow::Result<bool> {
    let c1 = symlink_checksum(orig.as_ref())?;
    fill_checksum(checksum, c1)
        .with_context(|| format!("fixing the copy of {}", orig.as_ref().display()))?;

    let c2 = match symlink_checksum(target.as_ref()) {
        Ok(c2) => Some(c2),
        Err(e) => {
            match e.downcast::<std::io::Error>() {
                Ok(io) => {
                    match io.raw_os_error().map(Errno::from_i32) {
                        Some(Errno::EINVAL) => {
                            // target is not a symbolic link
                            remove_path(target.as_ref()).with_context(|| format!("removing copy target {} of symlink {} because it is not a symlink", target.as_ref().display(), orig.as_ref().display()))?;
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
        copy_symlink(orig.as_ref(), target.as_ref())
            .with_context(|| format!("copy symlink {} to fix", orig.as_ref().display()))?;
        Ok(true)
    } else {
        Ok(false)
    }
}

pub fn copy_directory(
    orig: impl AsRef<Path>,
    target: impl AsRef<Path>,
) -> anyhow::Result<Checksum> {
    create_directory(target.as_ref())?;
    directory_checksum(orig.as_ref())
}

/// Copies a file or directory or symlink `orig` to `target` and returns `orig`'s checksum
pub fn copy_path(orig: impl AsRef<Path>, target: impl AsRef<Path>) -> anyhow::Result<Checksum> {
    match FileKind::of_path(orig.as_ref())
        .with_context(|| format!("stat({}) to copy", orig.as_ref().display()))?
    {
        FileKind::Regular | FileKind::Device => copy_file(orig.as_ref(), target.as_ref()),
        FileKind::Directory => copy_directory(orig.as_ref(), target.as_ref()),
        FileKind::Symlink => {
            copy_symlink(orig.as_ref(), target.as_ref())?;
            symlink_checksum(orig.as_ref())
        }
        FileKind::Other => Err(anyhow!(
            "cannot copy unknown fs path type {}",
            orig.as_ref().display()
        )),
    }
}

/// Returns the checksum of a path, except a device file, because the length to checksum
/// is not known in advance for device files.
#[allow(unused)]
pub fn checksum_path(path: impl AsRef<Path>) -> anyhow::Result<Checksum> {
    match FileKind::of_path(path.as_ref())
        .with_context(|| format!("stat({}) to copy", path.as_ref().display()))?
    {
        FileKind::Regular => file_checksum(path.as_ref()),
        FileKind::Directory => directory_checksum(path.as_ref()),
        FileKind::Symlink => symlink_checksum(path.as_ref()),
        FileKind::Device => Err(anyhow!(
            "cannot checksum device file {}",
            path.as_ref().display()
        )),
        FileKind::Other => Err(anyhow!(
            "cannot checksum unknown fs path type {}",
            path.as_ref().display()
        )),
    }
}

/// Fixes the copy `target` of `orig` which has checksum `checksum`.
/// Returns `true` if some fixing was needed or `false` otherwise.
/// Returns an error if `orig` has changed since it has been checksummed
/// Sets checksum to `Some` if it was `None`.
pub fn fix_path(
    orig: impl AsRef<Path>,
    target: impl AsRef<Path>,
    checksum: &mut Option<Checksum>,
) -> anyhow::Result<bool> {
    match FileKind::of_path(orig.as_ref())
        .with_context(|| format!("stat({}) to fix", orig.as_ref().display()))?
    {
        FileKind::Regular | FileKind::Device => fix_file(orig.as_ref(), target.as_ref(), checksum),
        FileKind::Directory => fix_directory(orig.as_ref(), target.as_ref(), checksum),
        FileKind::Symlink => fix_symlink(orig.as_ref(), target.as_ref(), checksum),
        FileKind::Other => Err(anyhow!(
            "cannot fix unknown fs path type {}",
            orig.as_ref().display()
        )),
    }
}
