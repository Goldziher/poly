//! HTTP for tool provisioning: every fetch retries transient failures with the
//! shared exponential backoff, honours `Retry-After`, and is bounded by connect,
//! stall and overall deadlines, so a flaky registry neither fails a run on the
//! first 503 nor hangs it on a stalled transfer.
//!
//! Retried: connection errors, timeouts, 429, 500, 502, 503, 504. Not retried: any
//! other 4xx, and a GitHub primary rate limit (its reset is minutes away).

use std::fs::File;
use std::io::{Read, Seek, Write};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use super::unit::{Failed, Retry};

/// Tries per fetch, the first included.
const ATTEMPTS: u32 = 3;
/// Seconds before the second try; doubles per failure, plus up to [`JITTER_SECS`].
const BACKOFF_SECS: u64 = 1;
const JITTER_SECS: u64 = 1;
/// Longest `Retry-After` poly will sleep; a longer ask fails the fetch instead.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(30);

/// Largest text body (API response, checksum file) read into memory.
const MAX_TEXT_BODY: u64 = 16 * 1024 * 1024;

/// How patient a fetch is.
#[derive(Debug, Clone, Copy)]
pub(super) struct Policy {
    pub(super) retry: Retry,
    /// Time to open a connection.
    pub(super) connect: Duration,
    /// Longest silence while waiting for or reading data.
    pub(super) stall: Duration,
    /// Overall deadline of an API or checksum request.
    pub(super) request: Duration,
    /// Overall deadline of one download attempt.
    pub(super) download: Duration,
    /// Cap on a honoured `Retry-After`.
    pub(super) max_retry_after: Duration,
}

impl Policy {
    pub(super) const PRODUCTION: Policy = Policy {
        retry: Retry::new(ATTEMPTS, BACKOFF_SECS, JITTER_SECS),
        connect: Duration::from_secs(10),
        stall: Duration::from_secs(30),
        request: Duration::from_secs(60),
        download: Duration::from_secs(600),
        max_retry_after: MAX_RETRY_AFTER,
    };
}

/// A GitHub rate-limit response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RateLimit {
    /// Unix time at which the quota resets.
    pub(super) reset: Option<u64>,
}

/// Why a fetch failed, after its retries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum FetchError {
    /// The server answered with a failing status.
    Status {
        url: String,
        status: u16,
        rate_limit: Option<RateLimit>,
    },
    /// The request never completed.
    Transport {
        url: String,
        message: String,
        timed_out: bool,
    },
}

impl FetchError {
    /// The HTTP status, when the server answered.
    pub(super) fn status(&self) -> Option<u16> {
        match self {
            FetchError::Status { status, .. } => Some(*status),
            FetchError::Transport { .. } => None,
        }
    }

    /// Whether the failure says nothing about the resource itself: the network
    /// was unreachable, slow, or the server was failing.
    pub(super) fn is_unavailable(&self) -> bool {
        match self {
            FetchError::Transport { .. } => true,
            FetchError::Status { status, rate_limit, .. } => rate_limit.is_some() || retryable_status(*status),
        }
    }
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchError::Status {
                url,
                status,
                rate_limit: Some(limit),
            } => write!(f, "GET {url} was rate limited (HTTP {status}){}", reset_note(*limit)),
            FetchError::Status { url, status, .. } => write!(f, "GET {url} answered HTTP {status}"),
            FetchError::Transport {
                url,
                message,
                timed_out: true,
            } => write!(f, "GET {url} timed out ({message})"),
            FetchError::Transport { url, message, .. } => write!(f, "GET {url} failed ({message})"),
        }
    }
}

impl std::error::Error for FetchError {}

fn reset_note(limit: RateLimit) -> String {
    let Some(reset) = limit.reset else {
        return String::new();
    };
    let now = unix_now();
    let wait = reset.saturating_sub(now).div_ceil(60);
    format!("; the quota resets at {} (in {wait} min)", utc(reset))
}

/// `YYYY-MM-DD HH:MM UTC` of a Unix time.
pub(super) fn utc(seconds: u64) -> String {
    let days = (seconds / 86_400) as i64;
    let rest = seconds % 86_400;
    // Howard Hinnant's civil-from-days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
        rest / 3_600,
        rest % 3_600 / 60
    )
}

pub(super) fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn retryable_status(status: u16) -> bool {
    matches!(status, 429 | 500 | 502 | 503 | 504)
}

/// A successful answer: any 2xx, or a 304 to a conditional request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Reply {
    pub(super) status: u16,
    pub(super) etag: Option<String>,
    pub(super) last_modified: Option<String>,
    pub(super) body: String,
}

impl Reply {
    pub(super) fn not_modified(&self) -> bool {
        self.status == 304
    }
}

/// The HTTP client of tool provisioning.
#[derive(Clone)]
pub(super) struct Http {
    policy: Policy,
    sleep: Arc<dyn Fn(Duration) + Send + Sync>,
}

impl Http {
    pub(super) fn production() -> Self {
        Http::with(Policy::PRODUCTION, Arc::new(std::thread::sleep))
    }

    /// A client with its own policy and a replaceable sleep, so tests neither
    /// wait out a backoff nor lose track of how long one was asked for.
    pub(super) fn with(policy: Policy, sleep: Arc<dyn Fn(Duration) + Send + Sync>) -> Self {
        Http { policy, sleep }
    }

    fn agent(&self, deadline: Duration) -> ureq::Agent {
        ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_connect(Some(self.policy.connect))
            .timeout_per_read(Some(self.policy.stall))
            .timeout_global(Some(deadline))
            .user_agent(concat!("poly/", env!("CARGO_PKG_VERSION")))
            .build()
            .into()
    }

    /// GET `url` as text with the given request headers. A 304 is a success: the
    /// caller asked a conditional question.
    pub(super) fn get_text(&self, url: &str, headers: &[(&str, &str)]) -> Result<Reply, FetchError> {
        let agent = self.agent(self.policy.request);
        self.policy
            .retry
            .run_with(
                || {
                    let mut request = agent.get(url);
                    for (name, value) in headers {
                        request = request.header(*name, *value);
                    }
                    let mut response = request.call().map_err(|error| transport(url, &error))?;
                    let status = response.status().as_u16();
                    if !(200..300).contains(&status) && status != 304 {
                        return Err(self.reject(url, status, &response));
                    }
                    let etag = header(&response, "etag");
                    let last_modified = header(&response, "last-modified");
                    let body = response
                        .body_mut()
                        .with_config()
                        .limit(MAX_TEXT_BODY)
                        .read_to_string()
                        .map_err(|error| transport(url, &error))?;
                    Ok(Reply {
                        status,
                        etag,
                        last_modified,
                        body,
                    })
                },
                |wait| (self.sleep)(wait),
            )
            .map_err(|failed| failed.error)
    }

    /// Stream `url` into `target`, returning the hex SHA-256 of what was
    /// written. Each attempt starts the file over.
    pub(super) fn download(&self, url: &str, target: &File) -> Result<String, FetchError> {
        let agent = self.agent(self.policy.download);
        self.policy
            .retry
            .run_with(
                || {
                    let mut file = target;
                    file.set_len(0).map_err(|error| local(url, &error))?;
                    file.rewind().map_err(|error| local(url, &error))?;
                    let mut response = agent.get(url).call().map_err(|error| transport(url, &error))?;
                    let status = response.status().as_u16();
                    if !(200..300).contains(&status) {
                        return Err(self.reject(url, status, &response));
                    }
                    let mut reader = response.body_mut().as_reader();
                    let mut writer = std::io::BufWriter::new(file);
                    let mut hasher = Sha256::new();
                    let mut buffer = vec![0u8; 64 * 1024];
                    loop {
                        let read = reader.read(&mut buffer).map_err(|error| transport(url, &error))?;
                        if read == 0 {
                            break;
                        }
                        hasher.update(&buffer[..read]);
                        writer.write_all(&buffer[..read]).map_err(|error| local(url, &error))?;
                    }
                    writer.flush().map_err(|error| local(url, &error))?;
                    Ok(hex(&hasher.finalize()))
                },
                |wait| (self.sleep)(wait),
            )
            .map_err(|failed| failed.error)
    }

    fn reject(&self, url: &str, status: u16, response: &ureq::http::Response<ureq::Body>) -> Failed<FetchError> {
        let rate_limit = (header(response, "x-ratelimit-remaining").as_deref() == Some("0")).then(|| RateLimit {
            reset: header(response, "x-ratelimit-reset").and_then(|value| value.parse().ok()),
        });
        let asked = header(response, "retry-after").and_then(|value| value.trim().parse::<u64>().ok());
        let error = FetchError::Status {
            url: url.to_owned(),
            status,
            rate_limit,
        };
        let waits = asked.map(Duration::from_secs);
        if waits.is_some_and(|wait| wait > self.policy.max_retry_after) {
            return Failed::fatal(error);
        }
        if let (Some(_), None) = (rate_limit, waits) {
            return Failed::fatal(error);
        }
        let secondary_limit = status == 403 && waits.is_some();
        if retryable_status(status) || secondary_limit {
            Failed {
                error,
                retryable: true,
                retry_after: waits,
            }
        } else {
            Failed::fatal(error)
        }
    }
}

fn header(response: &ureq::http::Response<ureq::Body>, name: &str) -> Option<String> {
    response
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.trim().to_owned())
}

fn timed_out(message: &str) -> bool {
    let lowered = message.to_ascii_lowercase();
    lowered.contains("timeout") || lowered.contains("timed out")
}

fn transport(url: &str, error: &(dyn std::error::Error + 'static)) -> Failed<FetchError> {
    let mut message = error.to_string();
    let mut source = error.source();
    while let Some(inner) = source {
        message.push_str(": ");
        message.push_str(&inner.to_string());
        source = inner.source();
    }
    Failed::transient(FetchError::Transport {
        url: url.to_owned(),
        timed_out: timed_out(&message),
        message,
    })
}

/// A failure writing the destination: not the network's fault, so no retry.
fn local(url: &str, error: &std::io::Error) -> Failed<FetchError> {
    Failed::fatal(FetchError::Transport {
        url: url.to_owned(),
        message: format!("writing the download: {error}"),
        timed_out: false,
    })
}

pub(super) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_unix_time_renders_as_utc() {
        assert_eq!(utc(0), "1970-01-01 00:00 UTC");
        assert_eq!(utc(1_700_000_000), "2023-11-14 22:13 UTC");
    }

    #[test]
    fn only_transient_statuses_are_retryable() {
        for status in [429, 500, 502, 503, 504] {
            assert!(retryable_status(status), "{status}");
        }
        for status in [400, 401, 403, 404, 410] {
            assert!(!retryable_status(status), "{status}");
        }
    }
}
