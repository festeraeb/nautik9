//! Bakes the git commit and the build time into the binary for GET /api/admin/version, so the
//! fleet can tell which build a phone runs after an update. Never fails the build: outside a git
//! checkout the values read "unknown".
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let o = Command::new("git").args(args).output().ok()?;
    let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
    (o.status.success() && !s.is_empty()).then_some(s)
}

fn main() {
    let sha = git(&["rev-parse", "--short=9", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let dirty = Command::new("git").args(["diff", "--quiet", "HEAD", "--"]).status().map(|s| !s.success()).unwrap_or(false);
    println!("cargo:rustc-env=NAUTIK9_GIT_SHA={sha}{}", if dirty { "-dirty" } else { "" });
    let built = Command::new("date").args(["-u", "+%Y-%m-%dT%H:%M:%SZ"]).output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).filter(|s| !s.is_empty());
    println!("cargo:rustc-env=NAUTIK9_BUILT={}", built.unwrap_or_else(|| "unknown".into()));
    // Re-run on a new commit (HEAD and the branch ref; this works in a git worktree too) and on any
    // source change, so the dirty mark and the build time follow the code.
    for p in [git(&["rev-parse", "--path-format=absolute", "--git-path", "HEAD"]), git(&["symbolic-ref", "-q", "HEAD"]).and_then(|r| git(&["rev-parse", "--path-format=absolute", "--git-path", &r]))].into_iter().flatten() {
        println!("cargo:rerun-if-changed={p}");
    }
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=build.rs");
}
