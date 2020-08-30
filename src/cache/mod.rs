use std::fs::{File, OpenOptions};
use std::path::Path;

pub mod vm;

pub trait CacheManager {
    /// Returns an error if this Cache Manager is bound to fail (missing privileges, missing
    /// runtime deps, ...) for paths below `path`.
    fn permission_check(&mut self, path: &Path) -> anyhow::Result<()>;
    /// Opens the spcified path with the specified open options. May add some options.
    fn open_no_cache(&self, options: &mut OpenOptions, path: &Path) -> std::io::Result<File> {
        options.open(path)
    }
    /// Ensures all files opened after this call below `path` and with `open_no_cache` will not
    /// read from a cache.
    fn drop_cache(&mut self, path: &Path) -> anyhow::Result<()>;
    /// Just for debugging purposes
    fn name(&self) -> &'static str;
}
