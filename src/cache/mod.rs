use std::fs::{File, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

pub mod directio;
pub mod vm;
pub mod umount;

pub trait CacheManager {
    /// Returns an error if this Cache Manager is bound to fail (missing privileges, missing
    /// runtime deps, ...) for paths below `path`.
    fn permission_check(&mut self, path: &Path) -> anyhow::Result<()>;
    /// Opens the spcified path with the specified open options.
    /// The custom_flags must be specified here, if set on the options, they will be ignored.
    fn open_no_cache(
        &self,
        options: &mut OpenOptions,
        custom_flags: i32,
        path: &Path,
    ) -> std::io::Result<File> {
        options.custom_flags(custom_flags).open(path)
    }
    /// Ensures all files opened after this call below `path` and with `open_no_cache` will not
    /// read from a cache.
    fn drop_cache(&mut self, path: &Path) -> anyhow::Result<()>;
    /// Just for debugging purposes
    fn name(&self) -> &'static str;
}
