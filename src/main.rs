mod cache;
mod checksum;
mod copy;
mod utils;

use anyhow::Context;
use std::io::prelude::*;
use std::path::{Path, PathBuf};
use structopt::StructOpt;
use checksum::Checksum;
use std::collections::HashSet;

fn corrupt(file: impl AsRef<std::path::Path>) -> anyhow::Result<()> {
    let mut fd = std::fs::OpenOptions::new()
        .write(true)
        .open(file.as_ref())
        .with_context(|| format!("opening {} for corruption", file.as_ref().display()))?;
    fd.seek(std::io::SeekFrom::End(-32))
        .with_context(|| format!("seeking in {} for corruption", file.as_ref().display()))?;
    fd.write_all(b"foo")?;
    Ok(())
}

#[derive(Debug, PartialEq, Eq, Hash, Clone)]
struct Obligation<T: AsRef<Path>+std::hash::Hash+Eq> {
    source: T,
    dest: PathBuf,
    checksum: Checksum,
}
fn first_copy(orig: impl AsRef<Path>, target: &PathBuf) -> anyhow::Result<HashSet<Obligation<PathBuf>>> {
    let mut orig_paths = vec![];
    for entry in walkdir::WalkDir::new(orig.as_ref()) {
        let entry = entry?;
        orig_paths.push(entry.into_path());
    }
    let mut new_paths = utils::change_prefixes(orig, target, &orig_paths);
    let mut res = HashSet::new();
    for (dest, source) in new_paths.drain(..).zip(orig_paths) {
        let checksum = copy::copy_path(&source, &dest)?;
        res.insert(Obligation { source, dest, checksum });
    }
    Ok(res)
}

/// A basic example
#[derive(StructOpt, Debug)]
#[structopt(name = "basic")]
struct Opt {
    /// File or directoty to copy
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
            let res = copy::fix_path(&obligation.source, &obligation.dest, obligation.checksum).context("while fixing copy").unwrap();
            if res {
                println!("Fixed {}", obligation.dest.display());
            }
            res
        });
    }
    Ok(())
}
