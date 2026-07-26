use std::path::PathBuf;

use super::*;
use crate::config::GlobalDefaults;

fn make_src(path: &str, language: Language, content: &str) -> SourceFile {
    SourceFile {
        path: PathBuf::from(path),
        language,
        content: content.into(),
    }
}

/// Empty options → the tool's `default_on` policy decides (canonical tools
/// on, opt-in tools off). This is the out-of-the-box config.
fn default_cfg() -> EngineConfig {
    EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width: 4,
        options: toml::Table::new(),
    }
}

fn bool_cfg(enabled: bool) -> EngineConfig {
    let mut options = toml::Table::new();
    options.insert("enabled".to_string(), toml::Value::Boolean(enabled));
    EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width: 4,
        options,
    }
}

fn disabled_cfg() -> EngineConfig {
    bool_cfg(false)
}

fn enabled_cfg() -> EngineConfig {
    bool_cfg(true)
}

#[test]
fn engine_metadata_go() {
    let engine = NativeToolEngine::for_language(Language::Go);
    assert_eq!(engine.name(), "gofmt");
    assert_eq!(engine.languages(), &[Language::Go]);
    assert!(engine.capabilities().format);
    assert!(!engine.capabilities().lint, "gofmt is format-only; no lint rules");
    assert!(!engine.capabilities().fix);
}

#[test]
fn engine_metadata_rust() {
    let engine = NativeToolEngine::for_language(Language::Rust);
    assert_eq!(engine.name(), "rustfmt");
    assert_eq!(engine.languages(), &[Language::Rust]);
    assert!(engine.capabilities().format);
}

#[test]
fn engine_metadata_zig() {
    let engine = NativeToolEngine::for_language(Language::Zig);
    assert_eq!(engine.name(), "zigfmt");
    assert_eq!(engine.languages(), &[Language::Zig]);
    assert!(engine.capabilities().format);
}

#[test]
fn engine_metadata_shell_shfmt() {
    let engine = NativeToolEngine::shell_format();
    assert_eq!(engine.name(), "shfmt");
    assert_eq!(engine.languages(), &[Language::Shell]);
    assert!(engine.capabilities().format);
    assert!(!engine.capabilities().lint, "shfmt is format-only");
    assert!(!engine.capabilities().fix);
}

#[test]
fn engine_metadata_shell_shellcheck() {
    let engine = NativeToolEngine::shell_lint();
    assert_eq!(engine.name(), "shellcheck");
    assert_eq!(engine.languages(), &[Language::Shell]);
    assert!(engine.capabilities().lint);
    assert!(!engine.capabilities().format, "shellcheck is lint-only");
    assert!(!engine.capabilities().fix);
}

#[test]
fn default_policy_canonical_on_option_off() {
    assert!(
        NativeToolEngine::for_language(Language::Rust).is_enabled(&default_cfg()),
        "rustfmt must be default-on (canonical toolchain)"
    );
    assert!(
        NativeToolEngine::for_language(Language::Go).is_enabled(&default_cfg()),
        "gofmt must be default-on (canonical toolchain)"
    );
    assert!(
        !NativeToolEngine::for_language(Language::Zig).is_enabled(&default_cfg()),
        "zig fmt must stay opt-in"
    );
    assert!(
        !NativeToolEngine::shell_format().is_enabled(&default_cfg()),
        "shfmt must be opt-in (third-party tool)"
    );
    assert!(
        !NativeToolEngine::shell_lint().is_enabled(&default_cfg()),
        "shellcheck must be opt-in"
    );
    assert!(
        !NativeToolEngine::for_language(Language::Java).is_enabled(&default_cfg()),
        "google-java-format must be opt-in"
    );
    assert!(
        !NativeToolEngine::for_language(Language::Kotlin).is_enabled(&default_cfg()),
        "ktfmt must be opt-in"
    );
    assert!(
        !NativeToolEngine::for_language(Language::R).is_enabled(&default_cfg()),
        "styler must be opt-in"
    );
    assert!(
        !NativeToolEngine::for_language(Language::Swift).is_enabled(&default_cfg()),
        "swift-format must be opt-in"
    );
    assert!(
        !NativeToolEngine::for_language(Language::Dart).is_enabled(&default_cfg()),
        "dartfmt must be opt-in"
    );
    assert!(
        !NativeToolEngine::for_language(Language::Gleam).is_enabled(&default_cfg()),
        "gleamfmt must be opt-in"
    );
}

#[test]
fn explicit_config_overrides_default_policy() {
    assert!(
        !NativeToolEngine::for_language(Language::Rust).is_enabled(&disabled_cfg()),
        "explicit enabled=false must force rustfmt off"
    );
    assert!(
        !NativeToolEngine::for_language(Language::Go).is_enabled(&disabled_cfg()),
        "explicit enabled=false must force gofmt off"
    );
    assert!(
        NativeToolEngine::for_language(Language::Zig).is_enabled(&enabled_cfg()),
        "explicit enabled=true must opt zig fmt in"
    );
    assert!(
        NativeToolEngine::shell_format().is_enabled(&enabled_cfg()),
        "explicit enabled=true must opt shfmt in"
    );
    assert!(
        NativeToolEngine::shell_lint().is_enabled(&enabled_cfg()),
        "explicit enabled=true must opt shellcheck in"
    );
}

#[test]
fn fallback_notice_fires_only_when_wanted_and_absent() {
    assert!(should_notify_fallback(true, false));
    assert!(!should_notify_fallback(true, true));
    assert!(!should_notify_fallback(false, false));
    assert!(!should_notify_fallback(false, true));
}

#[test]
fn lint_clean_go_produces_no_diags() {
    let engine = NativeToolEngine::for_language(Language::Go);
    let src = make_src("main.go", Language::Go, "package main\n");
    let diags = engine.lint(&src, &disabled_cfg()).unwrap();
    assert!(
        diags.is_empty(),
        "clean Go source should produce no diagnostics via tree-sitter delegation"
    );
}

#[test]
fn lint_go_with_trailing_whitespace_not_flagged() {
    let engine = NativeToolEngine::for_language(Language::Go);
    let src = make_src("main.go", Language::Go, "package main   \nfunc main() {}\n");
    let diags = engine.lint(&src, &disabled_cfg()).unwrap();
    assert!(
        diags.is_empty(),
        "Go is format-only; lint must emit no diagnostics, got {diags:?}"
    );
}

#[test]
fn shellcheck_lint_disabled_emits_nothing() {
    let engine = NativeToolEngine::shell_lint();
    let src = make_src("script.sh", Language::Shell, "#!/bin/bash\necho hello   \n");
    let diags = engine.lint(&src, &disabled_cfg()).unwrap();
    assert!(
        diags.is_empty(),
        "disabled shellcheck must emit no diagnostics, got {diags:?}"
    );
}

#[test]
fn disabled_go_delegates_to_tier2() {
    const SRC: &str = "package main\nimport \"fmt\"\nfunc main() {\nfmt.Println(\"hi\")\n}\n";
    let engine = NativeToolEngine::for_language(Language::Go);
    let src = make_src("main.go", Language::Go, SRC);

    let native_result = engine.format(&src, &disabled_cfg()).unwrap();
    let ts_result = TreeSitterEngine.format(&src, &disabled_cfg()).unwrap();

    let native_out = match native_result {
        FormatOutput::Formatted(s) => s,
        FormatOutput::Unchanged => SRC.to_string(),
    };
    let ts_out = match ts_result {
        FormatOutput::Formatted(s) => s,
        FormatOutput::Unchanged => SRC.to_string(),
    };

    assert_eq!(
        native_out, ts_out,
        "disabled NativeToolEngine must produce byte-identical output to TreeSitterEngine"
    );
}

#[test]
fn disabled_rust_delegates_to_tier2() {
    const SRC: &str = "fn main(){let x=1+2;println!(\"{x}\");}\n";
    let engine = NativeToolEngine::for_language(Language::Rust);
    let src = make_src("main.rs", Language::Rust, SRC);

    let native_out = match engine.format(&src, &disabled_cfg()).unwrap() {
        FormatOutput::Formatted(s) => s,
        FormatOutput::Unchanged => SRC.to_string(),
    };
    let tier2_out = match TreeSitterEngine.format(&src, &disabled_cfg()).unwrap() {
        FormatOutput::Formatted(s) => s,
        FormatOutput::Unchanged => SRC.to_string(),
    };

    assert_eq!(native_out, tier2_out);
}

#[test]
fn disabled_shfmt_delegates_to_tier2() {
    const SRC: &str = "#!/bin/bash\nif [ \"$1\" = \"a\" ]; then\necho hello\nfi\n";
    let engine = NativeToolEngine::shell_format();
    let src = make_src("script.sh", Language::Shell, SRC);

    let native_out = match engine.format(&src, &disabled_cfg()).unwrap() {
        FormatOutput::Formatted(s) => s,
        FormatOutput::Unchanged => SRC.to_string(),
    };
    let tier2_out = match TreeSitterEngine.format(&src, &disabled_cfg()).unwrap() {
        FormatOutput::Formatted(s) => s,
        FormatOutput::Unchanged => SRC.to_string(),
    };

    assert_eq!(
        native_out, tier2_out,
        "disabled shfmt must produce byte-identical output to TreeSitterEngine"
    );
}

#[test]
fn default_rust_routes_by_rustfmt_presence() {
    const UNFORMATTED: &str = "fn main(){let x=1+2;println!(\"{x}\");}\n";
    let engine = NativeToolEngine::for_language(Language::Rust);
    let src = make_src("main.rs", Language::Rust, UNFORMATTED);

    let result = engine.format(&src, &default_cfg()).unwrap();
    let out = match result {
        FormatOutput::Formatted(s) => s,
        FormatOutput::Unchanged => UNFORMATTED.to_string(),
    };

    let tier2 = match TreeSitterEngine.format(&src, &default_cfg()).unwrap() {
        FormatOutput::Formatted(s) => s,
        FormatOutput::Unchanged => UNFORMATTED.to_string(),
    };

    if engine.probed_version().is_some() {
        assert!(
            out.contains("fn main() {"),
            "rustfmt should expand the signature; got: {out:?}"
        );
        assert!(
            !should_notify_fallback(engine.is_enabled(&default_cfg()), true),
            "no fallback notice when rustfmt is present"
        );
    } else {
        assert_eq!(
            out, tier2,
            "absent rustfmt must fall back to byte-identical tree-sitter output"
        );
        assert!(
            should_notify_fallback(engine.is_enabled(&default_cfg()), false),
            "absent default-on rustfmt must arm the tier-2 fallback notice"
        );
    }
}

#[test]
fn go_native_formats_unformatted_source() {
    let engine = NativeToolEngine::for_language(Language::Go);
    if engine.probed_version().is_none() {
        eprintln!("gofmt not found on PATH — skipping go_native_formats_unformatted_source");
        return;
    }

    const UNFORMATTED: &str = "package main\nimport \"fmt\"\nfunc main() {\nfmt.Println(\"hello\")\n}\n";
    let src = make_src("main.go", Language::Go, UNFORMATTED);
    let result = engine.format(&src, &enabled_cfg()).unwrap();

    let formatted = match result {
        FormatOutput::Formatted(s) => s,
        FormatOutput::Unchanged => panic!("expected gofmt to reformat the unformatted source"),
    };

    assert!(
        formatted.contains("\nfunc main()"),
        "gofmt output should contain a blank line before func"
    );
    assert!(
        formatted.contains("\tfmt.Println"),
        "gofmt output should use tab indentation"
    );

    insta::assert_snapshot!("go_native_known_unformatted", formatted);
}

#[test]
fn rust_native_formats_unformatted_source() {
    let engine = NativeToolEngine::for_language(Language::Rust);
    if engine.probed_version().is_none() {
        eprintln!("rustfmt not found on PATH — skipping rust_native_formats_unformatted_source");
        return;
    }

    const UNFORMATTED: &str = "fn main(){println!(\"hello\");let x=1+2;}\n";
    let src = make_src("main.rs", Language::Rust, UNFORMATTED);
    let result = engine.format(&src, &enabled_cfg()).unwrap();

    let formatted = match result {
        FormatOutput::Formatted(s) => s,
        FormatOutput::Unchanged => {
            panic!("expected rustfmt to reformat the unformatted source")
        }
    };

    assert!(
        formatted.contains("fn main() {"),
        "rustfmt output should expand the function signature"
    );

    insta::assert_snapshot!("rust_native_known_unformatted", formatted);
}

#[test]
fn zig_native_formats_unformatted_source() {
    let engine = NativeToolEngine::for_language(Language::Zig);
    if engine.probed_version().is_none() {
        eprintln!("zig not found on PATH — skipping zig_native_formats_unformatted_source");
        return;
    }

    const UNFORMATTED: &str = "const std = @import(\"std\");\npub fn main() void {\n_ = std;\n}\n";
    let src = make_src("main.zig", Language::Zig, UNFORMATTED);
    let result = engine.format(&src, &enabled_cfg()).unwrap();

    let formatted = match result {
        FormatOutput::Formatted(s) => s,
        FormatOutput::Unchanged => UNFORMATTED.to_string(),
    };

    insta::assert_snapshot!("zig_native_known_unformatted", formatted);
}

/// Known-unformatted shell: shfmt should add consistent indentation.
/// Skipped when `shfmt` is not on PATH.
#[test]
fn shfmt_formats_unformatted_shell() {
    let engine = NativeToolEngine::shell_format();
    if engine.probed_version().is_none() {
        eprintln!("shfmt not found on PATH — skipping shfmt_formats_unformatted_shell");
        return;
    }

    const UNFORMATTED: &str = "#!/bin/bash\nif [ \"$1\" = \"hello\" ]; then\necho \"world\"\nfi\n";
    let src = make_src("script.sh", Language::Shell, UNFORMATTED);
    let result = engine.format(&src, &enabled_cfg()).unwrap();

    let formatted = match result {
        FormatOutput::Formatted(s) => s,
        FormatOutput::Unchanged => {
            panic!("expected shfmt to reformat the unformatted source")
        }
    };

    assert!(
        formatted.contains("    echo"),
        "shfmt output should use 4-space indentation; got:\n{formatted}"
    );

    insta::assert_snapshot!("shell_shfmt_known_unformatted", formatted);
}

/// shellcheck on a known-bad script produces SC-coded diagnostics.
/// Skipped when `shellcheck` is not on PATH.
#[test]
fn shellcheck_lint_produces_sc_diagnostics() {
    let engine = NativeToolEngine::shell_lint();
    if engine.probed_version().is_none() {
        eprintln!("shellcheck not found on PATH — skipping shellcheck_lint_produces_sc_diagnostics");
        return;
    }

    const BAD: &str = "#!/bin/bash\nx=\"hello world\"\necho $x\n";
    let src = make_src("bad.sh", Language::Shell, BAD);
    let diags = engine.lint(&src, &enabled_cfg()).unwrap();

    let sc_diags: Vec<_> = diags
        .iter()
        .filter(|d| d.code.as_deref().unwrap_or("").starts_with("SC"))
        .collect();
    assert!(
        !sc_diags.is_empty(),
        "shellcheck should flag SC2086 (unquoted variable) in the known-bad script"
    );
    assert!(
        sc_diags.iter().any(|d| d.code.as_deref() == Some("SC2086")),
        "expected SC2086 in diagnostics; got: {sc_diags:?}"
    );
}

#[test]
fn go_native_unchanged_on_already_formatted() {
    let engine = NativeToolEngine::for_language(Language::Go);
    if engine.probed_version().is_none() {
        eprintln!("gofmt not found on PATH — skipping go_native_unchanged_on_already_formatted");
        return;
    }

    const FORMATTED: &str = "package main\n\nimport \"fmt\"\n\nfunc main() {\n\tfmt.Println(\"hello\")\n}\n";
    let src = make_src("main.go", Language::Go, FORMATTED);
    let result = engine.format(&src, &enabled_cfg()).unwrap();
    assert!(
        matches!(result, FormatOutput::Unchanged),
        "gofmt must return Unchanged for already-formatted source"
    );
}
