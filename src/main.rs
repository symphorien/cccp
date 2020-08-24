mod cache;
mod checksum;
mod copy;
mod utils;

use anyhow::Context;
use checksum::Checksum;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use structopt::StructOpt;

#[derive(Debug, PartialEq, Eq, Hash, Clone)]
struct Obligation<T: AsRef<Path> + std::hash::Hash + Eq> {
    source: T,
    dest: PathBuf,
    checksum: Checksum,
}

fn first_copy(
    orig: impl AsRef<Path>,
    target: &PathBuf,
) -> anyhow::Result<HashSet<Obligation<PathBuf>>> {
    let mut orig_paths = vec![];
    for entry in walkdir::WalkDir::new(orig.as_ref()) {
        let entry = entry?;
        orig_paths.push(entry.into_path());
    }
    let mut new_paths = utils::change_prefixes(orig, target, &orig_paths);
    let mut res = HashSet::new();
    for (dest, source) in new_paths.drain(..).zip(orig_paths) {
        let checksum = if utils::exists(&dest)
            .with_context(|| format!("checking if a copy {} already exists", dest.display()))?
        {
            let checksum = copy::checksum_path(&source).with_context(|| {
                format!("computing checksum of reference file {}", source.display())
            })?;
            let _changed = copy::fix_path(&source, &dest, checksum).with_context(|| {
                format!(
                    "fixing existing copy {} of {}",
                    dest.display(),
                    source.display()
                )
            })?;
            checksum
        } else {
            copy::copy_path(&source, &dest)
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
    let mut obligations = first_copy(&opt.input, &opt.output).context("during initial copy")?;
    // corrupt(&opt.output)?;
    while !obligations.is_empty() {
        cache::global_drop_cache(&opt.output)?;
        obligations.retain(|obligation| {
            let res = copy::fix_path(&obligation.source, &obligation.dest, obligation.checksum)
                .context("while fixing copy")
                .unwrap();
            if res {
                println!("Fixed {}", obligation.dest.display());
            }
            res
        });
    }
    Ok(())
}
