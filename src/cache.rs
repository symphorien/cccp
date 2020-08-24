use crate::utils::FileKind;
use anyhow::anyhow;
use anyhow::Context;
use std::io::prelude::*;
use std::os::unix::io::{FromRawFd, IntoRawFd};
use std::path::Path;

const VM_DROP_CACHES: &'static str = "/proc/sys/vm/drop_caches";

fn syncfs<T: IntoRawFd + FromRawFd>(f: T) -> anyhow::Result<()> {
    let fd = f.into_raw_fd();
    nc::syncfs(fd).map_err(|errno| anyhow!("syncfs: errno={}", errno))?;
    drop(unsafe { T::from_raw_fd(fd) });
    Ok(())
}

pub fn global_drop_cache(file: impl AsRef<Path>) -> anyhow::Result<()> {
    // first sync
    match FileKind::of(file.as_ref())
        .with_context(|| format!("stat {} to drop cache", file.as_ref().display()))?
    {
        FileKind::Directory | FileKind::Symlink | FileKind::Regular => {
            let f = std::fs::OpenOptions::new()
                .read(true)
                .open(file.as_ref())
                .with_context(|| {
                    format!(
                        "open({}) for sync to drop cache",
                        file.as_ref().display()
                    )
                })?;
            syncfs(f)
                .with_context(|| format!("syncfs({}) to drop cache", file.as_ref().display()))?;
        }
        FileKind::Device => {
            let f = std::fs::File::open(file.as_ref())
                .with_context(|| format!("open {} to drop cache", file.as_ref().display()))?;
            f.sync_all()
                .with_context(|| format!("fsync({}) to drop cache", file.as_ref().display()))?;
        }
        FileKind::Other => {
            return Err(anyhow!(
                "Cannot sync {} to drop cache, wrong file type",
                file.as_ref().display()
            ))
        }
    }
    // second drop cache
    let mut f = std::fs::File::create(VM_DROP_CACHES)
        .with_context(|| format!("open {} to drop cache", VM_DROP_CACHES))?;
    f.write_all(b"3")
        .with_context(|| format!("write 3 to {} to drop cache", VM_DROP_CACHES))?;
    Ok(())
}
