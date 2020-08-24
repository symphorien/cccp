use crate::checksum::{Checksum, Crc64Hasher};
use crate::utils::FileKind;
use anyhow::Context;
use digest::Digest;
use std::fs::File;
use std::io::prelude::*;
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

use anyhow::anyhow;

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
    checksum: Checksum,
) -> anyhow::Result<bool> {
    let mut changed = false;
    let mut crc = Crc64Hasher::default();
    let mut orig_fd = File::open(orig.as_ref())
        .with_context(|| format!("Failed to open {} as fix input", orig.as_ref().display()))?;
    let mut target_fd = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(target.as_ref())
        .with_context(|| format!("Failed to open {} for fixing", target.as_ref().display()))?;
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
    if checksum != crc.into() {
        return Err(anyhow!("Bad checksum for file {}", orig.as_ref().display()));
    }
    Ok(changed)
}

fn copy_symlink(orig: impl AsRef<Path>, target: impl AsRef<Path>) -> anyhow::Result<()> {
    std::os::unix::fs::symlink(orig.as_ref(), target.as_ref()).with_context(|| {
        format!(
            "creating a symlink from {} to {}",
            orig.as_ref().display(),
            target.as_ref().display()
        )
    })?;
    Ok(())
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
            std::io::ErrorKind::AlreadyExists => Ok(()),
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

/// Copies a file or directory or symlink `orig` to `target` and returns `orig`'s checksum
pub fn copy_path(orig: impl AsRef<Path>, target: impl AsRef<Path>) -> anyhow::Result<Checksum> {
    match FileKind::of(orig.as_ref())
        .with_context(|| format!("stat({}) to copy", orig.as_ref().display()))?
    {
        FileKind::Regular | FileKind::Device => copy_file(orig.as_ref(), target.as_ref()),
        FileKind::Directory => {
            create_directory(target.as_ref())?;
            directory_checksum(orig.as_ref())
        }
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

/// Returns the checksum of a path
pub fn checksum_path(path: impl AsRef<Path>) -> anyhow::Result<Checksum> {
    match FileKind::of(path.as_ref())
        .with_context(|| format!("stat({}) to copy", path.as_ref().display()))?
    {
        FileKind::Regular | FileKind::Device => file_checksum(path.as_ref()),
        FileKind::Directory => directory_checksum(path.as_ref()),
        FileKind::Symlink => symlink_checksum(path.as_ref()),
        FileKind::Other => Err(anyhow!(
            "cannot checksum unknown fs path type {}",
            path.as_ref().display()
        )),
    }
}

/// Fixes the copy `target` of `orig` which has checksum `checksum`.
/// Returns `true` if some fixing was needed or `false` otherwise.
/// Returns an error if `orig` has changed since it has been checksummed
pub fn fix_path(
    orig: impl AsRef<Path>,
    target: impl AsRef<Path>,
    checksum: Checksum,
) -> anyhow::Result<bool> {
    match FileKind::of(orig.as_ref())
        .with_context(|| format!("stat({}) to fix", orig.as_ref().display()))?
    {
        FileKind::Regular | FileKind::Device => fix_file(orig.as_ref(), target.as_ref(), checksum),
        FileKind::Directory | FileKind::Symlink => {
            let c2 = checksum_path(target.as_ref())?;
            if c2 != checksum {
                // needs fixing
                let c3 = copy_path(orig.as_ref(), target.as_ref()).with_context(|| {
                    format!(
                        "copy symlink or directory {} to fix",
                        orig.as_ref().display()
                    )
                })?;
                if c3 != checksum {
                    Err(anyhow!("wrong checksum for {}", orig.as_ref().display()))
                } else {
                    Ok(true)
                }
            } else {
                Ok(false)
            }
        }
        FileKind::Other => Err(anyhow!(
            "cannot checksum unknown fs path type {}",
            orig.as_ref().display()
        )),
    }
}
