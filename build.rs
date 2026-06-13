//! Capture the target triple this binary is built for so the self-updater can
//! pick the matching prebuilt release artifact at runtime (`src/update.rs`).
//! `TARGET` is only available to build scripts, not to the crate itself, so we
//! forward it as a compile-time env var read via `env!`.

fn main() {
    let target = std::env::var("TARGET").expect("cargo always sets TARGET for build scripts");
    println!("cargo:rustc-env=MUDPUPPY_TARGET_TRIPLE={target}");
}
