mod cache;
mod checksum;
mod copy;
mod utils;

use anyhow::Context;
use std::io::prelude::*;
use std::path::PathBuf;
use structopt::StructOpt;

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
    let checksum = copy::copy_path(&opt.input, &opt.output)?;
    corrupt(&opt.output)?;
    cache::global_drop_cache(&opt.output)?;
    dbg!(copy::fix_path(&opt.input, &opt.output, checksum)?);
    dbg!(copy::fix_path(&opt.input, &opt.output, checksum)?);
    Ok(())
}
