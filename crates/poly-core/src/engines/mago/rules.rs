//! Rule registry helpers for the mago backend.
//!
//! Provides category-name parsing and category-to-rule-code expansion used
//! when building the `only` allowlist for [`mago_linter::Linter::new`].
//!
//! # Category names accepted
//!
//! Matching is case-insensitive and accepts either kebab-case or snake_case:
//!
//! | Input | [`Category`] |
//! |-------|--------------|
//! | `clarity` | `Clarity` |
//! | `best-practices` / `best_practices` | `BestPractices` |
//! | `consistency` | `Consistency` |
//! | `deprecation` | `Deprecation` |
//! | `maintainability` | `Maintainability` |
//! | `redundancy` | `Redundancy` |
//! | `security` | `Security` |
//! | `safety` | `Safety` |
//! | `correctness` | `Correctness` |

use std::collections::HashSet;

use anyhow::Context as _;
use mago_linter::category::Category;
use mago_linter::integration::IntegrationSet;
use mago_linter::rule::AnyRule;
use mago_linter::settings::Settings;
use mago_php_version::PHPVersion;

use crate::config::EngineConfig;

/// Parse `php_version` from `cfg.options` (e.g. `"8.2"` → `PHPVersion::PHP82`),
/// shared by the lint and format passes so both honor the same target version.
///
/// # Errors
///
/// `Ok(None)` when the key is absent; an `Err` when it is present but malformed
/// (so a typo like `"8.x"` is reported rather than silently defaulted).
pub fn parse_php_version(cfg: &EngineConfig) -> anyhow::Result<Option<PHPVersion>> {
    let Some(value) = cfg.options.get("php_version") else {
        return Ok(None);
    };
    let text = value
        .as_str()
        .context("[*.php.mago] php_version must be a string, e.g. \"8.2\"")?;
    let mut parts = text.splitn(3, '.');
    let major: u32 = parts
        .next()
        .and_then(|p| p.parse().ok())
        .with_context(|| format!("invalid php_version {text:?}; expected MAJOR[.MINOR[.PATCH]]"))?;
    // Reject a non-numeric minor/patch (e.g. "8.x") rather than silently
    // defaulting it to 0 — a non-numeric component is a config error, not a 0.
    let mut component = |label: &str| -> anyhow::Result<u32> {
        match parts.next() {
            None | Some("") => Ok(0),
            Some(part) => part
                .parse()
                .with_context(|| format!("invalid php_version {label} in {text:?}; expected MAJOR[.MINOR[.PATCH]]")),
        }
    };
    let minor = component("minor")?;
    let patch = component("patch")?;
    Ok(Some(PHPVersion::new(major, minor, patch)))
}

// ── Category parsing ──────────────────────────────────────────────────────────

/// Parse a user-supplied category string to a mago [`Category`].
///
/// Accepts kebab-case (`best-practices`), snake_case (`best_practices`), and
/// any capitalisation.  Returns `None` for unrecognised inputs.
pub fn parse_category(s: &str) -> Option<Category> {
    match s.replace('-', "_").to_lowercase().as_str() {
        "clarity" => Some(Category::Clarity),
        "best_practices" => Some(Category::BestPractices),
        "consistency" => Some(Category::Consistency),
        "deprecation" => Some(Category::Deprecation),
        "maintainability" => Some(Category::Maintainability),
        "redundancy" => Some(Category::Redundancy),
        "security" => Some(Category::Security),
        "safety" => Some(Category::Safety),
        "correctness" => Some(Category::Correctness),
        _ => None,
    }
}

// ── Rule code enumeration ─────────────────────────────────────────────────────

/// Return the codes of all rules in `cat` that are present in the mago
/// registry, regardless of their default-enabled status or PHP version / integration
/// gates.
pub fn codes_for_category(cat: Category, php_version: PHPVersion, integrations: IntegrationSet) -> Vec<String> {
    all_rules_unconstrained(php_version, integrations)
        .filter_map(|rule| {
            if rule.meta().category == cat {
                Some(rule.code().to_owned())
            } else {
                None
            }
        })
        .collect()
}

/// Return the codes of all rules enabled by default for the given PHP version
/// and integrations (respects `Config::default_enabled` and `is_enabled_for`).
pub fn default_enabled_codes(php_version: PHPVersion, integrations: IntegrationSet) -> Vec<String> {
    let settings = make_settings(php_version, integrations);
    AnyRule::get_all_for(&settings, None, false)
        .into_iter()
        .map(|(rule, _)| rule.code().to_owned())
        .collect()
}

/// Return the codes of every rule in the mago registry (ignoring default
/// enabled/disabled and all version/integration requirements).
pub fn all_codes(php_version: PHPVersion, integrations: IntegrationSet) -> Vec<String> {
    all_rules_unconstrained(php_version, integrations)
        .map(|r| r.code().to_owned())
        .collect()
}

// ── Expand codes + categories ─────────────────────────────────────────────────

/// Expand a list of code-or-category-name strings to a de-duplicated list of
/// rule codes.
///
/// # Errors
///
/// Returns `anyhow::Error` when any item is neither a known rule code nor a
/// recognised category name, so typos are caught loudly at lint time.
pub fn expand_to_codes(
    items: &[String],
    php_version: PHPVersion,
    integrations: IntegrationSet,
) -> anyhow::Result<Vec<String>> {
    let all: HashSet<String> = all_codes(php_version, integrations).into_iter().collect();
    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for item in items {
        if let Some(cat) = parse_category(item) {
            let codes = codes_for_category(cat, php_version, integrations);
            for code in codes {
                if seen.insert(code.clone()) {
                    out.push(code);
                }
            }
        } else if all.contains(item) {
            if seen.insert(item.clone()) {
                out.push(item.clone());
            }
        } else {
            anyhow::bail!(
                "unknown rule code or category name in mago config: {:?}. \
                 Valid categories: clarity, best-practices, consistency, deprecation, \
                 maintainability, redundancy, security, safety, correctness",
                item
            );
        }
    }
    Ok(out)
}

// ── Private helpers ───────────────────────────────────────────────────────────

fn make_settings(php_version: PHPVersion, integrations: IntegrationSet) -> Settings {
    Settings {
        php_version,
        integrations,
        ..Settings::default()
    }
}

/// Iterate all rules, with `include_disabled = true` so version/integration
/// requirements and default-enabled flags are both bypassed.
fn all_rules_unconstrained(php_version: PHPVersion, integrations: IntegrationSet) -> impl Iterator<Item = AnyRule> {
    let settings = make_settings(php_version, integrations);
    AnyRule::get_all_for(&settings, None, true)
        .into_iter()
        .map(|(rule, _)| rule)
}
