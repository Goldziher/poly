//! Progress notes for the slow parts of a recipe tool: a download into poly's
//! tool cache and a batched invocation. Either can take minutes on a first run
//! (a 40 MB release, `golangci-lint` type-checking a module), and with nothing
//! on stderr a user cannot tell that from a hang.
//!
//! The notes go through `tracing` at info level, the channel poly's other
//! notices use: stderr only, so `--format json`/`toon` stdout is untouched, and
//! raised to warn-only by `--quiet`.

use std::time::Duration;

use super::argv::Items;

/// A unit that finishes faster than this gets no completion note: the start note
/// already said what ran, and a line per fast unit is noise.
const SLOW_UNIT: Duration = Duration::from_secs(2);

/// Before a download: which tool, which release, from where.
pub(super) fn downloading(tool: &str, version: &str, url: &str) {
    tracing::info!("{}", downloading_message(tool, version, url));
}

/// After a verified install: its size on disk and how long it took.
pub(super) fn installed(tool: &str, version: &str, bytes: Option<u64>, elapsed: Duration) {
    tracing::info!("{}", installed_message(tool, version, bytes, elapsed));
}

/// Before one invocation of a batched unit.
pub(super) fn unit_started(tool: &str, items: Items, count: usize, dir: &str) {
    tracing::info!("{}", unit_started_message(tool, items, count, dir));
}

/// After one invocation, when it was slow enough to be worth a line.
pub(super) fn unit_finished(tool: &str, dir: &str, elapsed: Duration) {
    if elapsed >= SLOW_UNIT {
        tracing::info!("{}", unit_finished_message(tool, dir, elapsed));
    }
}

fn downloading_message(tool: &str, version: &str, url: &str) -> String {
    format!("{tool}: downloading release {version} from {url} into poly's tool cache (once per version)")
}

fn installed_message(tool: &str, version: &str, bytes: Option<u64>, elapsed: Duration) -> String {
    let size = bytes.map_or_else(String::new, |bytes| format!(" ({})", human_size(bytes)));
    format!(
        "{tool}: installed release {version}{size} in {:.1}s",
        elapsed.as_secs_f64()
    )
}

fn unit_started_message(tool: &str, items: Items, count: usize, dir: &str) -> String {
    let what = match items {
        Items::Files if count == 1 => "1 file".to_owned(),
        Items::Files => format!("{count} files"),
        Items::Dirs if count == 1 => "1 directory".to_owned(),
        Items::Dirs => format!("{count} directories"),
        Items::Nothing => return format!("{tool}: running in {dir}"),
    };
    format!("{tool}: running on {what} in {dir}")
}

fn unit_finished_message(tool: &str, dir: &str, elapsed: Duration) -> String {
    format!("{tool}: finished {dir} in {:.1}s", elapsed.as_secs_f64())
}

#[allow(clippy::cast_precision_loss)] // a display value; exactness past 2^53 bytes is irrelevant
fn human_size(bytes: u64) -> String {
    const MIB: f64 = 1024.0 * 1024.0;
    const KIB: f64 = 1024.0;
    let value = bytes as f64;
    if value >= MIB {
        format!("{:.1} MiB", value / MIB)
    } else if value >= KIB {
        format!("{:.1} KiB", value / KIB)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_notes_name_the_tool_release_size_and_time() {
        assert_eq!(
            downloading_message("golangci-lint", "2.6.1", "https://example.invalid/g.tar.gz"),
            "golangci-lint: downloading release 2.6.1 from https://example.invalid/g.tar.gz \
             into poly's tool cache (once per version)"
        );
        assert_eq!(
            installed_message(
                "golangci-lint",
                "2.6.1",
                Some(42 * 1024 * 1024),
                Duration::from_millis(12_340)
            ),
            "golangci-lint: installed release 2.6.1 (42.0 MiB) in 12.3s"
        );
        assert_eq!(
            installed_message("hadolint", "2.12.0", None, Duration::from_secs(1)),
            "hadolint: installed release 2.12.0 in 1.0s"
        );
    }

    #[test]
    fn unit_notes_say_what_is_spent_and_where() {
        assert_eq!(
            unit_started_message("golangci-lint", Items::Dirs, 12, "svc/api"),
            "golangci-lint: running on 12 directories in svc/api"
        );
        assert_eq!(
            unit_started_message("hadolint", Items::Files, 1, "."),
            "hadolint: running on 1 file in ."
        );
        assert_eq!(
            unit_started_message("helm", Items::Nothing, 0, "chart"),
            "helm: running in chart"
        );
        assert_eq!(
            unit_finished_message("golangci-lint", ".", Duration::from_millis(241_300)),
            "golangci-lint: finished . in 241.3s"
        );
    }

    #[test]
    fn sizes_render_in_binary_units() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(2048), "2.0 KiB");
        assert_eq!(human_size(3 * 1024 * 1024 + 512 * 1024), "3.5 MiB");
    }
}
