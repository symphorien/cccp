use anyhow::Context;
use std::os::unix::fs::FileTypeExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

#[derive(PartialEq, Eq, Debug, Clone, Copy)]
pub enum FileKind {
    /// A regular file
    Regular,
    /// A directory
    Directory,
    /// A symbolic link
    Symlink,
    /// A block device
    // does someone really need to copy a file to a character device ?
    Device,
    /// Something else that we cannot handle.
    Other,
}

impl FileKind {
    /// Gets the kind of the file which has this metadata. No syscall is issued.
    pub fn of_metadata(metadata: &std::fs::Metadata) -> FileKind {
        let t = metadata.file_type();
        if t.is_file() {
            FileKind::Regular
        } else if t.is_dir() {
            FileKind::Directory
        } else if t.is_symlink() {
            FileKind::Symlink
        } else if t.is_block_device() {
            FileKind::Device
        } else {
            FileKind::Other
        }
    }

    /// Makes a syscall to get the file kind of this path.
    pub fn of_path(path: &Path) -> anyhow::Result<FileKind> {
        let meta = std::fs::symlink_metadata(path)
            .with_context(|| format!("stat {} to determine file type", path.display()))?;
        Ok(Self::of_metadata(&meta))
    }

    /// Makes a syscall to get the file kind of an open file.
    pub fn of_file(file: &std::fs::File) -> anyhow::Result<FileKind> {
        let meta = file
            .metadata()
            .with_context(|| format!("stat of open file {:?} to determine file type", file))?;
        Ok(Self::of_metadata(&meta))
    }
}

/// Returns without this file exists, without following symlinks
pub fn exists(path: &Path) -> anyhow::Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) => match e.kind() {
            std::io::ErrorKind::NotFound => Ok(false),
            _ => Err(e)
                .with_context(|| format!("stat({}) to determine if it exists", path.display()))?,
        },
    }
}

fn change_prefix(path: &Path, old_prefix_len: usize, new_prefix: &Path) -> PathBuf {
    let mut res = new_prefix.to_path_buf();
    let mut c = path.components();
    for _ in 0..old_prefix_len {
        c.next();
    }
    for component in c {
        res.push(component);
    }

    res
}

/// Replaces in place the prefix `old_prefix` of all paths in `paths` by `new_prefix`.
pub fn change_prefixes<'a>(
    old_prefix: &'a Path,
    new_prefix: &'a Path,
) -> Box<dyn for<'b> FnMut(&'b Path) -> PathBuf + 'a> {
    let old_prefix_len = old_prefix.components().count();

    #[cfg(any(test, debug))]
    let c: Vec<_> = old_prefix.components().collect();

    let f = move |path: &'_ Path| -> PathBuf {
        #[cfg(any(test, debug))]
        {
            assert_eq!(c, dbg!(path.components().take(c.len()).collect::<Vec<_>>()));
        }
        change_prefix(path, old_prefix_len, new_prefix)
    };
    Box::new(f)
}

/// returns the mountpoint of block which is synctatically the parent of path.
pub fn get_mountpoint_in<'a, 'b>(
    block: &'a dbus_udisks2::Block,
    path: &'b Path,
) -> Option<&'a Path> {
    for i in block.mount_points.iter() {
        if path.starts_with(i) {
            return Some(i);
        }
    }
    return None;
}

/// Returns the size of the file as needed for the progress bar.
/// This is 0 for symlinks and directories.
pub fn copy_size(meta: &std::fs::Metadata) -> u64 {
    match FileKind::of_metadata(meta) {
        FileKind::Symlink | FileKind::Directory | FileKind::Other => 0,
        FileKind::Regular | FileKind::Device => meta.size(),
    }
}

/// Return type for `get_unique`.
pub enum Unique<T> {
    /// The iterator had no element
    Zero,
    /// The iterator had this one element.
    One(T),
    /// The iterator had several elements.
    Several,
}

/// Returns the only element of this iterator, or indicate whether there was
/// zero or more than one elements.
pub fn get_unique<I, T>(mut iter: I) -> Unique<T>
where
    I: Iterator<Item = T>,
{
    match iter.next() {
        None => Unique::Zero,
        Some(x) => match iter.next() {
            None => Unique::One(x),
            Some(_) => Unique::Several,
        },
    }
}

#[cfg(test)]
mod test {
    use super::*;
    fn test_change_prefix(
        path: &'static str,
        old: &'static str,
        new: &'static str,
        expected: impl Into<Option<&'static str>>,
    ) {
        let old_prefix = PathBuf::from(old);
        let new_prefix = PathBuf::from(new);
        let mut f = change_prefixes(&old_prefix, &new_prefix);
        let res = f(PathBuf::from(path).as_path());
        let expected_str: Option<&'static str> = expected.into();
        match expected_str {
            Some(x) => assert_eq!(res, PathBuf::from(x)),
            None => {
                dbg!(path, res);
            }
        }
    }

    #[test]
    fn test_change_prefixes() {
        test_change_prefix("/", "/", "/b", "/b");
        test_change_prefix("/a", "/a", "/b", "/b");
        test_change_prefix("/a", "/", "/b", "/b/a");
        test_change_prefix("/a/c", "/a", "/b", "/b/c");
    }

    #[test]
    #[should_panic]
    fn test_change_prefixes_wrong_prefix() {
        test_change_prefix("/a", "/b", "/c", None)
    }
}
