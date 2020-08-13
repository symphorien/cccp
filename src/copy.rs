use crate::checksum::{Checksum, Crc64Hasher};
use anyhow::Context;
use digest::Digest;
use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

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
