/// Stamp the binary with the git commit ("ab1c4c38" / "ab1c4c38-dirty") so
/// the running build is identifiable from the tray tooltip and the debug
/// console.
fn main() {
    // Re-stamp when sources change (dirty flag) and when HEAD moves (a
    // commit) — listing paths here replaces cargo's rerun-on-any-change.
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=.git/HEAD");
    if let Ok(head) = std::fs::read_to_string(".git/HEAD") {
        if let Some(reference) = head.strip_prefix("ref: ") {
            println!("cargo:rerun-if-changed=.git/{}", reference.trim());
        }
    }
    // The release workflow sets RELEASE_TAG=vX.Y.Z (CI builds before the
    // tag exists, so `git describe` can't see it). Dev builds leave it
    // unset; an empty RELEASE_TAG disables the update check.
    println!("cargo:rerun-if-env-changed=RELEASE_TAG");
    println!(
        "cargo:rustc-env=RELEASE_TAG={}",
        std::env::var("RELEASE_TAG").unwrap_or_default()
    );
    let hash = std::process::Command::new("git")
        .args(["describe", "--always", "--dirty", "--abbrev=8"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=GIT_HASH={hash}");
}
