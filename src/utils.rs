use anyhow::Context;
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};

#[derive(PartialEq, Eq, Debug, Clone, Copy)]
pub enum FileKind {
    Regular,
    Directory,
    Symlink,
    Device,
    Other,
}

impl FileKind {
    pub fn of_metadata(metadata: std::fs::Metadata) -> FileKind {
        let t = metadata.file_type();
        if t.is_file() {
            FileKind::Regular
        } else if t.is_dir() {
            FileKind::Directory
        } else if t.is_symlink() {
            FileKind::Symlink
        } else if t.is_block_device() || t.is_char_device() {
            FileKind::Device
        } else {
            FileKind::Other
        }
    }

    pub fn of_path(path: &Path) -> anyhow::Result<FileKind> {
        let meta = std::fs::symlink_metadata(path)
            .with_context(|| format!("stat {} to determine file type", path.display()))?;
        Ok(Self::of_metadata(meta))
    }

    pub fn of_file(file: &std::fs::File) -> anyhow::Result<FileKind> {
        let meta = file
            .metadata()
            .with_context(|| format!("stat of open file {:?} to determine file type", file))?;
        Ok(Self::of_metadata(meta))
    }
}

/// Returns without this file exists, without following metadata
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

fn change_prefix(path: &Path, old_prefix_len: usize, new_prefix: &PathBuf) -> PathBuf {
    let mut res = new_prefix.clone();
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
pub fn change_prefixes<T: AsRef<Path>>(
    old_prefix: &Path,
    new_prefix: &PathBuf,
    paths: &[T],
) -> Vec<PathBuf> {
    let old_prefix_len = old_prefix.components().count();

    #[cfg(any(test, debug))]
    let c: Vec<_> = old_prefix.components().collect();

    let mut res = Vec::with_capacity(paths.len());
    for path in paths {
        #[cfg(any(test, debug))]
        {
            assert_eq!(
                c,
                dbg!(path.as_ref().components().take(c.len()).collect::<Vec<_>>())
            );
        }
        let new = change_prefix(path.as_ref(), old_prefix_len, new_prefix);
        res.push(new);
    }
    res
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
        let paths = vec![PathBuf::from(path)];
        let old_prefix = PathBuf::from(old);
        let new_prefix = PathBuf::from(new);
        let res = change_prefixes(&old_prefix, &new_prefix, &paths);
        let expected_str: Option<&'static str> = expected.into();
        match expected_str {
            Some(x) => assert_eq!(res, vec![PathBuf::from(x)]),
            None => {
                dbg!(paths);
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
