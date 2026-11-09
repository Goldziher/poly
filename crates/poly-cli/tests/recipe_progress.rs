//! What a recipe tool says on stderr while it works, and what `--debug` calls
//! its answers.
//!
//! A first `install = "auto"` run downloads the tool and then runs it once per
//! unit; both can take minutes, so each must leave a note on stderr at the
//! default verbosity — never on stdout, and not under `--quiet`. And a batched
//! answer computed in this run is not a cache hit, which `--no-cache` makes
//! plain.
//!
//! The tool is a shell stub served from a local HTTP server and pinned with
//! `version` + `url` + `sha256`, so nothing touches the network.
#![cfg(unix)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::{Command, Output};

use sha2::{Digest, Sha256};
use tempfile::TempDir;

const POLY: &str = env!("CARGO_BIN_EXE_poly");

/// A `golangci-lint` that reports one finding in `main.go`, whatever it is
/// asked, so the file has a result for `--debug` to annotate.
const STUB: &str = "#!/bin/sh\nprintf '{\"Issues\":[{\"FromLinter\":\"stub\",\"Text\":\"stub finding\",\
\"Pos\":{\"Filename\":\"main.go\",\"Line\":3,\"Column\":1}}]}'\n";

/// Serve `body` for every request, forever, and return the base URL.
fn serve(body: &'static [u8]) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("address"));
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut buffer = [0u8; 4096];
            let _ = stream.read(&mut buffer);
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(body);
        }
    });
    base
}

struct Fixture {
    repo: TempDir,
    cache: TempDir,
    url: String,
}

fn fixture() -> Fixture {
    let repo = tempfile::tempdir().expect("repo");
    let cache = tempfile::tempdir().expect("cache");
    let url = format!("{}/golangci-lint", serve(STUB.as_bytes()));
    let sha256: String = Sha256::digest(STUB.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let write = |name: &str, body: &str| std::fs::write(repo.path().join(name), body).expect("write fixture");
    write("go.mod", "module example.com/demo\n\ngo 1.22\n");
    write("main.go", "package main\n\nfunc main() {}\n");
    // A second file, so the runner batches the unit rather than linting per file.
    write("util.go", "package main\n\nfunc util() {}\n");
    write(
        "poly.toml",
        &format!(
            "[tools.golangci-lint]\nenabled = true\ninstall = \"auto\"\nversion = \"2.6.1\"\n\
             url = \"{url}\"\nsha256 = \"{sha256}\"\n"
        ),
    );
    Fixture { repo, cache, url }
}

fn poly(fixture: &Fixture, args: &[&str]) -> Output {
    Command::new(POLY)
        .args(args)
        .current_dir(fixture.repo.path())
        .env("POLY_CACHE_HOME", fixture.cache.path())
        .env("NO_COLOR", "1")
        .env_remove("RUST_LOG")
        .env_remove("POLY_OFFLINE")
        .output()
        .expect("run poly")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn lint(fixture: &Fixture, extra: &[&str]) -> Output {
    let mut args = vec!["lint", "--workspace", "never", "--no-cache"];
    args.extend_from_slice(extra);
    args.extend(["main.go", "util.go"]);
    let output = poly(fixture, &args);
    assert!(
        output.status.code().is_some_and(|code| code < 2),
        "{args:?} did not complete: {}\n{}",
        text(&output.stdout),
        text(&output.stderr)
    );
    output
}

#[test]
fn a_download_and_a_unit_run_are_announced_on_stderr_only() {
    let fixture = fixture();
    let first = lint(&fixture, &["--format", "json"]);
    let stderr = text(&first.stderr);
    let stdout = text(&first.stdout);
    let downloading = format!("golangci-lint: downloading release 2.6.1 from {}", fixture.url);
    assert!(stderr.contains(&downloading), "no download note on stderr:\n{stderr}");
    assert!(
        stderr.contains("golangci-lint: installed release 2.6.1 ("),
        "no install note with a size on stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("golangci-lint: running on 1 directory in ."),
        "no unit note on stderr:\n{stderr}"
    );
    let document: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|error| panic!("stdout is not one JSON document ({error}):\n{stdout}"));
    assert_eq!(document["summary"]["errored"], 0, "{stdout}\n{stderr}");
    assert!(
        !stdout.contains("golangci-lint:"),
        "a progress note reached stdout:\n{stdout}"
    );

    let second = lint(&fixture, &[]);
    let stderr = text(&second.stderr);
    assert!(
        !stderr.contains("downloading"),
        "an installed release must not be announced as a download:\n{stderr}"
    );
    assert!(
        stderr.contains("golangci-lint: running on 1 directory in ."),
        "no unit note on the second run:\n{stderr}"
    );

    let quiet = lint(&fixture, &["--quiet"]);
    let stderr = text(&quiet.stderr);
    assert!(
        !stderr.contains("golangci-lint: running"),
        "--quiet must silence progress notes:\n{stderr}"
    );
}

#[test]
fn no_cache_debug_never_reports_a_cache_hit() {
    let fixture = fixture();
    let output = lint(&fixture, &["--debug"]);
    let stdout = text(&output.stdout);
    let stderr = text(&output.stderr);
    let line = stdout
        .lines()
        .find(|line| line.contains("[debug] golangci-lint"))
        .unwrap_or_else(|| panic!("no debug record for golangci-lint:\n{stdout}\n{stderr}"));
    assert!(line.contains("  batched  "), "a batched answer must say so: {line}");
    assert!(
        !stdout.contains("cache hit") && !stderr.contains("cache hit"),
        "--no-cache reported a cache hit:\n{stdout}\n{stderr}"
    );
}
