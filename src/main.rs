mod cache;
mod checksum;
mod copy;
mod progress;
mod udev;
mod utils;

use crate::cache::CacheManager;
use crate::progress::Progress;
use crate::utils::FileKind;
use anyhow::Context;
use checksum::Checksum;
use clap::arg_enum;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use structopt::StructOpt;

#[derive(Debug, PartialEq, Eq, Hash, Clone)]
struct Obligation {
    source: PathBuf,
    dest: PathBuf,
    checksum: Checksum,
    size: u64,
}

fn first_copy(
    cache_manager: &dyn CacheManager,
    progress: &mut Progress,
    orig: &Path,
    target: &PathBuf,
) -> anyhow::Result<HashSet<Obligation>> {
    let mut orig_paths = vec![];
    let meta = std::fs::symlink_metadata(orig)
        .with_context(|| format!("stat({}) to enumerate obligations", orig.display()))?;
    // walkdir always dereferences its arguments if it is a symlink, so we special case it
    match FileKind::of_metadata(&meta) {
        FileKind::Directory => {
            for entry in walkdir::WalkDir::new(orig) {
                let entry = entry.with_context(|| format!("iterating in {}", orig.display()))?;
                let meta = entry
                    .metadata()
                    .with_context(|| format!("stat({}) to get size", entry.path().display()))?;
                orig_paths.push((entry.into_path(), utils::copy_size(&meta)));
            }
        }
        _ => orig_paths.push((orig.to_path_buf(), utils::copy_size(&meta))),
    }
    let total_size = orig_paths.iter().map(|&(_, size)| size).sum();
    progress.next_round(total_size);
    let mut new_paths = utils::change_prefixes(orig, target, orig_paths.iter().map(|x| &x.0));
    let mut res = HashSet::new();
    for (dest, (source, size)) in new_paths.drain(..).zip(orig_paths) {
        let checksum = if utils::exists(&dest)
            .with_context(|| format!("checking if a copy {} already exists", dest.display()))?
        {
            let mut checksum = None;
            let _changed = copy::fix_path(cache_manager, progress, &source, &dest, &mut checksum)
                .with_context(|| {
                format!(
                    "fixing existing copy {} of {}",
                    dest.display(),
                    source.display()
                )
            })?;
            checksum.unwrap()
        } else {
            copy::copy_path(cache_manager, progress, &source, &dest)
                .with_context(|| format!("copying {} to {}", source.display(), dest.display()))?
        };
        res.insert(Obligation {
            source,
            dest,
            checksum,
            size,
        });
    }
    Ok(res)
}

arg_enum! {
    #[derive(Debug, Copy, Clone)]
    enum Mode {
        Vm,
        DirectIO,
        Umount,
    }
}

#[derive(StructOpt, Debug)]
#[structopt(name = "cccp")]
struct Opt {
    /// File or directory to copy
    #[structopt(name = "SOURCE", parse(from_os_str))]
    input: PathBuf,
    /// Destination. Can be a block device if SOURCE is a regular file.
    #[structopt(name = "DEST", parse(from_os_str))]
    output: PathBuf,
    /// Only attempt to fix files once, and bail out if it is not enough
    #[structopt(short = "1", long)]
    once: bool,
    /// Method used to prevent re-reading from cache when checking files.
    #[structopt(possible_values = &Mode::variants(), case_insensitive = true, default_value="directio", short, long)]
    mode: Mode,
}

/// Attempts to canonicalizes the input path, but allows the last component of the path to be a broken symlink
/// or to not exist at at all if `must_exist` is true.
/// May return a non canonical path for example if the path ends with ..
fn canonicalize(path: &Path, must_exist: bool) -> anyhow::Result<PathBuf> {
    let canon = match (path.parent(), path.file_name()) {
        (Some(p), Some(f)) => {
            let mut p2 = p
                .canonicalize()
                .with_context(|| format!("Canonicalizing parent directory {}", p.display()))?;
            p2.push(f);
            p2
        }
        _ => path.into(),
    };
    anyhow::ensure!(
        !must_exist
            || utils::exists(&canon).with_context(|| format!(
                "Checking the existence of {} to canonicalize {}",
                canon.display(),
                path.display()
            ))?,
        "Path {} (canonicalized to {}) does not exist.",
        path.display(),
        canon.display()
    );
    Ok(canon)
}

fn main() -> anyhow::Result<()> {
    let opt = Opt::from_args();
    let mut cache_manager = match opt.mode {
        Mode::Vm => Box::new(cache::vm::PageCacheManager::default()) as Box<dyn CacheManager>,
        Mode::DirectIO => Box::new(cache::directio::DirectIOCacheManager::default()),
        Mode::Umount => Box::new(cache::umount::UmountCacheManager::default()),
    };
    let source_ = canonicalize(&opt.input, true)
        .with_context(|| format!("Canonicalizing input path {}", opt.input.display()))?;
    let source = &source_;
    let target_ = canonicalize(&opt.output, false)
        .with_context(|| format!("Canonicalizing output path {}", opt.input.display()))?;
    let target = &target_;
    if target.is_absolute() && source.is_absolute() {
        // this prevents trying to unmount .
        std::env::set_current_dir("/").context("chdir(/)")?;
    }
    cache_manager.permission_check(&target).with_context(|| {
        format!(
            "Checking permissions for cache management mode --mode={}",
            opt.mode
        )
    })?;
    let mut progress = Progress::new();
    let mut obligations = first_copy(&*cache_manager, &mut progress, source, target)
        .context("during initial copy")?;
    // corrupt(&opt.output)?;
    while !obligations.is_empty() {
        progress.syncing();
        cache_manager
            .drop_cache(&target)
            .with_context(|| format!("Dropping cache below {}", target.display()))?;
        let total_size = obligations.iter().map(|o| o.size).sum();
        progress.next_round(total_size);
        obligations.retain(|obligation| {
            let mut checksum = Some(obligation.checksum);
            let res = copy::fix_path(
                &*cache_manager,
                &progress,
                &obligation.source,
                &obligation.dest,
                &mut checksum,
            )
            .context("while fixing copy")
            .unwrap();
            res
        });
        if opt.once && !obligations.is_empty() {
            anyhow::bail!("Still files to fix: {:?}", &obligations);
        }
    }
    progress.done();
    Ok(())
}
