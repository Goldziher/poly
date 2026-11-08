//! Re-verifying and repairing a damaged cached component, over a real archive on a `file://` URL.

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use alef_component_runtime::{
    COMPONENT_ABI_VERSION, COMPONENT_MANIFEST_SCHEMA, ComponentIdentity, ComponentLibrary, ComponentManifest,
};
use poly_component_host::{Availability, CacheCheck, ComponentHost, HostOptions, UnavailableKind};
use sha2::{Digest, Sha256};

const TARGET: &str = "test-target";
const ID: &str = "demo-component";
const LIBRARY: &[u8] = b"native library bytes";

fn sha(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn append(builder: &mut tar::Builder<flate2::write::GzEncoder<&mut Vec<u8>>>, path: &str, content: &[u8]) {
    let mut header = tar::Header::new_gnu();
    header.set_size(content.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    builder.append_data(&mut header, path, Cursor::new(content)).unwrap();
}

/// An unsigned component archive on disk and the lock that names it.
fn archive(dir: &Path) -> String {
    let identity = ComponentIdentity {
        crate_name: "c".into(),
        component: ID.into(),
        version: "1.2.3".into(),
        target: TARGET.into(),
        feature_hash: "11".repeat(32),
        contract_hash: "22".repeat(32),
    };
    let manifest = ComponentManifest {
        schema_version: COMPONENT_MANIFEST_SCHEMA,
        abi_version: COMPONENT_ABI_VERSION,
        identity: identity.clone(),
        provides: Vec::new(),
        features: Vec::new(),
        default_features: false,
        library: ComponentLibrary {
            file: "libdemo.so".into(),
            sha256: sha(LIBRARY),
            size: LIBRARY.len() as u64,
        },
    };
    let manifest_bytes = manifest.canonical_bytes().unwrap();
    let mut compressed = Vec::new();
    {
        let encoder = flate2::write::GzEncoder::new(&mut compressed, flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);
        append(&mut builder, "component.json", &manifest_bytes);
        append(&mut builder, "libdemo.so", LIBRARY);
        builder.into_inner().unwrap().finish().unwrap();
    }
    let path = dir.join("demo.tar.gz");
    std::fs::write(&path, &compressed).unwrap();
    serde_json::json!({
        "schema_version": 2,
        "public_keys": {},
        "artifacts": [{
            "identity": identity,
            "provides": [],
            "mode": "download",
            "url": format!("file://{}", path.display()),
            "sha256": sha(&compressed),
            "size": compressed.len(),
            "manifest_sha256": sha(&manifest_bytes),
            "key_id": "",
        }],
    })
    .to_string()
}

struct Fixture {
    lock: String,
    cache: PathBuf,
    _dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let lock = archive(dir.path());
        let cache = dir.path().join("cache");
        Self { lock, cache, _dir: dir }
    }

    fn host(&self, offline: bool) -> ComponentHost {
        let mut options = HostOptions::new(self.lock.clone(), self.cache.clone());
        options.target = Some(TARGET.into());
        options.offline = offline;
        ComponentHost::new(options).unwrap()
    }

    fn installed(&self) -> ComponentHost {
        let host = self.host(false);
        assert_eq!(host.ensure_frontend(ID, true), Availability::InstalledNow);
        assert_eq!(host.check_cache(ID).unwrap(), CacheCheck::Intact);
        host
    }

    fn object(&self, host: &ComponentHost) -> PathBuf {
        host.manager().cache_path(ID).unwrap()
    }
}

/// Write `bytes` and move the mtime forward, so the verified stamp cannot mistake the edit for
/// the file it recorded on a filesystem with coarse timestamps.
fn overwrite(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).unwrap();
    let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    file.set_modified(SystemTime::now() + Duration::from_secs(30)).unwrap();
}

fn flip_library_byte(object: &Path) {
    let mut bytes = std::fs::read(object.join("libdemo.so")).unwrap();
    bytes[0] ^= 0xff;
    overwrite(&object.join("libdemo.so"), &bytes);
}

fn truncate_library(object: &Path) {
    overwrite(&object.join("libdemo.so"), &LIBRARY[..4]);
}

fn edit_manifest(object: &Path) {
    let mut bytes = std::fs::read(object.join("component.json")).unwrap();
    bytes.push(b' ');
    overwrite(&object.join("component.json"), &bytes);
}

type Tamper = fn(&Path);

const TAMPERS: [(&str, Tamper); 3] = [
    ("flipped library byte", flip_library_byte),
    ("truncated library", truncate_library),
    ("edited manifest", edit_manifest),
];

#[test]
fn an_intact_copy_is_present_and_a_missing_one_is_not_cached() {
    let f = Fixture::new();
    assert_eq!(f.host(true).check_cache(ID).unwrap(), CacheCheck::NotCached);
    f.installed();
    let again = f.host(true);
    assert_eq!(again.ensure_frontend(ID, true), Availability::Present);
    assert!(!again.was_repaired(ID));
    assert_eq!(again.locked_version(ID).as_deref(), Some("1.2.3"));
}

#[test]
fn every_kind_of_damage_is_found_and_named_offline() {
    for (what, tamper) in TAMPERS {
        let f = Fixture::new();
        let host = f.installed();
        tamper(&f.object(&host));
        let offline = f.host(true);
        let CacheCheck::Corrupt(detail) = offline.check_cache(ID).unwrap() else {
            panic!("{what}: not detected");
        };
        assert!(detail.contains("failed verification"), "{what}: {detail}");
        let Availability::Unavailable(unavailable) = offline.ensure_frontend(ID, true) else {
            panic!("{what}: an offline run used a damaged component");
        };
        assert_eq!(unavailable.kind, UnavailableKind::VerifyFailed, "{what}");
        let reason = unavailable.reason(ID);
        assert!(
            reason.contains("failed verification") && reason.contains("poly components install demo-component"),
            "{what}: {reason}"
        );
        assert!(
            !reason.contains("network access is disabled"),
            "{what}: blamed the network: {reason}"
        );
    }
}

#[test]
fn install_never_refuses_a_damaged_copy_with_the_remedy() {
    let f = Fixture::new();
    let host = f.installed();
    flip_library_byte(&f.object(&host));
    let Availability::Unavailable(unavailable) = f.host(false).ensure_frontend(ID, false) else {
        panic!("a damaged component was used");
    };
    assert_eq!(unavailable.kind, UnavailableKind::VerifyFailed);
    assert!(
        unavailable.detail.contains("install = \"never\""),
        "{}",
        unavailable.detail
    );
}

#[test]
fn an_online_ensure_repairs_a_damaged_copy_and_says_so() {
    for (what, tamper) in TAMPERS {
        let f = Fixture::new();
        let host = f.installed();
        tamper(&f.object(&host));
        let online = f.host(false);
        assert_eq!(online.ensure_frontend(ID, true), Availability::InstalledNow, "{what}");
        assert!(online.was_repaired(ID), "{what}");
        assert_eq!(online.check_cache(ID).unwrap(), CacheCheck::Intact, "{what}");
        assert_eq!(f.host(true).ensure_frontend(ID, true), Availability::Present, "{what}");
    }
}

#[cfg(unix)]
#[test]
fn an_unreadable_copy_is_an_error_not_corruption() {
    use std::os::unix::fs::PermissionsExt;

    let f = Fixture::new();
    let host = f.installed();
    let object = f.object(&host);
    let before = std::fs::metadata(&object).unwrap().permissions();
    std::fs::set_permissions(&object, std::fs::Permissions::from_mode(0o000)).unwrap();
    let readable = std::fs::read_dir(&object).is_ok();
    let offline = f.host(true);
    let result = offline.check_cache(ID);
    std::fs::set_permissions(&object, before).unwrap();
    if readable {
        return; // running as root: permissions do not apply
    }
    let error = result.expect_err("an I/O failure must not read as damage");
    assert!(format!("{error:#}").contains("cached copy"), "{error:#}");
    assert_eq!(
        offline.check_cache(ID).unwrap(),
        CacheCheck::Intact,
        "nothing was removed"
    );
}

#[cfg(unix)]
#[test]
fn an_unreadable_library_is_an_error_not_corruption() {
    use std::os::unix::fs::PermissionsExt;

    let f = Fixture::new();
    let host = f.installed();
    let library = f.object(&host).join("libdemo.so");
    std::fs::set_permissions(&library, std::fs::Permissions::from_mode(0o000)).unwrap();
    let root = std::fs::read(&library).is_ok();
    let result = f.host(true).check_cache(ID);
    std::fs::set_permissions(&library, std::fs::Permissions::from_mode(0o644)).unwrap();
    if root {
        return; // running as root: permissions do not apply
    }
    let error = result.expect_err("an I/O failure must not read as damage");
    assert!(
        format!("{error:#}").contains("could not read the cached copy"),
        "{error:#}"
    );
    assert_eq!(
        f.host(true).check_cache(ID).unwrap(),
        CacheCheck::Intact,
        "nothing was removed"
    );
}

#[test]
fn a_check_that_lands_mid_repair_waits_and_sees_the_repaired_copy() {
    let f = Fixture::new();
    let host = f.installed();
    let library = f.object(&host).join("libdemo.so");
    let saved = std::fs::read(&library).unwrap();
    std::fs::remove_file(&library).unwrap();
    let restore = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(40));
        std::fs::write(&library, saved).unwrap();
    });
    let check = f.host(true).check_cache(ID).unwrap();
    restore.join().unwrap();
    assert_eq!(check, CacheCheck::Intact);
}
