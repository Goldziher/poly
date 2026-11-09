//! `poly lock` resolves a hook source or `extends` ref with `git ls-remote`. Run
//! from the consumer repository, that command reads the consumer's
//! `.git/config`, whose `url.<base>.insteadOf` can point the lookup at a
//! different repository than the one the cache mirror later verifies as origin.
//! The ref would then be pinned to a commit from the wrong source.
//!
//! The test sets the process working directory, so it lives alone in its own
//! test binary where nothing else can observe the change.

use std::path::Path;
use std::process::Command;

use poly_cli::remote::resolve_remote_revision;

fn git(args: &[&str], cwd: &Path) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} in {} failed: {}",
        cwd.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn repository_with_commit(message: &str) -> tempfile::TempDir {
    let repository = tempfile::tempdir().expect("repository dir");
    git(&["init", "--quiet"], repository.path());
    git(
        &[
            "-c",
            "user.name=Poly Test",
            "-c",
            "user.email=poly@example.invalid",
            "-c",
            "commit.gpgSign=false",
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            message,
        ],
        repository.path(),
    );
    repository
}

#[test]
fn ls_remote_ignores_the_consumer_repository_config() {
    let configured = repository_with_commit("the configured source");
    let impostor = repository_with_commit("a different source");
    let configured_url = configured.path().to_string_lossy().into_owned();
    let impostor_url = impostor.path().to_string_lossy().into_owned();
    let expected = git(&["rev-parse", "HEAD"], configured.path());
    assert_ne!(expected, git(&["rev-parse", "HEAD"], impostor.path()));

    // The consumer rewrites the configured URL to the impostor.
    let consumer = tempfile::tempdir().expect("consumer dir");
    git(&["init", "--quiet"], consumer.path());
    git(
        &["config", &format!("url.{impostor_url}.insteadOf"), &configured_url],
        consumer.path(),
    );
    std::env::set_current_dir(consumer.path()).expect("enter the consumer");

    let neutral = tempfile::tempdir().expect("neutral dir");
    let resolved = resolve_remote_revision(&configured_url, "HEAD", neutral.path()).expect("resolve HEAD");
    assert_eq!(resolved, expected, "the consumer's insteadOf redirected ls-remote");
}
