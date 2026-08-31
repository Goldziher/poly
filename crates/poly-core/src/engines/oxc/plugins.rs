//! The oxlint plugin set `[lint.<lang>.oxc] plugins` asks for.
//!
//! Its own module rather than part of `config`: `config` holds the *formatter*
//! option builders, which the markup_fmt backend shares for Astro `<script>`
//! blocks, and a lint-only key read out of a file that backend also owns would
//! read as a key markup_fmt accepts.

use oxc_linter::LintPlugins;

use crate::config::EngineConfig;
use crate::engines::rule_config::string_list;

/// The plugin names `plugins` accepts, for the error message an unknown name
/// produces. oxlint's `LintPlugins::try_from` additionally accepts spelling
/// variants (`react-hooks`, `@typescript-eslint`, `import-x`, `deepscan`, an
/// `eslint-plugin-`/`oxlint-plugin-` prefix), so this is the canonical name of
/// each plugin rather than the exhaustive set of strings that parse.
const KNOWN_PLUGIN_NAMES: &str = "eslint, react, unicorn, typescript, oxc, import, jsdoc, jest, vitest, \
                                  jsx-a11y, nextjs, react-perf, promise, node, vue";

/// The oxlint plugins `[lint.<lang>.oxc] plugins` asks for, on top of oxlint's
/// own default set (`unicorn | typescript | oxc`).
///
/// A plugin is not reachable through rule selection: `ConfigStoreBuilder`
/// resolves a filter against the rules of *enabled* plugins only, and filters
/// the built rule list by them a second time, so `extend_select =
/// ["vitest/expect-expect"]` on its own is a no-op. This key is what turns the
/// plugin on; the rule then behaves like any other.
///
/// # Errors
/// An unrecognised plugin name is an error rather than a skip: silently
/// dropping it would leave the user believing a rule set is live when the run
/// checks nothing of the kind.
pub(super) fn extra_lint_plugins(cfg: &EngineConfig) -> anyhow::Result<LintPlugins> {
    let mut plugins = LintPlugins::empty();
    for name in string_list(cfg, "plugins") {
        plugins |= parse_plugin_name(&name)?;
    }
    Ok(plugins)
}

/// Resolve one plugin name, naming the offending value when it does not parse.
fn parse_plugin_name(name: &str) -> anyhow::Result<LintPlugins> {
    LintPlugins::try_from(name)
        .map_err(|()| anyhow::anyhow!("unknown oxlint plugin {name:?}; known plugins: {KNOWN_PLUGIN_NAMES}"))
}
