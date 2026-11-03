//! `poly mcp` on a build without the in-process server: it needs the `mcp` component, so a build
//! that embeds no component lock must say so and exit 2, never hang waiting on stdio.
#![cfg(all(unix, not(feature = "bundled-mcp"), not(feature = "dev-lock")))]

use std::process::{Command, Stdio};

const POLY: &str = env!("CARGO_BIN_EXE_poly");

#[test]
fn without_a_component_lock_poly_mcp_explains_and_exits_2() {
    let cache = tempfile::tempdir().expect("tempdir");
    let output = Command::new(POLY)
        .arg("mcp")
        .env_remove("POLY_OFFLINE")
        .env_remove("POLY_COMPONENT_DEV_LOCK")
        .env("POLY_COMPONENT_CACHE", cache.path())
        .current_dir(cache.path())
        .stdin(Stdio::null())
        .output()
        .expect("run poly mcp");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("`mcp` component"), "{stderr}");
    assert!(stderr.contains("bundled-mcp"), "names the in-process build: {stderr}");
    assert!(output.stdout.is_empty(), "stdout stays clean for the protocol");
}
