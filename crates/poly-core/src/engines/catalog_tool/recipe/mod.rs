//! Recipe tools: catalog tools that run as more than one spawn per file with an
//! exit-code verdict (ADR 0035).
//!
//! A [`RecipeTool`] is built for one role (lint or format) from a catalog
//! [`Tool`], its [`Recipe`] and the user's `[tools.<name>]` config. It owns the
//! three things that make a heavyweight external tool usable in a monorepo:
//!
//! - **Batching.** The run's files go to the tool as one argv (sharded to the
//!   argument budget), with failure isolation by halving: a tool that rejects
//!   one file does not send the other thousand back to a spawn each.
//! - **Units.** A tool that works on a chart, a Go module or a buf module is
//!   given the project root found from the files' markers, once per root, with
//!   independent roots running in parallel (or one at a time for a tool that
//!   holds a global lock).
//! - **Reports.** What the tool prints is parsed into file/line/column/rule
//!   diagnostics instead of one opaque message per file.
//!
//! The submodules split along those lines: [`argv`] (placeholders and sharding),
//! [`lint`] (grouping, running, attributing), [`format`] (scratch copies and
//! in-place passes), [`output`] (report parsers), [`launch`] and [`install`]
//! (finding or downloading the program), [`salt`] (cache inputs), [`mirror`],
//! [`paths`] and [`exec`].

mod argv;
mod exec;
mod format;
mod install;
mod launch;
mod lint;
mod mirror;
mod output;
mod paths;
mod salt;

use std::collections::BTreeMap;
use std::path::PathBuf;

use globset::{Glob, GlobSet, GlobSetBuilder};
use poly_catalog::{Command, Group, Recipe, Tool};
use poly_config::ToolConfig;

use self::argv::Template;
use self::launch::{Launcher, Resolved};
use crate::engine::SourceFile;

/// Which role a tool is built for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Role {
    Lint,
    Format,
}

/// A catalog tool prepared to run in one role.
pub(super) struct RecipeTool {
    tool: &'static Tool,
    recipe: &'static Recipe,
    launcher: Launcher,
    env: BTreeMap<String, String>,
    /// The directory file paths are relative to and tools run from by default.
    cwd: PathBuf,
    /// The role's passes, in order. Lint has one; format has the command and its
    /// `then` chain. Every pass takes the run's files.
    passes: Vec<Template>,
    /// The single-file stdin form of a formatter whose batch form is in-place.
    stdin: Option<Vec<String>>,
    /// The passes that apply the tool's own fixes (lint role).
    fix_passes: Vec<Template>,
    /// Include/exclude filter from `path_globs` and the user's `files`/`exclude`.
    filter: Option<Filter>,
    /// Context files copied beside a scratch copy: markers and config names.
    context_names: Vec<String>,
    /// The part of the cache key that does not vary per file.
    version: String,
}

/// File selection for one tool.
struct Filter {
    include: Option<GlobSet>,
    exclude: Option<GlobSet>,
}

impl Filter {
    fn matches(&self, path: &std::path::Path) -> bool {
        self.include.as_ref().is_none_or(|set| set.is_match(path))
            && !self.exclude.as_ref().is_some_and(|set| set.is_match(path))
    }
}

fn glob_set(patterns: &[String]) -> Option<GlobSet> {
    if patterns.is_empty() {
        return None;
    }
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        if let Ok(glob) = Glob::new(pattern) {
            builder.add(glob);
        }
    }
    builder.build().ok()
}

impl RecipeTool {
    /// Prepare `tool` for `role`.
    ///
    /// `Err` carries the message to show when the tool cannot be run (not
    /// installed, a pin poly cannot satisfy). The engine stays registered but
    /// inert, and says so once.
    pub(super) fn build(
        tool: &'static Tool,
        recipe: &'static Recipe,
        config: &ToolConfig,
        role: Role,
        command_name: Option<&str>,
    ) -> Result<RecipeTool, String> {
        let launcher = match launch::resolve(tool, recipe, config) {
            Resolved::Ready(launcher) => launcher,
            Resolved::Missing(message) => return Err(message),
        };
        let command = match (role, command_name) {
            (_, Some(name)) => tool.command(name),
            (Role::Format, None) => tool.format_command().map(|(_, command)| command),
            (Role::Lint, None) => tool.lint_command().map(|(_, command)| command),
        }
        .ok_or_else(|| format!("tool `{}` has no command for this role", tool.name))?;

        let cwd = std::env::current_dir().map_err(|error| format!("reading the working directory: {error}"))?;
        let cwd = match &config.root {
            Some(root) => cwd.join(root),
            None => cwd,
        };
        let extra = extra_args(config, role);
        let flags = &recipe.config_flags;
        let main = config.args.as_deref();

        let (mut passes, stdin) = passes_for(command, main, &extra, role);
        for pass in &command.then {
            let tokens = pass.batch_arguments().map(<[String]>::to_vec).unwrap_or_default();
            passes.push(Template::new(&salt::absolutize_config_args(&tokens, flags, &cwd), &[]));
        }
        let fix_passes = recipe
            .fix
            .as_deref()
            .and_then(|name| tool.command(name))
            .filter(|_| role == Role::Lint)
            .map(|fix| fix_templates(fix, &extra, flags, &cwd))
            .unwrap_or_default();

        let flagged: Vec<PathBuf> = passes
            .iter()
            .flat_map(|pass| salt::config_flag_files(pass.tokens(), flags))
            .map(|path| if path.is_absolute() { path } else { cwd.join(path) })
            .collect();
        let fingerprint = salt::config_fingerprint(&cwd, &flagged, &recipe.config_files);

        let mut context_names = recipe.config_files.clone();
        if let Group::Marker(markers) = &recipe.group {
            context_names.extend(markers.iter().cloned());
        }
        let version = format!(
            "{}:{}:role={role:?}:passes={:?}:fix={:?}:stdin={stdin:?}:env={:?}:cfg={fingerprint}",
            tool.name,
            launcher.identity,
            passes.iter().map(Template::fingerprint).collect::<Vec<_>>(),
            fix_passes.iter().map(Template::fingerprint).collect::<Vec<_>>(),
            config.env,
        );

        Ok(RecipeTool {
            tool,
            recipe,
            launcher,
            env: config.env.clone(),
            cwd,
            passes,
            stdin,
            fix_passes,
            filter: filter_for(tool, config),
            context_names,
            version,
        })
    }

    pub(super) fn version(&self) -> &str {
        &self.version
    }

    /// Whether the tool applies to `path` (its `path_globs` and the user's
    /// `files`/`exclude`).
    pub(super) fn applies_to(&self, path: &std::path::Path) -> bool {
        self.filter.as_ref().is_none_or(|filter| {
            filter.matches(&paths::relative(&self.cwd, path).unwrap_or_else(|| path.to_path_buf()))
        })
    }

    /// Fewest files worth a separate invocation (see `Recipe::shard_floor`).
    pub(super) fn shard_floor(&self) -> usize {
        if self.recipe.is_grouped() {
            usize::MAX / 2
        } else {
            self.recipe.shard_floor.unwrap_or(2)
        }
    }

    pub(super) fn can_fix(&self) -> bool {
        !self.fix_passes.is_empty()
    }

    pub(super) fn can_batch(&self) -> bool {
        !self.passes.is_empty()
    }

    /// The part of a file's lint cache key that its own bytes do not cover: the
    /// digest of the files its project root's verdict depends on.
    pub(super) fn cache_salt(&self, src: &SourceFile) -> Option<String> {
        if self.recipe.dependencies.is_empty() {
            return None;
        }
        let abs = paths::absolute(&self.cwd, &src.path);
        let root = match &self.recipe.group {
            Group::None => self.cwd.clone(),
            Group::Dir => abs.parent()?.to_path_buf(),
            Group::Marker(markers) => paths::project_root(&abs, markers)?,
        };
        Some(salt::dependency_digest(&root, &self.recipe.dependencies))
    }
}

fn extra_args(config: &ToolConfig, role: Role) -> Vec<String> {
    let specific = match role {
        Role::Lint => &config.lint_extra_args,
        Role::Format => &config.format_extra_args,
    };
    config.extra_args.iter().chain(specific).cloned().collect()
}

/// The role's templates and, for a formatter with a distinct stdin form, that
/// form's argv.
fn passes_for(
    command: &Command,
    main: Option<&[String]>,
    extra: &[String],
    role: Role,
) -> (Vec<Template>, Option<Vec<String>>) {
    let arguments = main
        .map(<[String]>::to_vec)
        .unwrap_or_else(|| command.arguments.clone());
    if command.stdin && role == Role::Format {
        let batch = command.batch.clone().or_else(|| main.map(<[String]>::to_vec));
        let stdin = splice_before_stdin_dash(&arguments, extra);
        let passes = batch
            .map(|tokens| vec![Template::new(&tokens, extra)])
            .unwrap_or_default();
        return (passes, Some(stdin));
    }
    let tokens = command.batch.clone().filter(|_| main.is_none()).unwrap_or(arguments);
    (vec![Template::new(&tokens, extra)], None)
}

fn fix_templates(fix: &Command, extra: &[String], flags: &[String], cwd: &std::path::Path) -> Vec<Template> {
    let mut templates = vec![Template::new(
        &salt::absolutize_config_args(&fix.arguments, flags, cwd),
        extra,
    )];
    for pass in &fix.then {
        templates.push(Template::new(
            &salt::absolutize_config_args(&pass.arguments, flags, cwd),
            &[],
        ));
    }
    templates
}

/// `["fmt", "-"]` plus extras -> `["fmt", <extras>, "-"]`: flags precede the
/// stdin marker.
fn splice_before_stdin_dash(arguments: &[String], extra: &[String]) -> Vec<String> {
    let at = arguments.iter().rposition(|a| a == "-").unwrap_or(arguments.len());
    let mut out = arguments[..at].to_vec();
    out.extend_from_slice(extra);
    out.extend_from_slice(&arguments[at..]);
    out
}

fn filter_for(tool: &Tool, config: &ToolConfig) -> Option<Filter> {
    let mut include: Vec<String> = tool.path_globs.clone();
    if let Some(files) = &config.files {
        include = files.as_slice().to_vec();
    }
    let exclude: Vec<String> = config
        .exclude
        .as_ref()
        .map(|p| p.as_slice().to_vec())
        .unwrap_or_default();
    let filter = Filter {
        include: glob_set(&include),
        exclude: glob_set(&exclude),
    };
    (filter.include.is_some() || filter.exclude.is_some()).then_some(filter)
}

#[cfg(test)]
mod tests;
