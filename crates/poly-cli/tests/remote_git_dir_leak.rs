//! Regression coverage for an inherited `GIT_DIR` leaking into the git commands
//! the `remote` module runs against **foreign** cached repositories.
//!
//! Every git hook exports `GIT_DIR`, and git honours an explicit `GIT_DIR` over
//! `-C <path>`. So `git -C <mirror> config --get remote.origin.url` — the check
//! in [`ensure_mirror`] that a cached mirror really points where the config says
//! — read the URL out of the **consumer's** repository instead of the mirror's.
//!
//! That check exists to block a cache-poisoning source substitution. It was
//! answering from the wrong repository, which is worse than the checkout failure
//! that first exposed the leak: a guard redirectable by an inherited environment
//! variable is not a guard. It happened to fail closed, which is luck rather than
//! design.
//!
//! This test asserts the **effect** — that verification reads the mirror's own
//! origin under a hostile ambient `GIT_DIR` — rather than that some variable
//! appears in a removal list. A list-membership assertion passes against code
//! that builds the list and never applies it.
//!
//! It lives in its own test binary because it sets `GIT_DIR` for the whole
//! process, which is precisely the inheritance being reproduced; alone in the
//! binary, no other test can race the environment write.

use std::path::Path;
use std::process::Command;

use poly_cli::remote::ensure_mirror;

/// Run git with the leaked `GIT_DIR` explicitly removed, so the test scaffolding
/// is never itself affected by the variable under test.
fn git(args: &[&str], cwd: &Path) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
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

fn commit(message: &str, cwd: &Path) {
    git(
        &[
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=test",
            "-c",
            "commit.gpgSign=false",
            "commit",
            "--quiet",
            "-m",
            message,
        ],
        cwd,
    );
}

#[test]
fn mirror_origin_verification_ignores_an_inherited_git_dir() {
    // The real hook source.
    let origin = tempfile::tempdir().expect("origin dir");
    git(&["init", "--quiet"], origin.path());
    std::fs::write(origin.path().join("a.txt"), "hook source\n").expect("write");
    git(&["add", "-A"], origin.path());
    commit("one", origin.path());

    // A different repository, standing in for the one whose `git commit` invoked
    // the hook. Its origin URL is deliberately not the hook source's, so reading
    // the URL from the wrong repository is detectable rather than coincidental.
    let consumer = tempfile::tempdir().expect("consumer dir");
    git(&["init", "--quiet"], consumer.path());
    std::fs::write(consumer.path().join("b.txt"), "consumer\n").expect("write");
    git(&["add", "-A"], consumer.path());
    commit("consumer base", consumer.path());
    git(
        &[
            "remote",
            "add",
            "origin",
            "https://example.invalid/not-the-hook-source.git",
        ],
        consumer.path(),
    );

    // SAFETY: this is the only test in this test binary, so no other thread can
    // be reading or writing the environment concurrently.
    unsafe {
        std::env::set_var("GIT_DIR", consumer.path().join(".git"));
    }

    let cache = tempfile::tempdir().expect("cache dir");
    let mirror = cache.path().join("mirror.git");
    let url = origin.path().to_string_lossy().into_owned();

    ensure_mirror(&mirror, &url).unwrap_or_else(|error| {
        panic!("mirror provisioning must ignore an inherited GIT_DIR, got: {error:#}");
    });

    // Verification must have read the mirror's stored origin, not the consumer's.
    // Under the leak this call reported the consumer's URL and aborted, naming a
    // repository the user never configured.
    ensure_mirror(&mirror, &url).unwrap_or_else(|error| {
        panic!("re-verifying an existing mirror must read the mirror's own origin, got: {error:#}");
    });

    assert_eq!(
        git(&["config", "--get", "remote.origin.url"], &mirror),
        url,
        "the mirror's stored origin must be the configured hook source"
    );

    // The guard must still reject a genuinely substituted cache — proving the fix
    // removed the leak without disabling the check it protects.
    let substituted = ensure_mirror(&mirror, "https://example.invalid/some-other-source.git");
    assert!(
        substituted.is_err(),
        "a mirror whose origin disagrees with the configured URL must still be rejected"
    );
}
