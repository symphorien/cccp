// SPDX-License-Identifier: LGPL-3.0

use cli_test_dir::ExpectStatus;
use cli_test_dir::TestDir;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::process::Command;

/// runs cccp with corresponding arguments
fn run(t: &TestDir, source: impl AsRef<Path>, destination: impl AsRef<Path>) {
    let mut c = t.cmd();
    c.env("CCCP_NO_ROOT", "1");
    c.current_dir(t.path("."));
    c.args(&[source.as_ref(), destination.as_ref()]);
    dbg!(c).expect_success();
}

/// panics if source and destination are different (with diffoscope)
fn compare(t: &TestDir, source: impl AsRef<Path>, destination: impl AsRef<Path>) {
    let mut c = Command::new("diffoscope");
    c.current_dir(t.path("."));
    c.arg("--exclude-directory-metadata=yes");
    c.args(&[source.as_ref(), destination.as_ref()]);
    dbg!(c).expect_success();
}

/// copies source to destination
fn copy(t: &TestDir, source: impl AsRef<Path>, destination: impl AsRef<Path>) {
    let mut c = Command::new("cp");
    c.current_dir(t.path("."));
    c.arg("-r");
    c.args(&[source.as_ref(), destination.as_ref()]);
    dbg!(c).expect_success();
}

fn run_test_case(t: &TestDir, path: impl AsRef<Path>) {
    let dest = path.as_ref().with_extension("dest");
    let exists = match std::fs::symlink_metadata(dbg!(&dest)) {
        Err(e) => match e.kind() {
            std::io::ErrorKind::NotFound => false,
            _ => panic!("cannot stat {}: {}", path.as_ref().display(), e),
        },
        Ok(_) => true,
    };
    let working = "./dest";
    if dbg!(exists) {
        copy(t, &dest, working);
    }
    run(t, &path, working);
    compare(t, &path, working);
}

fn main() -> anyhow::Result<()> {
    let t = TestDir::new("cccp", "global");
    for entry in std::fs::read_dir(t.src_path("tests/fixtures/"))? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().map(OsStr::as_bytes) == Some(b"orig") {
            eprintln!("Running test {}", path.display());
            run_test_case(
                &TestDir::new("cccp", &path.file_name().unwrap().to_string_lossy()),
                path,
            );
        }
    }
    Ok(())
}

#[test]
fn run_all_tests() {
    main().unwrap();
}
