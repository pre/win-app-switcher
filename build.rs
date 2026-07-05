/// Stamp the binary with the git commit ("4187cd2" / "4187cd2-dirty") so the
/// running build is identifiable from the tray tooltip and the debug console.
fn main() {
    let hash = std::process::Command::new("git")
        .args(["describe", "--always", "--dirty", "--abbrev=8"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=GIT_HASH={hash}");
}
