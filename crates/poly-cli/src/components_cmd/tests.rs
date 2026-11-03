//! The subcommand logic over a fake connector and a temporary cache.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};

use alef_component_runtime::ComponentManager;
use poly_component_host::{ComponentHost, ComponentTransport, HostOptions, TransportConnector};
use poly_component_index::{Component, ComponentIndex};
use tempfile::TempDir;

use super::ops::{self, Env, State};
use super::prune;

const TARGET: &str = "test-target";
const DEMO_SHA: &str = "abababababababababababababababababababababababababababababababab";
const OTHER_TARGET_SHA: &str = "2323232323232323232323232323232323232323232323232323232323232323";
const ORPHAN_SHA: &str = "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd";

struct Counting(Arc<AtomicUsize>);

struct Null;

impl ComponentTransport for Null {
    fn describe(&self) -> anyhow::Result<Vec<u8>> {
        anyhow::bail!("not a real component")
    }

    fn call(&self, _header: &[u8], _body: &[u8]) -> anyhow::Result<Vec<u8>> {
        anyhow::bail!("not a real component")
    }
}

impl TransportConnector for Counting {
    fn connect(&self, _manager: &ComponentManager, _id: &str) -> anyhow::Result<Arc<dyn ComponentTransport>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(Arc::new(Null))
    }
}

fn component(name: &str) -> Component {
    Component {
        name: name.into(),
        features: vec![format!("engine-{name}")],
        ..Component::default()
    }
}

fn index() -> ComponentIndex {
    ComponentIndex {
        schema: 1,
        components: vec![component("demo"), component("baked")],
        presets: [("all".to_owned(), vec!["demo".to_owned(), "baked".to_owned()])].into(),
    }
}

fn is_baked(component: &Component) -> bool {
    component.name == "baked"
}

fn lock() -> String {
    format!(
        r#"{{"schema_version":2,"public_keys":{{}},"artifacts":[{{
            "identity":{{"crate_name":"c","component":"demo","version":"1.0.0","target":"{TARGET}",
                         "feature_hash":"{h}","contract_hash":"{h}"}},
            "provides":[],"mode":"download","url":"https://127.0.0.1:9/unreachable/demo.tar.gz",
            "sha256":"{DEMO_SHA}","size":1,"manifest_sha256":"{h}","key_id":"k"}},{{
            "identity":{{"crate_name":"c","component":"demo","version":"1.0.0","target":"other-target",
                         "feature_hash":"{h}","contract_hash":"{h}"}},
            "provides":[],"mode":"download","url":"https://127.0.0.1:9/unreachable/demo-other.tar.gz",
            "sha256":"{OTHER_TARGET_SHA}","size":1,"manifest_sha256":"{h}","key_id":"k"}}]}}"#,
        h = "11".repeat(32),
    )
}

struct Fixture {
    tmp: TempDir,
    index: ComponentIndex,
    host: ComponentHost,
    lock: String,
    connects: Arc<AtomicUsize>,
    offline: bool,
}

impl Fixture {
    fn new(offline: bool) -> Self {
        let tmp = TempDir::new().unwrap();
        let lock = lock();
        let mut options = HostOptions::new(lock.clone(), tmp.path().join("cache"));
        options.target = Some(TARGET.into());
        options.offline = offline;
        options.gate_loads = true;
        let connects = Arc::new(AtomicUsize::new(0));
        let host = ComponentHost::new(options)
            .unwrap()
            .with_connector(Arc::new(Counting(Arc::clone(&connects))));
        Self {
            tmp,
            index: index(),
            host,
            lock,
            connects,
            offline,
        }
    }

    fn env(&self) -> Env<'_> {
        Env {
            index: &self.index,
            host: Some(&self.host),
            no_host: None,
            lock_json: Some(&self.lock),
            linked: &is_baked,
            offline: self.offline,
            cache_root: self.root(),
        }
    }

    fn root(&self) -> PathBuf {
        self.host.manager().cache_root().to_path_buf()
    }

    fn object(&self, sha: &str) -> PathBuf {
        self.root().join("sha256").join(sha)
    }

    fn seed(&self, sha: &str) {
        std::fs::create_dir_all(self.object(sha)).unwrap();
        std::fs::write(self.object(sha).join("component.json"), "{}").unwrap();
    }
}

fn names(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

#[test]
fn list_and_status_never_download() {
    let fixture = Fixture::new(true);
    let env = fixture.env();

    let listing = ops::list(&env);
    let states: Vec<_> = listing.components.iter().map(|c| (c.name.as_str(), c.state)).collect();
    assert_eq!(states, [("demo", State::NotInstalled), ("baked", State::Bundled)]);
    let status = ops::status(&env, &names(&["demo"]), true);
    assert_eq!(status.missing, Some(names(&["demo"])));
    assert!(!status.ok());

    assert_eq!(
        fixture.connects.load(Ordering::SeqCst),
        0,
        "list/status reached the connector"
    );
    assert!(
        !fixture.root().join("sha256").exists(),
        "list/status wrote to the cache"
    );
}

#[test]
fn install_does_reach_the_connector() {
    let fixture = Fixture::new(false);
    let demo = ops::resolve(&fixture.index, &names(&["demo"])).unwrap();
    let actions = ops::install(&fixture.env(), &demo).unwrap();
    assert_eq!(actions.results[0].action, "installed");
    assert!(actions.ok);
    assert_eq!(fixture.connects.load(Ordering::SeqCst), 1);
}

#[test]
fn offline_install_fails_without_connecting() {
    let fixture = Fixture::new(true);
    let demo = ops::resolve(&fixture.index, &names(&["demo"])).unwrap();
    let actions = ops::install(&fixture.env(), &demo).unwrap();
    assert_eq!(actions.results[0].action, "failed");
    assert!(actions.results[0].detail.as_deref().unwrap().contains("offline"));
    assert!(!actions.ok);
    assert_eq!(fixture.connects.load(Ordering::SeqCst), 0);
}

#[test]
fn sync_is_a_check_when_offline() {
    let fixture = Fixture::new(true);
    let missing = ops::sync(&fixture.env(), &names(&["demo"])).unwrap();
    assert!(!missing.ok);
    // A host remembers what `ensure` answered for the rest of its run, as one CLI invocation does.
    let fixture = Fixture::new(true);
    fixture.seed(DEMO_SHA);
    let present = ops::sync(&fixture.env(), &names(&["demo"])).unwrap();
    assert!(present.ok);
    assert_eq!(present.results[0].action, "already_installed");
}

#[test]
fn installed_state_reports_size_path_and_verification() {
    let fixture = Fixture::new(true);
    fixture.seed(DEMO_SHA);
    let listing = ops::status(&fixture.env(), &names(&["demo"]), true);
    let row = &listing.components[0];
    assert_eq!(row.state, State::Installed);
    assert!(row.size_bytes.is_some());
    assert_eq!(row.path.as_deref(), Some(fixture.object(DEMO_SHA).as_path()));
    assert_eq!(row.verified, Some(false), "a stub archive must fail verification");
    assert!(!listing.ok());
    assert_eq!(fixture.connects.load(Ordering::SeqCst), 0);
}

#[test]
fn uninstall_removes_the_cached_archive() {
    let fixture = Fixture::new(true);
    fixture.seed(DEMO_SHA);
    let demo = ops::resolve(&fixture.index, &names(&["demo"])).unwrap();
    let first = ops::uninstall(&fixture.env(), &demo).unwrap();
    assert_eq!(first.results[0].action, "removed");
    assert!(!fixture.object(DEMO_SHA).exists());
    let second = ops::uninstall(&fixture.env(), &demo).unwrap();
    assert_eq!(second.results[0].action, "not_installed");
}

#[test]
fn bundled_components_install_as_a_noop_with_a_note() {
    let fixture = Fixture::new(false);
    let mut env = fixture.env();
    env.host = None;
    env.no_host = Some("no lock".into());
    let baked = ops::resolve(&fixture.index, &names(&["baked"])).unwrap();
    let actions = ops::install(&env, &baked).unwrap();
    assert_eq!(actions.results[0].action, "bundled");
    assert!(actions.ok);
    assert!(!actions.notes.is_empty());
    let demo = ops::resolve(&fixture.index, &names(&["demo"])).unwrap();
    assert!(ops::install(&env, &demo).unwrap_err().to_string().contains("no lock"));
}

#[test]
fn prune_keeps_referenced_and_recent_unless_all() {
    let fixture = Fixture::new(true);
    fixture.seed(DEMO_SHA);
    fixture.seed(ORPHAN_SHA);

    let dry = ops::prune(&fixture.env(), true, true, 30).unwrap();
    assert_eq!(dry.results.len(), 2);
    assert!(fixture.object(DEMO_SHA).exists() && fixture.object(ORPHAN_SHA).exists());

    let kept = ops::prune(&fixture.env(), false, false, 30).unwrap();
    assert!(kept.results.is_empty());
    assert!(fixture.object(ORPHAN_SHA).exists());

    let all = ops::prune(&fixture.env(), true, false, 30).unwrap();
    assert_eq!(all.results.len(), 2);
    assert!(!fixture.object(DEMO_SHA).exists() && !fixture.object(ORPHAN_SHA).exists());
}

#[test]
fn resolve_expands_presets_and_rejects_unknown_names() {
    let index = index();
    let all: Vec<_> = ops::resolve(&index, &names(&["all"])).unwrap();
    assert_eq!(all.len(), 2);
    let error = ops::resolve(&index, &names(&["nope"])).unwrap_err().to_string();
    assert!(error.contains("`nope`") && error.contains("demo"), "{error}");
}

#[test]
fn flavor_is_partial_when_some_families_are_linked() {
    let fixture = Fixture::new(true);
    assert_eq!(ops::flavor(&fixture.env()), "partial");
}

#[test]
fn import_rejects_an_archive_the_lock_does_not_name() {
    let fixture = Fixture::new(true);
    let archive = fixture.tmp.path().join("x.tar.gz");
    std::fs::write(&archive, b"not an archive").unwrap();
    let error = ops::import(&fixture.env(), &archive).unwrap_err().to_string();
    assert!(error.contains("matches no component"), "{error}");
}

const OLD_LOCK_SHA: &str = "efefefefefefefefefefefefefefefefefefefefefefefefefefefefefefefef";
const FRESH_SHA: &str = "0101010101010101010101010101010101010101010101010101010101010101";

impl Fixture {
    fn used(&self, sha: &str, days_ago: u64) {
        let stamp = self.root().join("sha256").join(format!("{sha}.used"));
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(stamp)
            .unwrap();
        file.set_modified(SystemTime::now() - Duration::from_secs(days_ago * 86_400))
            .unwrap();
    }

    fn staging(&self, name: &str, age: Duration) -> PathBuf {
        let path = self.root().join("sha256").join(name);
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("partial"), "xx").unwrap();
        let mut options = std::fs::OpenOptions::new();
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt as _;
            // FILE_FLAG_BACKUP_SEMANTICS: required to open a directory handle on Windows.
            options.write(true).custom_flags(0x0200_0000);
        }
        #[cfg(not(windows))]
        options.read(true);
        let file = options.open(&path).unwrap();
        file.set_modified(SystemTime::now() - age).unwrap();
        path
    }
}

fn row_names(actions: &ops::Actions, action: &str) -> Vec<String> {
    let mut names: Vec<_> = actions
        .results
        .iter()
        .filter(|r| r.action == action)
        .map(|r| r.name.clone())
        .collect();
    names.sort();
    names
}

#[test]
fn prune_removes_stale_unreferenced_and_keeps_fresh_and_locked() {
    let fixture = Fixture::new(true);
    for sha in [DEMO_SHA, ORPHAN_SHA, FRESH_SHA] {
        fixture.seed(sha);
    }
    fixture.used(DEMO_SHA, 90);
    fixture.used(ORPHAN_SHA, 90);
    fixture.used(FRESH_SHA, 1);

    let actions = ops::prune(&fixture.env(), false, false, 30).unwrap();

    assert_eq!(row_names(&actions, "removed"), [ORPHAN_SHA]);
    assert!(fixture.object(DEMO_SHA).exists(), "the lock references it");
    assert!(fixture.object(FRESH_SHA).exists(), "used yesterday");
    assert!(!fixture.object(ORPHAN_SHA).exists());
    assert!(
        !fixture
            .root()
            .join("sha256")
            .join(format!("{ORPHAN_SHA}.used"))
            .exists(),
        "the stamp goes with the object"
    );
}

#[test]
fn prune_with_two_lock_versions_keeps_the_current_locks_entry_only() {
    // The cache is shared with an older poly whose lock named OLD_LOCK_SHA; this poly's lock names
    // DEMO_SHA only. Both are stale: the current lock's entry survives, the older one's goes.
    let fixture = Fixture::new(true);
    fixture.seed(DEMO_SHA);
    fixture.seed(OLD_LOCK_SHA);
    fixture.used(DEMO_SHA, 400);
    fixture.used(OLD_LOCK_SHA, 400);

    let actions = ops::prune(&fixture.env(), false, false, 30).unwrap();

    assert_eq!(row_names(&actions, "removed"), [OLD_LOCK_SHA]);
    assert!(fixture.object(DEMO_SHA).exists());

    fixture.seed(OLD_LOCK_SHA);
    fixture.used(OLD_LOCK_SHA, 2);
    let fresh = ops::prune(&fixture.env(), false, false, 30).unwrap();
    assert!(fresh.results.is_empty(), "recently used by the older poly: kept");
}

#[test]
fn prune_keeps_a_stale_object_the_lock_names_for_another_target() {
    // A cache shared across targets (a synced home directory) must not lose another target's
    // locked archive just because this target never loads it.
    let fixture = Fixture::new(true);
    fixture.seed(OTHER_TARGET_SHA);
    fixture.used(OTHER_TARGET_SHA, 400);

    let actions = ops::prune(&fixture.env(), false, false, 30).unwrap();

    assert!(row_names(&actions, "removed").is_empty(), "{:?}", actions.results);
    assert!(fixture.object(OTHER_TARGET_SHA).exists());
}

#[test]
fn prune_removes_old_staging_dirs_and_keeps_young_ones() {
    let fixture = Fixture::new(true);
    let old = fixture.staging(".install-crashed", Duration::from_secs(2 * 3600));
    let young = fixture.staging(".install-live", Duration::from_secs(60));
    let orphan_sidecar = fixture.root().join("sha256").join(format!("{ORPHAN_SHA}.verified"));
    std::fs::write(&orphan_sidecar, "{}").unwrap();

    let actions = ops::prune(&fixture.env(), false, false, 30).unwrap();

    assert_eq!(
        row_names(&actions, "removed"),
        [".install-crashed".to_owned(), format!("{ORPHAN_SHA}.verified")]
    );
    assert!(!old.exists() && !orphan_sidecar.exists());
    assert!(young.exists(), "a young staging dir may belong to a live install");
}

#[test]
fn dry_run_names_exactly_what_a_real_prune_removes() {
    let fixture = Fixture::new(true);
    for sha in [DEMO_SHA, ORPHAN_SHA, OLD_LOCK_SHA, FRESH_SHA] {
        fixture.seed(sha);
    }
    fixture.used(DEMO_SHA, 90);
    fixture.used(ORPHAN_SHA, 90);
    fixture.used(OLD_LOCK_SHA, 90);
    fixture.used(FRESH_SHA, 1);
    fixture.staging(".install-crashed", Duration::from_secs(2 * 3600));

    let dry = ops::prune(&fixture.env(), false, true, 30).unwrap();
    assert!(fixture.object(ORPHAN_SHA).exists(), "a dry run removes nothing");
    let real = ops::prune(&fixture.env(), false, false, 30).unwrap();

    let would = row_names(&dry, "would_remove");
    assert_eq!(would.len(), 3, "{would:?}");
    assert_eq!(would, row_names(&real, "removed"));
    assert_eq!(dry.freed_bytes, real.freed_bytes);
}

#[test]
fn sharing_violations_are_recognised_on_every_platform() {
    assert!(prune::is_sharing_violation(&std::io::Error::from_raw_os_error(32)));
    assert!(prune::is_sharing_violation(&std::io::Error::from_raw_os_error(33)));
    assert!(!prune::is_sharing_violation(&std::io::Error::other("boom")));
    assert!(!prune::is_sharing_violation(&std::io::Error::from(
        std::io::ErrorKind::NotFound
    )));
}

#[test]
fn a_sharing_violation_is_skipped_with_a_note_not_an_error() {
    let fixture = Fixture::new(true);
    fixture.seed(ORPHAN_SHA);
    fixture.seed(OLD_LOCK_SHA);
    fixture.used(ORPHAN_SHA, 90);
    fixture.used(OLD_LOCK_SHA, 90);
    let held = fixture.object(ORPHAN_SHA);
    let remover = |path: &std::path::Path| {
        if path == held {
            Err(std::io::Error::from_raw_os_error(32))
        } else {
            prune::remove_path(path)
        }
    };

    let actions = ops::prune_with(&fixture.env(), false, false, 30, &remover).unwrap();

    assert!(actions.ok, "a skipped removal is not a failure");
    assert_eq!(row_names(&actions, "skipped"), [ORPHAN_SHA]);
    assert_eq!(row_names(&actions, "removed"), [OLD_LOCK_SHA]);
    assert!(fixture.object(ORPHAN_SHA).exists());
    assert!(!actions.notes.is_empty());

    let later = ops::prune(&fixture.env(), false, false, 30).unwrap();
    assert_eq!(row_names(&later, "removed"), [ORPHAN_SHA], "the next prune retries it");
}

#[test]
fn any_other_removal_error_fails_the_prune() {
    let fixture = Fixture::new(true);
    fixture.seed(ORPHAN_SHA);
    fixture.used(ORPHAN_SHA, 90);
    let remover = |_: &std::path::Path| Err(std::io::Error::other("disk on fire"));
    assert!(ops::prune_with(&fixture.env(), false, false, 30, &remover).is_err());
}

#[cfg(windows)]
#[test]
fn a_file_held_open_on_windows_is_skipped() {
    let fixture = Fixture::new(true);
    fixture.seed(ORPHAN_SHA);
    fixture.used(ORPHAN_SHA, 90);
    use std::os::windows::fs::OpenOptionsExt as _;
    // Share mode 0 refuses deletes, as a loaded DLL does.
    let held = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(fixture.object(ORPHAN_SHA).join("component.json"))
        .unwrap();

    let actions = ops::prune(&fixture.env(), false, false, 30).unwrap();

    assert!(actions.ok);
    assert_eq!(row_names(&actions, "skipped"), [ORPHAN_SHA]);
    drop(held);
    let later = ops::prune(&fixture.env(), false, false, 30).unwrap();
    assert_eq!(row_names(&later, "removed"), [ORPHAN_SHA]);
}
