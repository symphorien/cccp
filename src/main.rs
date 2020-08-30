mod cache;
mod checksum;
mod copy;
mod utils;

use crate::cache::CacheManager;
use crate::utils::FileKind;
use anyhow::Context;
use checksum::Checksum;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use structopt::StructOpt;

#[derive(Debug, PartialEq, Eq, Hash, Clone)]
struct Obligation {
    source: PathBuf,
    dest: PathBuf,
    checksum: Checksum,
}

fn first_copy(
    cache_manager: &dyn CacheManager,
    orig: impl AsRef<Path>,
    target: &PathBuf,
) -> anyhow::Result<HashSet<Obligation>> {
    let mut orig_paths = vec![];
    // walkdir always dereferences its arguments if it is a symlink, so we special case it
    match FileKind::of_path(orig.as_ref())
        .with_context(|| format!("stat({}) to enumerate obligations", orig.as_ref().display()))?
    {
        FileKind::Directory => {
            for entry in walkdir::WalkDir::new(orig.as_ref()) {
                let entry =
                    entry.with_context(|| format!("iterating in {}", orig.as_ref().display()))?;
                orig_paths.push(entry.into_path());
            }
        }
        _ => orig_paths.push(orig.as_ref().to_path_buf()),
    }
    let mut new_paths = utils::change_prefixes(orig, target, &orig_paths);
    let mut res = HashSet::new();
    for (dest, source) in new_paths.drain(..).zip(orig_paths) {
        let checksum = if utils::exists(&dest)
            .with_context(|| format!("checking if a copy {} already exists", dest.display()))?
        {
            let mut checksum = None;
            let _changed = copy::fix_path(cache_manager, &source, &dest, &mut checksum)
                .with_context(|| {
                    format!(
                        "fixing existing copy {} of {}",
                        dest.display(),
                        source.display()
                    )
                })?;
            checksum.unwrap()
        } else {
            copy::copy_path(cache_manager, &source, &dest)
                .with_context(|| format!("copying {} to {}", source.display(), dest.display()))?
        };
        res.insert(Obligation {
            source,
            dest,
            checksum,
        });
    }
    Ok(res)
}

#[derive(StructOpt, Debug)]
#[structopt(name = "basic")]
struct Opt {
    /// File or directory to copy
    #[structopt(name = "SOURCE", parse(from_os_str))]
    input: PathBuf,
    /// Destination. Can a a block device if SOURCE is a regular file.
    #[structopt(name = "DEST", parse(from_os_str))]
    output: PathBuf,
}

fn main() -> anyhow::Result<()> {
    let opt = Opt::from_args();
    let mut cache_manager = cache::vm::PageCacheManager::default();
    cache_manager
        .permission_check(&opt.output)
        .context("Checking permissions for cache management mode")?;
    let mut obligations =
        first_copy(&cache_manager, &opt.input, &opt.output).context("during initial copy")?;
    // corrupt(&opt.output)?;
    while !obligations.is_empty() {
        cache_manager
            .drop_cache(&opt.output)
            .with_context(|| format!("Dropping cache below {}", opt.output.display()))?;
        obligations.retain(|obligation| {
            let mut checksum = Some(obligation.checksum);
            let res = copy::fix_path(
                &cache_manager,
                &obligation.source,
                &obligation.dest,
                &mut checksum,
            )
            .context("while fixing copy")
            .unwrap();
            if res {
                println!("Fixed {}", obligation.dest.display());
            }
            res
        });
        // if !obligations.is_empty() {
        //     anyhow::bail!("still things to fix {:?}", &obligations);
        // }
    }
    Ok(())
}
