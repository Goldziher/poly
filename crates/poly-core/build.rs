//! Stage the component lock a release build embeds.
//!
//! The release workflow sets `POLY_COMPONENT_LOCK_FILE` to the merged, signed
//! `components.lock.json`. It is copied to `OUT_DIR`, where
//! `components/embedded.rs` includes it. Without the variable the staged file is empty,
//! which the core reads as "this build embeds no component lock".

use std::path::PathBuf;

const LOCK_FILE_ENV: &str = "POLY_COMPONENT_LOCK_FILE";

fn main() {
    println!("cargo::rerun-if-env-changed={LOCK_FILE_ENV}");

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR is always set"));
    let lock = match std::env::var_os(LOCK_FILE_ENV) {
        Some(path) => {
            println!("cargo::rerun-if-changed={}", PathBuf::from(&path).display());
            let lock = std::fs::read_to_string(&path).unwrap_or_else(|error| {
                panic!(
                    "cannot read {LOCK_FILE_ENV}={}: {error}",
                    PathBuf::from(&path).display()
                )
            });
            assert!(!lock.trim().is_empty(), "{LOCK_FILE_ENV} names an empty file");
            lock
        }
        None => String::new(),
    };
    std::fs::write(out_dir.join("components.lock.json"), lock).expect("write the staged component lock");
}
