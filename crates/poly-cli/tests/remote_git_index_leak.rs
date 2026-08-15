//! Regression coverage for an inherited `GIT_INDEX_FILE` leaking into the git
//! commands the `remote` module runs against **foreign** cached repositories.
//!
//! A git hook runs with `GIT_INDEX_FILE` set, and for `git commit -a` /
//! `git commit <pathspec>` git sets it to an **absolute** path inside the
//! consumer repository. Inherited into `git checkout --detach <oid>` inside a
//! freshly cloned cache checkout, git then reconciles the foreign index against
//! an unrelated tree and aborts, dumping the unrelated repository's file list
//! into poly's error.
//!
//! This test lives in its own test binary because it must set `GIT_INDEX_FILE`
//! for the whole process (that is precisely the inheritance being reproduced);
//! keeping it alone in the binary means no other test can race the env write.

use std::path::Path;
use std::process::Command;

use poly_cli::remote::materialize;

/// Run git with the leaked `GIT_INDEX_FILE` explicitly removed, so test
/// scaffolding and assertions are never themselves affected by it.
fn git(args: &[&str], cwd: &Path) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env_remove("GIT_INDEX_FILE")
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} in {} failed: {}",
        cwd.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn commit(message: &str, cwd: &Path) {
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
            "-m",
            message,
        ],
        cwd,
    );
}

/// Create a lightweight tag, overriding any host config that would force an
/// annotated or signed tag.
fn tag(name: &str, cwd: &Path) {
    git(
        &[
            "-c",
            "tag.gpgSign=false",
            "-c",
            "tag.forceSignAnnotated=false",
            "tag",
            name,
        ],
        cwd,
    );
}

#[test]
fn materialize_ignores_inherited_git_index_file() {
    // Origin: two commits, two tags. The default branch (and therefore the
    // mirror's HEAD, and the fresh clone's HEAD) sits on `v2`, while the pinned
    // revision is `v1` — so the detach checkout has real work to do and must
    // consult an index.
    let origin = tempfile::tempdir().expect("origin dir");
    git(&["init", "--quiet"], origin.path());
    std::fs::write(origin.path().join("catalog.txt"), "origin v1\n").expect("write catalog");
    git(&["add", "-A"], origin.path());
    commit("one", origin.path());
    tag("v1", origin.path());
    std::fs::write(origin.path().join("catalog.txt"), "origin v2\n").expect("rewrite catalog");
    std::fs::write(origin.path().join("added-in-v2.txt"), "only in v2\n").expect("write added");
    git(&["add", "-A"], origin.path());
    commit("two", origin.path());
    tag("v2", origin.path());
    let pinned = git(&["rev-parse", "v1^{commit}"], origin.path());

    // A foreign consumer repository with a populated index, standing in for the
    // repository whose `git commit -a` invoked the hook.
    let consumer = tempfile::tempdir().expect("consumer dir");
    git(&["init", "--quiet"], consumer.path());
    for name in ["catalog.txt", "consumer-only.txt", "added-in-v2.txt"] {
        std::fs::write(consumer.path().join(name), format!("consumer {name}\n")).expect("write consumer file");
    }
    git(&["add", "-A"], consumer.path());
    commit("consumer base", consumer.path());
    std::fs::write(consumer.path().join("catalog.txt"), "consumer staged\n").expect("stage change");
    git(&["add", "-A"], consumer.path());
    let foreign_index = consumer.path().join(".git").join("index");
    let index_before = std::fs::read(&foreign_index).expect("read foreign index");

    // SAFETY: this is the only test in this test binary, so no other thread can
    // be reading or writing the environment concurrently.
    unsafe {
        std::env::set_var("GIT_INDEX_FILE", &foreign_index);
    }

    let cache = tempfile::tempdir().expect("cache dir");
    let url = origin.path().to_string_lossy().into_owned();
    let checkout = materialize(&url, &pinned, cache.path(), false)
        .unwrap_or_else(|error| panic!("materialize must ignore an inherited GIT_INDEX_FILE: {error:#}"));

    assert_eq!(
        git(&["rev-parse", "HEAD^{commit}"], &checkout),
        pinned,
        "checkout HEAD must be the pinned revision"
    );
    // Line endings are normalized before comparing: git's `core.autocrlf` is on
    // by default on Windows, so a checkout there legitimately writes CRLF. This
    // test is about which *revision* was checked out under a leaked
    // GIT_INDEX_FILE, not about how git spells a newline.
    assert_eq!(
        std::fs::read_to_string(checkout.join("catalog.txt"))
            .expect("read checked-out file")
            .replace("\r\n", "\n"),
        "origin v1\n",
        "checked-out tree must be the pinned revision's content"
    );
    assert!(
        !checkout.join("added-in-v2.txt").exists(),
        "the pinned revision predates added-in-v2.txt"
    );
    assert_eq!(
        std::fs::read(&foreign_index).expect("re-read foreign index"),
        index_before,
        "the consumer repository's index must be byte-identical afterwards"
    );
    // The staleness probe runs `git status` inside the checkout. With a leaked
    // index that died `fatal: unable to read <oid>`, so every run judged the
    // cached checkout invalid and rebuilt it from scratch.
    assert!(
        poly_cli::remote::checkout_is_valid(&checkout, &pinned),
        "a freshly materialized checkout must validate under an inherited GIT_INDEX_FILE"
    );
}
