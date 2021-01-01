use super::{CacheManager, Replacement};
use crate::udev::{
    ensure_mounted, get_udisk_blockdev_by_drive_and_size, get_udisk_blockdev_by_uuid,
    get_udisk_blockdev_for, reset_usb_hub, udisk_drives_for, underlying_device, usb_hub_for,
};
use crate::utils::{change_prefixes, get_mountpoint_in, FileKind, Unique};
use anyhow::Context;
use dbus_udisks2::{Drive, UDisks2};
use std::path::{Path, PathBuf};
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
    /// A file system, by uuid. There is also the mountpoint, but it's only to piggy back the info.
    Fs(String, PathBuf),
}

/// the content of UsbResetCacheManager after `permission_check` is called.
struct Inner {
    udisks: UDisks2,
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
                let mountpoint = match get_mountpoint_in(&block, path) {
                    None => anyhow::bail!(
                    "File system on block device {}, corresponding to sysfs {}, does not looks like it bears {}: mount points {:?}",
                    block.preferred_device.display(),
                    dev.syspath().display(),
                    path.display(),
                    &block.mount_points
                ),
                Some(x) => x.to_path_buf()
                };
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
                        Identifier::Fs(uuid, mountpoint)
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
            drives,
            usbhub,
            id,
        });
        Ok(())
    }

    fn drop_cache(&mut self, path: &Path) -> anyhow::Result<Option<Replacement>> {
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
        let new_path = match &inner.id {
            Identifier::Fs(uuid, mountpoint) => {
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
                if path.starts_with(&remounted_path) {
                    None
                } else {
                    let mut f = change_prefixes(mountpoint.as_path(), remounted_path.as_path());
                    Some(f(path))
                }
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
                if block.symlinks.iter().any(|x| x.as_path() == path) || path == block.device {
                    // the current path to the device file is still valid
                    None
                } else {
                    Some(block.device)
                }
            }
        };
        // this refreshes the members and checks that the currently detected mountpoint corresponds
        // to new_path
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
        "UsbResetCacheManager"
    }
}
