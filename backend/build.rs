//! Records the commit the backend is built from, for `GET /status` (review G5: anyone can check
//! that the running system is the submitted code).

use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

fn main() {
    println!("cargo:rerun-if-changed=../.git/HEAD");
    println!("cargo:rerun-if-changed=../.git/index");
    let commit = git(&["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".into());
    // Tracked files only: untracked local files (data, logs) do not make a build "dirty".
    let clean = git(&["status", "--porcelain", "--untracked-files=no"]).is_some_and(|s| s.is_empty());
    println!("cargo:rustc-env=ZECURED_COMMIT={commit}");
    println!("cargo:rustc-env=ZECURED_TREE_CLEAN={clean}");
}
