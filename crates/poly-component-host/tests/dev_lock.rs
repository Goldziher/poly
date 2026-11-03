//! Installing from a real lock and archive, gated on `POLY_COMPONENT_DEV_LOCK`.
//!
//! The variable names a component lock whose entries use `file://` archive URLs and include the
//! host target (the lock a development build of a component produces). Without it every test
//! here prints a "skipped" line and passes, so a missing archive is visible rather than silent.

use std::path::{Path, PathBuf};

use poly_component_host::{Availability, ComponentHost, HostOptions, UnavailableKind};
use serde_json::Value;

fn dev_lock() -> Option<(String, Value)> {
    let Some(path) = std::env::var_os("POLY_COMPONENT_DEV_LOCK") else {
        eprintln!("skipped: POLY_COMPONENT_DEV_LOCK is not set");
        return None;
    };
    let text = std::fs::read_to_string(&path).expect("read the dev lock");
    let json = serde_json::from_str(&text).expect("the dev lock is JSON");
    Some((text, json))
}

fn host_target_entry(lock: &Value) -> (usize, String) {
    let target = alef_component_runtime::target::host_target().expect("a supported host");
    let artifacts = lock["artifacts"].as_array().expect("artifacts");
    let index = artifacts
        .iter()
        .position(|a| a["identity"]["target"] == target)
        .unwrap_or_else(|| panic!("the dev lock has no entry for {target}"));
    let id = artifacts[index]["identity"]["component"]
        .as_str()
        .expect("component")
        .to_owned();
    (index, id)
}

fn host(lock: &Value, cache: &Path, offline: bool) -> ComponentHost {
    let mut options = HostOptions::new(lock.to_string(), PathBuf::from(cache));
    options.offline = offline;
    options.gate_loads = true;
    ComponentHost::new(options).expect("a host over the dev lock")
}

fn usable(availability: &Availability) -> bool {
    availability.is_usable()
}

#[test]
fn first_use_from_a_file_lock_installs_and_the_second_run_makes_no_fetch() {
    let Some((_, lock)) = dev_lock() else { return };
    let (_, id) = host_target_entry(&lock);
    let cache = tempfile::tempdir().unwrap();

    let first = host(&lock, cache.path(), false).ensure(&id, true);
    assert_eq!(first, Availability::InstalledNow, "{first:?}");

    // Offline cannot fetch, so a usable answer proves the archive came from the cache.
    let second = host(&lock, cache.path(), true).ensure(&id, true);
    assert_eq!(second, Availability::Present, "{second:?}");
}

#[test]
fn offline_with_an_empty_cache_is_unavailable() {
    let Some((_, lock)) = dev_lock() else { return };
    let (_, id) = host_target_entry(&lock);
    let cache = tempfile::tempdir().unwrap();
    let answer = host(&lock, cache.path(), true).ensure(&id, true);
    assert!(
        matches!(&answer, Availability::Unavailable(u) if u.kind == UnavailableKind::Offline),
        "{answer:?}"
    );
}

#[test]
fn a_tampered_archive_is_rejected() {
    let Some((_, mut lock)) = dev_lock() else { return };
    let (index, id) = host_target_entry(&lock);
    let url = lock["artifacts"][index]["url"].as_str().expect("url").to_owned();
    let source = url.strip_prefix("file://").expect("the dev lock uses file:// URLs");
    let mut bytes = std::fs::read(source).expect("read the archive");
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    let dir = tempfile::tempdir().unwrap();
    let tampered = dir.path().join("tampered.archive");
    std::fs::write(&tampered, bytes).unwrap();
    lock["artifacts"][index]["url"] = format!("file://{}", tampered.display()).into();

    let cache = tempfile::tempdir().unwrap();
    let answer = host(&lock, cache.path(), false).ensure(&id, true);
    assert!(!usable(&answer), "{answer:?}");
    assert!(
        matches!(&answer, Availability::Unavailable(u) if u.kind == UnavailableKind::VerifyFailed),
        "{answer:?}"
    );
}

#[test]
fn a_wrong_contract_hash_is_rejected() {
    let Some((_, mut lock)) = dev_lock() else { return };
    let (index, id) = host_target_entry(&lock);
    lock["artifacts"][index]["identity"]["contract_hash"] = "00".repeat(32).into();

    let cache = tempfile::tempdir().unwrap();
    let answer = host(&lock, cache.path(), false).ensure(&id, true);
    assert!(!usable(&answer), "{answer:?}");
}
