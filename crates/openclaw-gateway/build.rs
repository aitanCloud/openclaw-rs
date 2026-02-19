fn main() {
    println!(
        "cargo:rustc-env=BUILD_TIMESTAMP={}",
        chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC")
    );
    println!(
        "cargo:rustc-env=RUST_VERSION={}",
        rustc_version()
    );
}

fn rustc_version() -> String {
    std::process::Command::new("rustc")
        .arg("--version")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}
