use super::CacheManager;
use crate::udev::{get_udisk_blockdev_for, underlying_device};
use anyhow::Context;
use dbus_udisks2::{Block, UDisks2};
use std::path::Path;
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
}

/// returns true if block is synctatically the parent of path.
fn looks_parent(block: &Block, path: &Path) -> bool {
    for i in block.mount_points.iter() {
        if path.starts_with(i) {
            return true;
        }
    }
    false
}

impl CacheManager for UmountCacheManager {
    fn permission_check(&mut self, path: &Path) -> anyhow::Result<()> {
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
        anyhow::ensure!(
            looks_parent(&block, path),
            "File system on block device {}, corresponding to sysfs {}, does not looks like it bears {}: mount points {:?}",
            block.preferred_device.display(),
            dev.syspath().display(),
            path.display(),
            &block.mount_points
        );
        self.0 = Some(Inner { udisks, fs: block });
        Ok(())
    }

    fn drop_cache(&mut self, path: &Path) -> anyhow::Result<()> {
        let inner = self.0.as_ref().ok_or_else(|| {
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
        let remounted_path = inner
            .udisks
            .mount(
                &inner.fs,
                /*interactive */ true,
                None,
                None,
                LONG_TIMEOUT,
            )
            .with_context(|| format!("Remounting {}", &inner.fs.preferred_device.display()))?;
        anyhow::ensure!(
            path.starts_with(&remounted_path),
            "File system on block device {} was not remounted on a parent of {} but {}",
            inner.fs.preferred_device.display(),
            path.display(),
            remounted_path.display()
        );
        std::fs::symlink_metadata(path.parent().expect("tried to unmount /")).with_context(
            || {
                format!(
                    "checking that the parent of {} still exists after remounting {}",
                    path.display(),
                    inner.fs.preferred_device.display(),
                )
            },
        )?;
        Ok(())
    }
    fn name(&self) -> &'static str {
        "UmountCacheManager"
    }
}
