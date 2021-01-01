use crate::utils::FileKind;
use crate::utils::{get_unique, Unique};
use anyhow::Context;
use dbus_udisks2::{Block, Drive, MountError, UDisks2};
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::io::{FromRawFd, IntoRawFd};
use std::path::{Path, PathBuf};
use udev::Device;

/// Returns the device number of the device bearing the specified path.
/// Either this path, or its parent must exist.
fn underlying_device_number(path: &Path) -> anyhow::Result<u64> {
    let meta = match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // maybe the path is to be created, so try with the parent.
            let p = path.parent().unwrap_or(path);
            std::fs::symlink_metadata(p).with_context(|| {
                format!(
                    "stat({}) for device number bearing {}",
                    p.display(),
                    path.display()
                )
            })?
        }
        x => x.with_context(|| format!("stat({}) for device number", path.display()))?,
    };
    Ok(match FileKind::of_metadata(&meta) {
        FileKind::Device => meta.rdev(),
        _ => meta.dev(),
    })
}

/// Returns the udev Device bearing the specified path.
/// Either this path, or its parent must exist.
pub fn underlying_device(path: &Path) -> anyhow::Result<Device> {
    let number = underlying_device_number(path)?;
    let device_path = format!(
        "/sys/dev/block/{}:{}",
        unsafe { libc::major(number) },
        unsafe { libc::minor(number) }
    );
    let device = Device::from_syspath(device_path.as_ref()).with_context(|| {
        format!(
            "Opening {} underlying device of {}",
            &device_path,
            path.display()
        )
    })?;
    Ok(device)
}

/// Returns the UDisks2 block device corresponding to this udev Device.
pub fn get_udisk_blockdev_for(udisks: &UDisks2, dev: &Device) -> anyhow::Result<Block> {
    let node = match dev.devnode() {
        None => anyhow::bail!(
            "No device node corresponding to {}",
            dev.syspath().display()
        ),
        Some(x) => x,
    };
    match udisks
        .get_blocks()
        .find(|b| b.device.as_path() == node || b.symlinks.iter().any(|s| s.as_path() == node))
    {
        None => anyhow::bail!(
            "Device {} (for {}) is not known to UDisks2",
            node.display(),
            dev.syspath().display()
        ),
        Some(t) => Ok(t),
    }
}

/// Returns a UDisks2 block device by filesystem UUID
pub fn get_udisk_blockdev_by_uuid(udisks: &UDisks2, uuid: &str) -> Unique<Block> {
    get_unique(
        udisks
            .get_blocks()
            .filter(|b| b.id_uuid.as_ref().map(|x| -> &str { &x }) == Some(uuid)),
    )
}

/// Returns a UDisks2 block device by drive dbus path and size
pub fn get_udisk_blockdev_by_drive_and_size(
    udisks: &UDisks2,
    drive: &str,
    size: u64,
) -> Unique<Block> {
    get_unique(
        udisks
            .get_blocks()
            .filter(|b| b.drive == drive && b.size == size),
    )
}

/// Like Udisks2.mount, but does not fail if the fs is already mounted.
pub fn ensure_mounted(
    udisks: &mut UDisks2,
    block: &Block,
    timeout: std::time::Duration,
) -> anyhow::Result<PathBuf> {
    match udisks.mount(block, /* interactive */ true, None, None, timeout) {
        Err(MountError::DBUS(d)) => {
            if d.name() == Some("org.freedesktop.UDisks2.Error.AlreadyMounted") {
                udisks
                    .update()
                    .context("updating Udisks2 because already mounted")?;
                let new = match udisks.get_block(&block.path) {
                    None => anyhow::bail!(
                        "Udisks2 reported {} and then the block device disappeared",
                        d.message().unwrap_or("already mounted")
                    ),
                    Some(n) => n,
                };
                anyhow::ensure!(
                    !new.mount_points.is_empty(),
                    "Udisks2 reported {} but no mountpoint found",
                    d.message().unwrap_or("already mounted")
                );
                Ok(new.mount_points[0].clone())
            } else {
                Err(MountError::DBUS(d).into())
            }
        }
        x => Ok(x?),
    }
}

pub fn udisk_drives_for(udisks: &UDisks2, fs: &Block) -> anyhow::Result<Vec<Drive>> {
    let drive = match udisks.get_drive(&fs.drive) {
        None => anyhow::bail!("Could not find drive for {}", fs.device.display()),
        Some(x) => x,
    };
    let group = &drive.sibling_id;
    if group.len() == 0 {
        Ok(vec![drive])
    } else {
        let res: Vec<Drive> = udisks
            .get_drives()
            .filter(|d| &d.sibling_id == group)
            .collect();
        assert!(res.iter().find(|x| &x.id == &drive.id).is_some());
        Ok(res)
    }
}

/// Finds the corresponding usb hub for this device
// Method: first device with driver and subsystem equal to usb
pub fn usb_hub_for(dev: &Device) -> anyhow::Result<Device> {
    let mut dev = dev.clone();
    while let Some(p) = dev.parent() {
        if (
            p.subsystem().map(OsStrExt::as_bytes),
            p.driver().map(OsStrExt::as_bytes),
        ) == (Some(b"usb"), Some(b"usb"))
        {
            return Ok(p);
        }
        dev = p;
    }
    anyhow::bail!("{} is not on a usb hub", dev.syspath().display());
}

// defined in include/uapi/linux/usbdevice_fs.h
nix::ioctl_none!(usbreset, b'U', 20);

fn leftpad(s: &[u8]) -> anyhow::Result<OsString> {
    let mut res = [b'0'; 3];
    let len = s.len();
    anyhow::ensure!(
        len <= 3,
        "more than 3 digits: {}",
        String::from_utf8_lossy(s)
    );
    res[(3 - len)..].copy_from_slice(s);
    Ok(OsStr::from_bytes(&res).into())
}

#[test]
fn test_leftpad() {
    assert_eq!(
        leftpad(b"").map_err(|_| ()),
        Ok(OsStr::from_bytes(b"000").into())
    );
    assert_eq!(
        leftpad(b"1").map_err(|_| ()),
        Ok(OsStr::from_bytes(b"001").into())
    );
    assert_eq!(
        leftpad(b"12").map_err(|_| ()),
        Ok(OsStr::from_bytes(b"012").into())
    );
    assert_eq!(
        leftpad(b"123").map_err(|_| ()),
        Ok(OsStr::from_bytes(b"123").into())
    );
    assert!(leftpad(b"1234").is_err());
}

/// Resets a usb device, source: https://marc.info/?l=linux-usb-users&m=116827193506484
/// If dryrun is true, only performs permission checks.
pub fn reset_usb_hub(dev: &Device, dryrun: bool) -> anyhow::Result<()> {
    let (busnum, devnum) = match (dev.attribute_value("busnum"), dev.attribute_value("devnum")) {
        (Some(x), Some(y)) => (x, y),
        _ => anyhow::bail!("Device {} is missing busnum or devnum attribute"),
    };
    let mut buspath = PathBuf::from("/dev/bus/usb");
    buspath.push(leftpad(busnum.as_bytes()).context("bus number")?);
    buspath.push(leftpad(devnum.as_bytes()).context("dev number")?);
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(dbg!(&buspath))
        .with_context(|| format!("Opening usb device {} for reset ioctl", buspath.display()))?;
    if !dryrun {
        let fd = file.into_raw_fd();
        let res = unsafe { usbreset(fd) };
        drop(unsafe { std::fs::File::from_raw_fd(fd) });
        let _ = res.with_context(|| format!("ioctl({}, USBDEVFS_RESET, 0)", buspath.display()))?;
    }
    Ok(())
}
