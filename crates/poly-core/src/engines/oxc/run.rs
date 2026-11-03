//! Single-file oxlint run on the calling thread.
//!
//! `LintService::run_source` drives its work through a `rayon::scope`, which makes oxc's own global
//! rayon pool a second pool outside poly's `-j` budget and 16 MiB stacks. This module reproduces what
//! `run_source` does for one in-memory file (oxc `crates/oxc_linter/src/service/runtime.rs`,
//! `Runtime::run_source`, `process_path_to_module`, `process_source`, `process_source_section`) using
//! only public oxc items, with no import resolution (`cross_module` is off in poly) and no external
//! linter (no JS plugins), so the output is the same as the service's.

use std::cell::RefCell;
use std::ffi::OsStr;
use std::path::Path;
use std::sync::Arc;

use oxc_allocator::Allocator;
use oxc_diagnostics::OxcDiagnostic;
use oxc_linter::loader::{JavaScriptSource, LINT_PARTIAL_LOADER_EXTENSIONS, PartialLoader};
use oxc_linter::{ConfigStore, ContextSubHost, ContextSubHostOptions, Linter, Message, ModuleRecord, PossibleFixes};
use oxc_parser::{ParseOptions, Parser, config::RuntimeParserConfig};
use oxc_semantic::SemanticBuilder;
use oxc_span::SourceType;

thread_local! {
    /// One arena per thread, reset before each file, so a run does not pay for a fresh allocation.
    static ALLOCATOR: RefCell<Allocator> = RefCell::new(Allocator::new());
}

/// A configured oxlint [`Linter`] that lints one in-memory file at a time on the calling thread.
pub(super) struct SourceLinter {
    linter: Linter,
    respect_eslint_disable_directives: bool,
}

impl SourceLinter {
    pub(super) fn new(options: oxc_linter::LintOptions, config: ConfigStore) -> Self {
        // `Linter::respect_eslint_disable_directives` is crate-private; the store answers the same question.
        let respect_eslint_disable_directives = config.respect_eslint_disable_directives();
        Self {
            linter: Linter::new(options, config, None),
            respect_eslint_disable_directives,
        }
    }

    /// Lint `source_text` as the file at `path`; the extension picks the source type (and the
    /// `.vue` / `.astro` / `.svelte` script extraction).
    pub(super) fn run(&self, path: &Path, source_text: &str) -> Vec<Message> {
        ALLOCATOR.with(|cell| {
            let mut allocator = cell.borrow_mut();
            allocator.reset();
            self.run_in(path, source_text, &allocator)
        })
    }

    fn run_in<'a>(&self, path: &Path, source_text: &str, allocator: &'a Allocator) -> Vec<Message> {
        let Some(ext) = path.extension().and_then(OsStr::to_str) else {
            return Vec::new();
        };
        let source_type = SourceType::from_path(path);
        if source_type
            .as_ref()
            .is_err_and(|_| !LINT_PARTIAL_LOADER_EXTENSIONS.contains(&ext))
        {
            return Vec::new();
        }
        let mut source_type = source_type.unwrap_or_default();
        // JS files are parsed with JSX on, to maximise the chance of parsing them.
        if source_type.is_javascript() {
            source_type = source_type.with_jsx(true);
        }
        let source_text: &'a str = allocator.alloc_str(source_text);

        let sections = PartialLoader::parse(ext, source_text)
            .unwrap_or_else(|| vec![JavaScriptSource::partial(source_text, source_type, 0)]);

        let mut messages = Vec::new();
        let mut hosts: Vec<ContextSubHost<'a>> = Vec::with_capacity(sections.len());
        for section in sections {
            match self.build_section(path, allocator, &section) {
                Ok(host) => hosts.push(host),
                Err(diagnostics) => {
                    messages.extend(
                        diagnostics
                            .into_iter()
                            .map(|diagnostic| Message::new(diagnostic, PossibleFixes::None)),
                    );
                }
            }
        }
        if hosts.is_empty() {
            return messages;
        }
        messages.extend(self.linter.run(path, hosts, allocator));
        messages
    }

    /// Parse one script section and build its semantic model; a parse or semantic error yields the
    /// diagnostics, with label offsets shifted back into the whole file.
    // `ContextSubHostOptions` is `#[non_exhaustive]`, so it can only be filled in field by field.
    #[allow(clippy::field_reassign_with_default)]
    fn build_section<'a>(
        &self,
        path: &Path,
        allocator: &'a Allocator,
        section: &JavaScriptSource<'a>,
    ) -> Result<ContextSubHost<'a>, Vec<OxcDiagnostic>> {
        let shift = |diagnostics: Vec<OxcDiagnostic>| -> Vec<OxcDiagnostic> {
            diagnostics
                .into_iter()
                .map(|mut diagnostic| {
                    for label in &mut diagnostic.labels {
                        label.set_span_offset(label.offset() + section.start);
                    }
                    diagnostic
                })
                .collect()
        };

        let ret = Parser::new(allocator, section.source_text, section.source_type)
            .with_options(ParseOptions {
                parse_regular_expression: true,
                allow_return_outside_function: true,
                ..ParseOptions::default()
            })
            .with_config(RuntimeParserConfig::new(false))
            .parse();
        if !ret.diagnostics.is_empty() {
            return Err(shift(if ret.is_flow_language {
                Vec::new()
            } else {
                ret.diagnostics.into()
            }));
        }

        let semantic_ret = SemanticBuilder::new_linter()
            .with_check_syntax_error(true)
            .build(allocator.alloc(ret.program));
        if !semantic_ret.diagnostics.is_empty() {
            return Err(shift(semantic_ret.diagnostics.into()));
        }
        let mut semantic = semantic_ret.semantic;
        semantic.set_irregular_whitespaces(ret.irregular_whitespaces);

        let module_record = Arc::new(ModuleRecord::new(path, &ret.module_record, &semantic));
        let mut options = ContextSubHostOptions::default();
        options.framework_options = section.framework_options;
        options.parser_tokens = ret.tokens.into_boxed_slice();
        options.respect_eslint_disable_directives = self.respect_eslint_disable_directives;
        Ok(ContextSubHost::new(semantic, module_record, section.start, options))
    }
}
