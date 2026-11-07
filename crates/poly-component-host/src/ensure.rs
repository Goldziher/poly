//! Making components available before a run plans, and keeping every later load offline.
//!
//! [`ComponentHost::ensure`] is the only place a component is installed. A host built with
//! [`HostOptions::gate_loads`](crate::HostOptions::gate_loads) refuses to load any component
//! `ensure` did not report present, so the per-file workers, which only ever load, cannot reach
//! the network: an unensured component fails with a message instead.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use alef_component_runtime::{ComponentError, ComponentStatus};

use crate::host::ComponentHost;
use crate::transport::ComponentTransport;

/// Why a component cannot be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnavailableKind {
    /// Not installed, and the run is offline.
    Offline,
    /// Not installed, and `[components] install = "never"`.
    InstallNever,
    /// The download failed.
    DownloadFailed,
    /// The archive arrived but failed size, digest, signature or manifest verification.
    VerifyFailed,
    /// No artifact for this target, or its contract or ABI does not match this poly.
    Incompatible,
}

impl UnavailableKind {
    /// The stable word reports use: `offline`, `install_never`, `download_failed`,
    /// `verify_failed` or `incompatible`.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Offline => "offline",
            Self::InstallNever => "install_never",
            Self::DownloadFailed => "download_failed",
            Self::VerifyFailed => "verify_failed",
            Self::Incompatible => "incompatible",
        }
    }

    /// Whether the component was never attempted because policy forbade it
    /// ([`Offline`](Self::Offline), [`InstallNever`](Self::InstallNever)): the cases
    /// `on_missing = "skip"` may turn into skips. An attempted install that failed is an error
    /// whatever `on_missing` says.
    #[must_use]
    pub const fn is_not_installed(self) -> bool {
        matches!(self, Self::Offline | Self::InstallNever)
    }
}

/// A component that cannot be used, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unavailable {
    /// The category.
    pub kind: UnavailableKind,
    /// The underlying message; empty for the two policy kinds.
    pub detail: String,
}

impl Unavailable {
    /// The reason a report quotes: `component not installed: <name> (offline)` for a policy
    /// kind, `<kind>: <name>: <detail>` for a failure.
    #[must_use]
    pub fn reason(&self, component: &str) -> String {
        if self.kind.is_not_installed() {
            format!("component not installed: {component} ({})", self.kind.label())
        } else {
            format!("{}: {component}: {}", self.kind.label(), self.detail)
        }
    }
}

/// The outcome of [`ComponentHost::ensure`] for one component.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Availability {
    /// Already on disk and verified, or bundled: loads without the network.
    Present,
    /// Downloaded and verified by this run.
    InstalledNow,
    /// Cannot be used.
    Unavailable(Unavailable),
}

impl Availability {
    /// Whether the component can be loaded.
    #[must_use]
    pub const fn is_usable(&self) -> bool {
        matches!(self, Self::Present | Self::InstalledNow)
    }
}

/// State shared between a host and the loaders of its slots.
pub(crate) struct Shared {
    pub(crate) gate: bool,
    sealed: AtomicBool,
    ensured: Mutex<HashMap<String, Availability>>,
    connected: Mutex<HashMap<String, Arc<dyn ComponentTransport>>>,
}

impl Shared {
    pub(crate) fn new(gate: bool) -> Self {
        Self {
            gate,
            sealed: AtomicBool::new(false),
            ensured: Mutex::new(HashMap::new()),
            connected: Mutex::new(HashMap::new()),
        }
    }

    fn recorded(&self, id: &str) -> Option<Availability> {
        self.ensured
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(id)
            .cloned()
    }

    fn record(&self, id: &str, availability: Availability) -> Availability {
        self.ensured
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id.to_owned(), availability.clone());
        availability
    }

    /// The transport `ensure` already connected, handed over once.
    pub(crate) fn take_connected(&self, id: &str) -> Option<Arc<dyn ComponentTransport>> {
        self.connected.lock().unwrap_or_else(PoisonError::into_inner).remove(id)
    }

    /// Whether a slot may call the connector for `id`: always on an ungated host, otherwise only
    /// for a component `ensure` reported usable.
    pub(crate) fn may_load(&self, id: &str) -> bool {
        !self.gate || self.recorded(id).is_some_and(|a| a.is_usable())
    }
}

/// Classify an alef error by its stable kind. An error that is no [`ComponentError`] is a
/// download failure unless the archive is `installed`: then the connector got past the download
/// and failed its contract, `Init` or `describe` check.
fn classify(error: &anyhow::Error, installed: bool) -> Unavailable {
    let kind = match error.chain().find_map(|cause| cause.downcast_ref::<ComponentError>()) {
        Some(ComponentError::Offline { .. }) => UnavailableKind::Offline,
        Some(ComponentError::Download { .. } | ComponentError::Cancelled | ComponentError::Io(_)) => {
            UnavailableKind::DownloadFailed
        }
        None if installed => UnavailableKind::Incompatible,
        None => UnavailableKind::DownloadFailed,
        Some(
            ComponentError::ArtifactNotFound { .. }
            | ComponentError::ComponentsNotLocked { .. }
            | ComponentError::BundledComponentNotLoadable { .. }
            | ComponentError::ContractNotProvided { .. }
            | ComponentError::ContractHashMismatch
            | ComponentError::FeatureSetHashMismatch
            | ComponentError::IdentityMismatch { .. }
            | ComponentError::IncompatibleAbi { .. }
            | ComponentError::UnsupportedManifest
            | ComponentError::UnsupportedLockSchema(_)
            | ComponentError::SidecarProtocol(_)
            | ComponentError::SidecarStopped
            | ComponentError::SidecarTimeout
            | ComponentError::SidecarCall { .. }
            | ComponentError::UnsupportedTransport(_)
            | ComponentError::LibraryLoad { .. }
            | ComponentError::MissingEntrypoint(_)
            | ComponentError::InvalidDescriptor(_),
        ) => UnavailableKind::Incompatible,
        Some(_) => UnavailableKind::VerifyFailed,
    };
    Unavailable {
        kind,
        detail: format!("{error:#}"),
    }
}

fn unavailable(kind: UnavailableKind, detail: impl Into<String>) -> Availability {
    Availability::Unavailable(Unavailable {
        kind,
        detail: detail.into(),
    })
}

/// How [`ComponentHost::ensure`] installs a component that is not cached.
#[derive(Clone, Copy)]
enum Install {
    /// Download, verify, load and connect an engine component (the `poly` contract), keeping the
    /// connection for the slot that will use it.
    Connect,
    /// Download and verify only: a frontend is loaded, once, by [`ComponentHost::run_frontend`].
    Fetch,
}

impl ComponentHost {
    /// Make `component_id` usable: report it present, or install it.
    ///
    /// Present means the verified archive is already cached (or the component is bundled), so
    /// nothing is fetched and nothing is loaded. Otherwise, offline fails with
    /// [`UnavailableKind::Offline`], `allow_install = false` with
    /// [`UnavailableKind::InstallNever`], and anything else installs through the connector,
    /// classifying a failure. A connector that installs and then cannot connect (contract, `Init` or
    /// `describe` mismatch) is [`UnavailableKind::Incompatible`], not installed.
    ///
    /// The answer is remembered: a second call never fetches. After [`ComponentHost::seal`] a
    /// component not yet ensured is refused, so the call cannot be made late by accident.
    pub fn ensure(&self, component_id: &str, allow_install: bool) -> Availability {
        self.ensure_as(component_id, allow_install, Install::Connect)
    }

    /// [`ComponentHost::ensure`] for a frontend component (one providing the `poly_frontend`
    /// contract): the same present, offline, install-never and failure answers, but an install
    /// only downloads and verifies the archive. [`ComponentHost::run_frontend`] loads it.
    pub fn ensure_frontend(&self, component_id: &str, allow_install: bool) -> Availability {
        self.ensure_as(component_id, allow_install, Install::Fetch)
    }

    fn ensure_as(&self, component_id: &str, allow_install: bool, install: Install) -> Availability {
        if let Some(done) = self.shared.recorded(component_id) {
            return done;
        }
        if self.shared.sealed.load(Ordering::Acquire) {
            return unavailable(
                UnavailableKind::DownloadFailed,
                "components are installed only before a run is planned",
            );
        }
        let availability = self.ensure_now(component_id, allow_install, install);
        self.shared.record(component_id, availability)
    }

    fn ensure_now(&self, component_id: &str, allow_install: bool, install: Install) -> Availability {
        if let Err(error) = self.manager.lock_entry(component_id) {
            return unavailable(
                UnavailableKind::Incompatible,
                format!("no artifact for this target: {error}"),
            );
        }
        match self.manager.status(component_id) {
            Ok(ComponentStatus::Cached | ComponentStatus::Ready | ComponentStatus::Bundled) => {
                return Availability::Present;
            }
            Ok(ComponentStatus::Unsupported { reason }) => return unavailable(UnavailableKind::Incompatible, reason),
            Ok(ComponentStatus::NotDownloaded) => {}
            Err(error) => return Availability::Unavailable(classify(&anyhow::Error::new(error), false)),
        }
        if self.offline {
            return unavailable(UnavailableKind::Offline, "");
        }
        if !allow_install {
            return unavailable(UnavailableKind::InstallNever, "");
        }
        let installed = match install {
            Install::Connect => self.connector.connect(&self.manager, component_id).map(Some),
            Install::Fetch => self
                .manager
                .prefetch(&[component_id])
                .map(|_| None)
                .map_err(anyhow::Error::new),
        };
        match installed {
            Ok(transport) => {
                if let Some(transport) = transport {
                    crate::loaded::record(component_id);
                    self.shared
                        .connected
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .insert(component_id.to_owned(), transport);
                }
                Availability::InstalledNow
            }
            // `connect` installs and loads (`ensure_contract`), so a component that downloaded but
            // fails its contract check is unusable, not installed.
            Err(error) => {
                let installed = matches!(
                    self.manager.status(component_id),
                    Ok(ComponentStatus::Cached | ComponentStatus::Ready)
                );
                Availability::Unavailable(classify(&error, installed))
            }
        }
    }

    /// Refuse every later install: components are ensured before planning and never again.
    pub fn seal(&self) {
        self.shared.sealed.store(true, Ordering::Release);
    }

    /// What [`ComponentHost::ensure`] reported for `component_id`, if it has run.
    #[must_use]
    pub fn availability(&self, component_id: &str) -> Option<Availability> {
        self.shared.recorded(component_id)
    }
}
