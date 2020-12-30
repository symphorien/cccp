use crate::utils::FileKind;
use anyhow::Context;
use dbus_udisks2::{Block, UDisks2};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use udev::Device;
// use std::os::unix::io::{FromRawFd, IntoRawFd};

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

/*
fn udisk_drives_for(udisks: &UDisks2, fs: &Block) -> anyhow::Result<Vec<Drive>> {
    let drive = match udisks.get_drive(&fs.drive) {
        None => anyhow::bail!("Could not find drive for {}", fs.device.display()),
        Some(x) => x
    };
    let group = &drive.sibling_id;
    if group.len() == 0 {
        Ok(vec![drive])
    } else {
        let res: Vec<Drive> = udisks.get_drives().filter(|d| &d.sibling_id == group).collect();
        assert!(res.iter().find(|x| &x.id == &drive.id).is_some());
        Ok(res)
    }
}

fn usb_hub_for(dev: &Device) -> anyhow::Result<Device> {
    match dev.parent_with_subsystem_devtype("usb", "usb") {
        Ok(Some(x)) => Ok(x),
        Err(e) => Err(e)?,
        Ok(None) => anyhow::bail!("{} is not on a usb hub", dev.syspath().display())
    }
}


// defined in include/uapi/linux/usbdevice_fs.h
nix::ioctl_none!(usbreset, b'U', 20);

/// Resets a usb device, source: https://marc.info/?l=linux-usb-users&m=116827193506484
fn reset_usb_hub(dev: &Device) -> anyhow::Result<()> {
    let (busnum, devnum) = match (dev.attribute_value("busnum"), dev.attribute_value("devnum")) {
        (Some(x), Some(y)) => (x, y),
        _ => anyhow::bail!("Device {} is missing busnum or devnum attribute")
    };
    let mut buspath = PathBuf::from("/dev/bus/usb");
    buspath.push(busnum);
    buspath.push(devnum);
    let file = std::fs::File::create(&buspath).with_context(|| format!("Opening usb device {} for reset ioctl", buspath.display()))?;
    let fd = file.into_raw_fd();
    let res = unsafe{usbreset(fd)};
    drop(unsafe {std::fs::File::from_raw_fd(fd)});
    let _  = res.with_context(|| format!("ioctl({}, USBDEVFS_RESET, 0)", buspath.display()))?;
    Ok(())
}
*/
