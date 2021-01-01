use super::CacheManager;
use crate::udev::{
    ensure_mounted, get_udisk_blockdev_by_drive_and_size, get_udisk_blockdev_by_uuid,
    get_udisk_blockdev_for, reset_usb_hub, udisk_drives_for, underlying_device, usb_hub_for,
};
use crate::utils::{looks_parent, FileKind, Unique};
use anyhow::Context;
use dbus_udisks2::{Block, Drive, UDisks2};
use std::path::Path;
use std::time::Duration;
use udev::Device;

const LONG_TIMEOUT: Duration = Duration::from_secs(3600);

#[derive(Default)]
/// Resets the usb bus bearing the drive.
pub struct UsbResetCacheManager(Option<Inner>);

/// Enough info to find what we are copying to after usb reset
enum Identifier {
    /// A block device, by device dbus path and size. Using the size is pretty hacky, sorry
    BlockDevice(String, u64),
    /// A file system, by uuid
    Fs(String),
}

/// the content of UsbResetCacheManager after `permission_check` is called.
struct Inner {
    udisks: UDisks2,
    block: Block,
    drives: Vec<Drive>,
    usbhub: Device,
    id: Identifier,
}

impl CacheManager for UsbResetCacheManager {
    fn permission_check(&mut self, path: &Path) -> anyhow::Result<()> {
        anyhow::ensure!(
            nix::unistd::getuid().is_root(),
            "USB reset IOCTL method requires root privileges"
        );

        let udisks = UDisks2::new().context("Connecting to udisks dbus interface")?;
        let dev = underlying_device(path)?;
        let block = get_udisk_blockdev_for(&udisks, &dev)?;
        let id = match FileKind::of_path(path) {
            Ok(FileKind::Device) => {
                let b = get_udisk_blockdev_by_drive_and_size(&udisks, &block.drive, block.size);
                match b {
                    Unique::Zero => {
                        anyhow::bail!("{} disappeared", block.preferred_device.display())
                    }
                    Unique::Several => anyhow::bail!(
                        "Several partitions on {} have the size {}",
                        block.drive,
                        block.size
                    ),
                    Unique::One(x) => {
                        anyhow::ensure!(
                            x.path == block.path,
                            "{} changed path to {}",
                            block.path,
                            x.path
                        );
                        Identifier::BlockDevice(block.drive.clone(), block.size)
                    }
                }
            }
            _ => {
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
                let uuid = match block.id_uuid.clone() {
                    None => anyhow::bail!(
                        "Attempting to write to a filesystem {} without uuid",
                        block.preferred_device.display()
                    ),
                    Some(x) => x,
                };
                match get_udisk_blockdev_by_uuid(&udisks, &uuid) {
                    Unique::Zero => anyhow::bail!("FS with UUID {} disappeared", uuid),
                    Unique::Several => anyhow::bail!("Several fs with UUID {}", uuid),
                    Unique::One(x) => {
                        anyhow::ensure!(
                            x.path == block.path,
                            "{} changed path to {}",
                            block.path,
                            x.path
                        );
                        Identifier::Fs(uuid)
                    }
                }
            }
        };
        let drives = udisk_drives_for(&udisks, &block).with_context(|| {
            format!(
                "Failed to enumerate drives corresponding to {} (for {})",
                block.preferred_device.display(),
                path.display()
            )
        })?;
        anyhow::ensure!(
            !drives.is_empty(),
            "Found 0 drive for {} (corresponding to {})",
            block.preferred_device.display(),
            path.display()
        );
        for d in drives.iter() {
            if !d.ejectable {
                anyhow::bail!("Drive {} is not ejectable according to udisks", &d.id);
            }
        }
        let usbhub = usb_hub_for(&dev).with_context(|| {
            format!(
                "Device {} corresponding to {} is not plugged in by usb",
                dev.syspath().display(),
                path.display()
            )
        })?;
        reset_usb_hub(&usbhub, /* dryrun */true).with_context(|| format!("Cannot access usb device file for {} to issue usbreset ioctl. Missing permissions ?", usbhub.syspath().display()))?;
        self.0 = Some(Inner {
            udisks,
            block,
            drives,
            usbhub,
            id,
        });
        Ok(())
    }

    fn drop_cache(&mut self, path: &Path) -> anyhow::Result<()> {
        let inner = self.0.as_mut().ok_or_else(|| {
            anyhow::anyhow!("tried to drop_cache on uninitialised UmountCacheManager")
        })?;
        // unmount all fs on these drives
        for b in inner.udisks.get_blocks() {
            if !b.mount_points.is_empty()
                && inner
                    .drives
                    .iter()
                    .map(|d| &d.path)
                    .any(|path| path == &b.drive)
            {
                inner
                    .udisks
                    .unmount(
                        &b,
                        /*interative*/ true,
                        /*force*/ false,
                        LONG_TIMEOUT,
                    )
                    .with_context(|| format!("Unmounting {}", b.preferred_device.display()))?;
            }
        }

        // eject the drives
        for d in inner.drives.iter() {
            inner
                .udisks
                .eject(d, /* interactive */ true, LONG_TIMEOUT)
                .with_context(|| format!("Ejecting {}", &d.id))?;
        }
        // reset the bus
        reset_usb_hub(&inner.usbhub, /* dryrun */ false).with_context(|| {
            format!(
                "Cannot reset usb hub for {}",
                inner.usbhub.syspath().display()
            )
        })?;
        // ensure everything is ready
        match &inner.id {
            Identifier::Fs(uuid) => {
                let mut found = None;
                for _ in 0..60 {
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    inner.udisks.update().context("Updating Udisks2")?;
                    match get_udisk_blockdev_by_uuid(&inner.udisks, &uuid) {
                        Unique::Zero => (),
                        Unique::Several => anyhow::bail!("Several FS with uuid {}", uuid),
                        Unique::One(x) => {
                            found = Some(x);
                            break;
                        }
                    }
                }
                let block = match found {
                    None => anyhow::bail!(
                        "Timeout reached waiting for fs with uuid {} to appear",
                        uuid
                    ),
                    Some(x) => x,
                };
                // we need to remount the fs
                let remounted_path = ensure_mounted(&mut inner.udisks, &block, LONG_TIMEOUT)
                    .with_context(|| format!("Remounting {}", &block.preferred_device.display()))?;
                anyhow::ensure!(
                    path.starts_with(&remounted_path),
                    "File system on block device {} was not remounted on a parent of {} but {}",
                    inner.block.preferred_device.display(),
                    path.display(),
                    remounted_path.display()
                );
                std::fs::symlink_metadata(path.parent().expect("tried to unmount /"))
                    .with_context(|| {
                        format!(
                            "checking that the parent of {} still exists after remounting {}",
                            path.display(),
                            block.preferred_device.display(),
                        )
                    })?;
            }
            Identifier::BlockDevice(drive, size) => {
                let mut found = None;
                for _ in 0..60 {
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    inner.udisks.update().context("Updating Udisks2")?;
                    match get_udisk_blockdev_by_drive_and_size(&inner.udisks, &drive, *size) {
                        Unique::Zero => (),
                        Unique::Several => anyhow::bail!(
                            "Several block devices on drive {} with size {}",
                            drive,
                            size
                        ),
                        Unique::One(x) => {
                            found = Some(x);
                            break;
                        }
                    }
                }
                let block = match found {
                    None => anyhow::bail!("Timeout reached waiting for block device on drive {} with size {} to appear", drive, size),
                    Some(x) => x
                };
                // just check that the device file still exists and points to this device
                anyhow::ensure!(
                    block.symlinks.iter().any(|x| x.as_path() == path) || path == block.device,
                    "{} reappeared at {} and {:?}",
                    path.display(),
                    block.device.display(),
                    block.symlinks
                );
            }
        }
        // FIXME: update the fields of inner.
        Ok(())
    }

    fn name(&self) -> &'static str {
        "UsbResetCacheManager"
    }
}
