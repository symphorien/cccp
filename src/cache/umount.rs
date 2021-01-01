use super::{CacheManager, Replacement};
use crate::udev::{ensure_mounted, get_udisk_blockdev_for, underlying_device};
use crate::utils::{change_prefixes, get_mountpoint_in, FileKind};
use anyhow::Context;
use dbus_udisks2::{Block, UDisks2};
use std::path::{Path, PathBuf};
use std::time::Duration;

const LONG_TIMEOUT: Duration = Duration::from_secs(3600);

#[derive(Default)]
/// Drops the page cache of a file system by unmounting then remounting it with
/// udisks2.
pub struct UmountCacheManager(Option<Inner>);

/// the content of UmountCacheManager after `permission_check` is called.
struct Inner {
    udisks: UDisks2,
    fs: Block,
    mountpoint: PathBuf,
}

impl CacheManager for UmountCacheManager {
    fn permission_check(&mut self, path: &Path) -> anyhow::Result<()> {
        anyhow::ensure!(
            !matches!(FileKind::of_path(path), Ok(FileKind::Device)),
            "umount method can only handle files on a filesystem, not a block device {}",
            path.display()
        );
        let udisks = UDisks2::new().context("Connecting to udisks dbus interface")?;
        let dev = underlying_device(path)?;
        let block = get_udisk_blockdev_for(&udisks, &dev)?;
        anyhow::ensure!(
            block.has_fs(),
            "UDisks knows about no file system on block device {}, corresponding to sysfs {} and path {}",
            block.preferred_device.display(),
            dev.syspath().display(),
            path.display()
        );
        let mountpoint = match get_mountpoint_in(&block, path) {
            None => anyhow::bail!("File system on block device {}, corresponding to sysfs {}, does not looks like it bears {}: mount points {:?}",
            block.preferred_device.display(),
            dev.syspath().display(),
            path.display(),
            &block.mount_points
        ),
        Some(x) => x.to_path_buf(),
        };
        self.0 = Some(Inner {
            udisks,
            fs: block,
            mountpoint,
        });
        Ok(())
    }

    fn drop_cache(&mut self, path: &Path) -> anyhow::Result<Option<Replacement>> {
        let inner = self.0.as_mut().ok_or_else(|| {
            anyhow::anyhow!("tried to drop_cache on uninitialised UmountCacheManager")
        })?;
        inner
            .udisks
            .unmount(
                &inner.fs,
                /* interactive */ true,
                /* force */ false,
                LONG_TIMEOUT,
            )
            .with_context(|| format!("Unmounting {}", inner.fs.preferred_device.display()))?;
        let remounted_path = ensure_mounted(&mut inner.udisks, &inner.fs, LONG_TIMEOUT)
            .with_context(|| format!("Remounting {}", &inner.fs.preferred_device.display()))?;
        let new_path = if path.starts_with(&remounted_path) {
            None
        } else {
            let mut f = change_prefixes(inner.mountpoint.as_path(), remounted_path.as_path());
            Some(f(path))
        };
        inner.udisks.update().context("updating udisks")?;
        self.permission_check(match &new_path {
            None => path,
            Some(x) => x.as_path(),
        })?;
        Ok(new_path.map(|new_path| Replacement {
            before: path.to_path_buf(),
            after: new_path,
        }))
    }
    fn name(&self) -> &'static str {
        "UmountCacheManager"
    }
}
