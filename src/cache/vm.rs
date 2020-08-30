use super::CacheManager;
use crate::utils::FileKind;
use anyhow::anyhow;
use anyhow::Context;
use std::io::prelude::*;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::{FromRawFd, IntoRawFd};
use std::path::Path;

const VM_DROP_CACHES: &'static str = "/proc/sys/vm/drop_caches";

fn syncfs<T: IntoRawFd + FromRawFd>(f: T) -> anyhow::Result<()> {
    let fd = f.into_raw_fd();
    let res = nc::syncfs(fd).map_err(|errno| anyhow!("syncfs: errno={}", errno));
    // close the file, even if synfs failed.
    drop(unsafe { T::from_raw_fd(fd) });
    res?;
    Ok(())
}

fn global_drop_cache(file: &Path) -> anyhow::Result<()> {
    // first sync
    match FileKind::of_path(file)
        .with_context(|| format!("stat {} to drop cache", file.display()))?
    {
        FileKind::Directory | FileKind::Regular => {
            let f = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(file)
                .with_context(|| format!("open({}) for sync to drop cache", file.display()))?;
            syncfs(f).with_context(|| format!("syncfs({}) to drop cache", file.display()))?;
        }
        FileKind::Symlink => {
            // syncfs does not work on symlinks (how do I get a filedesc for a symlink ?) so let's
            // to syncfs on the parent. The parent always exists because / cannot be a symlink,
            // right ?
            let parent = match file.parent() {
                Some(x) => x,
                None => anyhow::bail!("Cannot syncfs(parent of {file}) because {file} is a symlink and has no parent. Is / a symlink ?", file = file.display()),
            };
            return global_drop_cache(parent);
        }
        FileKind::Device => {
            let f = std::fs::File::open(file)
                .with_context(|| format!("open {} to drop cache", file.display()))?;
            f.sync_all()
                .with_context(|| format!("fsync({}) to drop cache", file.display()))?;
        }
        FileKind::Other => {
            return Err(anyhow!(
                "Cannot sync {} to drop cache, wrong file type",
                file.display()
            ))
        }
    }
    // second drop cache
    // tests need to skip this test, with an environment variable
    if std::env::var("CCCP_NO_ROOT").is_err() {
        let mut f = std::fs::File::create(VM_DROP_CACHES)
            .with_context(|| format!("open {} to drop cache", VM_DROP_CACHES))?;
        f.write_all(b"3")
            .with_context(|| format!("write 3 to {} to drop cache", VM_DROP_CACHES))?;
    }
    Ok(())
}

#[derive(Default, Debug)]
pub struct PageCacheManager {}
impl CacheManager for PageCacheManager {
    fn permission_check(&mut self, _path: &Path) -> anyhow::Result<()> {
        if nix::unistd::getuid().is_root() || std::env::var("CCCP_NO_ROOT").is_ok() {
            Ok(())
        } else {
            anyhow::bail!("PageCacheManager needs root privileges")
        }
    }
    fn drop_cache(&mut self, path: &Path) -> anyhow::Result<()> {
        global_drop_cache(path)
    }
    fn name(&self) -> &'static str {
        "PageCacheManager"
    }
}
