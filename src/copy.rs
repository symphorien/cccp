use crate::checksum::{Checksum, Crc64Hasher};
use anyhow::Context;
use digest::Digest;
use std::fs::File;
use std::io::prelude::*;
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

use anyhow::anyhow;

/// Copies a file to another and computes the checksum
pub fn copy_file(file: impl AsRef<Path>, target: impl AsRef<Path>) -> anyhow::Result<Checksum> {
    let mut crc = Crc64Hasher::default();
    let mut orig_fd = File::open(file.as_ref())
        .with_context(|| format!("Failed to open {} for copy input", file.as_ref().display()))?;
    let meta = orig_fd
        .metadata()
        .with_context(|| format!("Failed to stat {} to copy mode", file.as_ref().display()))?;
    let mode = meta.mode();
    let mut target_fd = std::fs::OpenOptions::new().write(true).create(true).mode(mode).open(target.as_ref()).with_context(|| {
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

pub fn fix_file(file: impl AsRef<Path>, target: impl AsRef<Path>, checksum: &mut Option<Checksum>) -> anyhow::Result<bool> {
    let mut changed = false;
    let mut crc = Crc64Hasher::default();
    let mut orig_fd = File::open(file.as_ref())
        .with_context(|| format!("Failed to open {} as fix input", file.as_ref().display()))?;
    let mut target_fd = std::fs::OpenOptions::new().read(true).write(true).open(target.as_ref()).with_context(|| {
        format!(
            "Failed to open {} for fixing",
            target.as_ref().display()
        )
    })?;
    let mut orig = [0; 4096];
    let mut actual = [0; 4096];
    let mut offset = 0u64;
    loop {
        // invariant: both fd are at offset `offset` and identical up to there.
        let mut append = false;
        let n_orig = orig_fd.read(&mut orig)
            .with_context(|| format!("Reading from {} for comparing", file.as_ref().display()))?;
        if n_orig == 0 {
            let n_read = target_fd.read(&mut actual[..1])
                .with_context(|| format!("Reading from {} for comparing", target.as_ref().display()))?;
            if n_read != 0 {
                // target file is longer
                target_fd.set_len(offset)
                .with_context(|| format!("Truncating {}", target.as_ref().display()))?;
                changed = true;
            }
            break;
        }
        let mut n_actual = 0;
        while n_actual < n_orig {
            let n_read = target_fd
                .read(&mut actual[n_actual..n_orig])
                .with_context(|| format!("Reading from {} for comparing", target.as_ref().display()))?;
            n_actual += n_read;
            if n_read == 0 {
                // orig file is longer
                append = true;
                break;
            };
        }
        let data = &orig[..n_orig];
        crc.update(data);
        if append || data != &actual[..n_orig] {
            if !changed {
                println!("fixing {}", target.as_ref().display());
            }
            changed = true;
            target_fd.seek(std::io::SeekFrom::Start(offset))
                .with_context(|| format!("seeking in {} for fixing output", target.as_ref().display()))?;
            target_fd
                .write_all(data)
                .with_context(|| format!("writing to {} for fixing output", target.as_ref().display()))?;
        }
        offset += n_orig as u64;
    }
    match checksum {
        None => *checksum = Some(crc.into()),
        Some(c) => {
            if *c != crc.into() {
                return Err(anyhow!("Bad checksum for file {}", file.as_ref().display()));
            }
        }
    }
    Ok(changed)
}

