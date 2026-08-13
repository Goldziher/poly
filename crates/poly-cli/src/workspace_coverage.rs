//! Which languages `poly lint`'s **whole-project phase** lints.
//!
//! `poly lint` is two halves — a per-file tier and a whole-project phase
//! (`cargo clippy` and any configured whole-project analysis job) — and this
//! crate is the only layer that sees both. Without the mapping below, a run over
//! poly's own repository printed 229 lines of
//! `skipped …: no lint rules for Rust` and then, in the same output, `✓
//! cargo-clippy`. Rust *was* linted; the per-file tier simply had no rules for
//! it, which is a true statement about the tier and a false one about the run.
//!
//! The languages resolved here are handed to
//! [`poly_core::RunOptions::externally_linted_languages`], so the per-file tier
//! stops recording those files as skipped **at all** rather than hiding a skip
//! the JSON would still report. The linted count, the human note, the JSON
//! payload and `--deny-skips` therefore agree by construction.
//!
//! # How the mapping is derived
//!
//! It is keyed on the whole-project **hook id** — the same string the phase
//! prints in its `✓ / ×` list — so what the note claims and what the reader can
//! see named in the output are drawn from one identifier.
//!
//! - `cargo-clippy` is poly's own builtin (`[hooks.builtin.cargo] clippy`). It
//!   compiles the workspace and runs Rust lints over the source, so it is the
//!   entry poly can vouch for from first principles.
//! - The other three cargo builtins — `cargo-sort`, `cargo-machete`,
//!   `cargo-deny` — are deliberately **absent**. They check `Cargo.toml` sort
//!   order, unused dependency declarations and the license graph; none of them
//!   reads a line of `.rs`. Crediting them with Rust coverage would be the same
//!   false claim wearing a different hat, so a repo that runs `cargo deny` with
//!   `clippy = false` still gets the accurate `no lint rules for Rust`.
//! - The remaining entries name canonical whole-project linters for languages
//!   poly's per-file tier has no rules for — which is exactly where a false skip
//!   would otherwise survive. They match the id a repository gives its own
//!   inline `workspace = true` job.
//!
//! # Where this can be wrong
//!
//! An inline job's id is author-chosen, so matching it is a convention rather
//! than a guarantee, and it errs in two directions:
//!
//! - **Under-crediting** (common, benign): a job named `typecheck` or `lint-go`
//!   is not recognised, so its language keeps a skip it has arguably outgrown.
//!   The output is over-cautious but never false.
//! - **Over-crediting** (rare, harmful): a job named `golangci-lint` that in
//!   fact runs `golangci-lint fmt` would be credited with lint coverage it does
//!   not provide, and Go files would silently leave the skip set.
//!
//! Only ids that *name their tool* are matched, because the second failure is
//! the one this whole release exists to remove and the first merely reads as
//! caution. Matching on a job's command line instead would widen the recognised
//! set at the cost of crediting any `echo "run golangci-lint"`, which trades the
//! benign error for the harmful one.

use poly_config::PolyConfig;
use poly_core::Language;

/// The languages the whole-project phase will lint for `config`, for the caller
/// to declare to the per-file tier.
///
/// Resolves the phase's planned tool set without running it. A failure to
/// resolve it is *not* fatal here: the phase itself runs moments later and
/// reports the same failure with its own context, and answering "nothing" leaves
/// the per-file tier's skip accounting exactly as it was before this feature —
/// cautious, not wrong.
pub fn workspace_lint_languages(config: &PolyConfig) -> Vec<Language> {
    let tool_ids = match poly_workspace::planned_workspace_tool_ids(config) {
        Ok(ids) => ids,
        Err(error) => {
            tracing::debug!("could not resolve the whole-project tool set: {error:#}");
            return Vec::new();
        }
    };
    languages_for_tools(tool_ids.iter().map(String::as_str))
}

/// The deduplicated languages covered by a set of whole-project tool ids.
fn languages_for_tools<'a>(tool_ids: impl Iterator<Item = &'a str>) -> Vec<Language> {
    let mut languages: Vec<Language> = Vec::new();
    for language in tool_ids.flat_map(languages_linted_by) {
        if !languages.contains(&language) {
            languages.push(language);
        }
    }
    languages
}

/// The languages one whole-project tool lints, by hook id. Empty for an id poly
/// cannot vouch for — see the module docs for why silence is the safe answer.
fn languages_linted_by(tool_id: &str) -> Vec<Language> {
    match tool_id {
        // poly's own cargo builtin. `cargo-sort` / `cargo-machete` / `cargo-deny`
        // are absent on purpose: they read manifests, not source.
        "cargo-clippy" | "clippy" => vec![Language::Rust],
        "golangci-lint" | "go-vet" | "staticcheck" => vec![Language::Go],
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reported defect, at the mapping layer: the phase that runs clippy
    /// covers Rust, so the per-file tier must not call a `.rs` file unlinted.
    #[test]
    fn clippy_covers_rust() {
        assert_eq!(languages_for_tools(["cargo-clippy"].into_iter()), vec![Language::Rust]);
    }

    /// The sharpest edge of the mapping. These three run in the same phase and
    /// print beside clippy, but they inspect `Cargo.toml` and the dependency
    /// graph — never `.rs` source. A repo that runs them with `clippy = false`
    /// has genuinely not linted its Rust, and must still be told so.
    #[test]
    fn manifest_only_cargo_tools_do_not_cover_rust() {
        assert_eq!(
            languages_for_tools(["cargo-sort", "cargo-machete", "cargo-deny"].into_iter()),
            Vec::<Language>::new(),
            "a manifest checker is not a source linter"
        );
    }

    /// An id poly does not recognise contributes nothing: an over-cautious skip
    /// note is the benign failure, a false claim of coverage is not.
    #[test]
    fn an_unrecognised_tool_covers_nothing() {
        assert_eq!(
            languages_for_tools(["typecheck", "my-custom-job"].into_iter()),
            Vec::<Language>::new()
        );
    }

    /// Two tools naming the same language yield it once, so the declaration the
    /// runner scans stays as short as the language set.
    #[test]
    fn languages_are_deduplicated_across_tools() {
        assert_eq!(
            languages_for_tools(["golangci-lint", "go-vet", "cargo-clippy"].into_iter()),
            vec![Language::Go, Language::Rust]
        );
    }

    /// A phase with no tools at all — the `--no-workspace` and no-`[hooks]`
    /// cases — covers nothing, which is what keeps their skip accurate.
    #[test]
    fn no_tools_covers_nothing() {
        assert_eq!(languages_for_tools(std::iter::empty()), Vec::<Language>::new());
    }
}
