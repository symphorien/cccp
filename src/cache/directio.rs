use super::{CacheManager, Replacement};
use crate::utils::FileKind;

use anyhow::Context;

use std::fs::{File, OpenOptions};
use std::io::ErrorKind;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use nix::errno::Errno;

#[derive(Default, Debug)]
pub struct DirectIOCacheManager {}

impl CacheManager for DirectIOCacheManager {
    fn permission_check(&mut self, path: &Path) -> anyhow::Result<()> {
        fn test_file(x: &DirectIOCacheManager, path: &Path, create: bool) -> anyhow::Result<()> {
            let fd = x.open_no_cache(OpenOptions::new().append(true).create(create), 0, path);
            match fd {
                Ok(fd) => {
                    drop(fd);
                    Ok(())
                }
                Err(io) => {
                    let errno = io.raw_os_error().map(Errno::from_i32);
                    let e = Err(io).with_context(|| format!("open({}, O_DIRECT)", path.display()));
                    match errno {
                        Some(Errno::EINVAL) => {
                            // fs does not support DIRECT_IO
                            e.context("Filesystem does not support direct IO")?
                        }
                        _ => e?,
                    }
                }
            }
        }
        match FileKind::of_path(path) {
            Ok(FileKind::Symlink) | Ok(FileKind::Other) => Ok(()),
            Ok(FileKind::Device) | Ok(FileKind::Regular) => test_file(self, path, false),
            Ok(FileKind::Directory) => {
                let tmp_dir = tempfile::TempDir::new_in(path).with_context(|| {
                    format!(
                        "creating a temporary directory in {} to test opening with O_DIRECT",
                        path.display()
                    )
                })?;
                let mut test_file_path = tmp_dir.path().to_path_buf();
                test_file_path.push("test");
                let res = test_file(self, &test_file_path, true);
                tmp_dir.close().with_context(|| {
                    format!(
                        "removing a temporary directory in {} to test opening with O_DIRECT",
                        path.display()
                    )
                })?;
                res
            }
            Err(e) => {
                // if the file did not exist, that's fine. Otherwise, propagate.
                match e.downcast::<std::io::Error>() {
                    Ok(e) => {
                        match e.kind() {
                            ErrorKind::NotFound => {
                                let res = test_file(self, path, true);
                                // this may have created a file
                                match std::fs::remove_file(path) {
                        Ok(()) => (),
                        Err(e) => match e.kind() {
                            ErrorKind::NotFound => (),
                            _ => Err(e).with_context(|| format!("removing temporary file {} after open(O_DIRECT) test", path.display()))?,
                        }
                    }
                                res?;
                                Ok(())
                            }
                            _ => Err(e).with_context(|| {
                                format!("stat({}) to test opening with O_DIRECT", path.display())
                            })?,
                        }
                    }
                    Err(e) => Err(e).with_context(|| {
                        format!("stat({}) to test opening with O_DIRECT", path.display())
                    })?,
                }
            }
        }
    }
    fn open_no_cache(
        &self,
        options: &mut OpenOptions,
        custom_flags: i32,
        path: &Path,
    ) -> std::io::Result<File> {
        options
            .custom_flags(libc::O_DIRECT | custom_flags)
            .open(path)
    }
    fn drop_cache(&mut self, _path: &Path) -> anyhow::Result<Option<Replacement>> {
        Ok(None)
    }
    fn name(&self) -> &'static str {
        "DirectIOCacheManager"
    }
}
