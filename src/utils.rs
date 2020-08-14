use anyhow::Context;
use std::os::unix::fs::FileTypeExt;
use std::path::Path;

pub enum FileKind {
    Regular,
    Directory,
    Symlink,
    Device,
    Other,
}

impl FileKind {
    pub fn of(file: impl AsRef<Path>) -> anyhow::Result<FileKind> {
        let meta = std::fs::metadata(file.as_ref())
            .with_context(|| format!("stat {} to determine file type", file.as_ref().display()))?;
        let t = meta.file_type();
        Ok(if t.is_file() {
            FileKind::Regular
        } else if t.is_dir() {
            FileKind::Directory
        } else if t.is_symlink() {
            FileKind::Symlink
        } else if t.is_block_device() || t.is_char_device() {
            FileKind::Device
        } else {
            FileKind::Other
        })
    }
}
