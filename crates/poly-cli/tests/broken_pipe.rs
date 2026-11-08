//! A reader that stops early (`poly config schema | head`) must not turn into a panic report.

use std::io::Read;
use std::process::{Command, Stdio};

const POLY: &str = env!("CARGO_BIN_EXE_poly");

#[test]
fn a_closed_stdout_ends_the_run_without_a_panic_message() {
    let mut child = Command::new(POLY)
        .args(["config", "schema"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn poly");
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut first = [0_u8; 16];
    stdout.read_exact(&mut first).expect("read the first bytes");
    drop(stdout);
    let output = child.wait_with_output().expect("wait for poly");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("panicked") && !stderr.contains("Broken pipe"),
        "stderr was: {stderr}"
    );
}
