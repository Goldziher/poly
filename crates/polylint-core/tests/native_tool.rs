//! Integration fixtures for the tier-3 native tool backends.
//!
//! Each fixture is **gated on the tool being present** at runtime: when
//! `gofmt` / `rustfmt` / `zig` is not on `PATH`, the test prints a skip
//! notice and returns early so CI without the respective toolchain still
//! passes.
//!
//! Fixtures:
//! - `go_known_unformatted` — a Go source file that gofmt reformats: asserts
//!   the exact formatted output.
//! - `rust_known_unformatted` — a Rust source file that rustfmt reformats.
//! - `zig_known_unformatted` — a Zig source file that zig fmt reformats.
//! - `disabled_is_tier2` — with an explicit `enabled = false`, `NativeToolEngine`
//!   must produce byte-identical output to `TreeSitterEngine` for Go — proving
//!   the config-disable fallback. (The canonical tools rustfmt/gofmt are now
//!   default-ON when present, so an explicit override is required to disable.)

use polylint_core::{
    Language,
    config::{EngineConfig, GlobalDefaults},
    engine::{Engine, FormatOutput, SourceFile},
    engines::native_tool::NativeToolEngine,
    engines::treesitter::TreeSitterEngine,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn make_src(path: &str, language: Language, content: &str) -> SourceFile {
    SourceFile {
        path: path.into(),
        language,
        content: content.into(),
    }
}

fn disabled_cfg() -> EngineConfig {
    // Explicit enabled=false: required now that the canonical tools (rustfmt,
    // gofmt) are default-on when present.
    let mut options = toml::Table::new();
    options.insert("enabled".to_string(), toml::Value::Boolean(false));
    EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width: 4,
        options,
    }
}

fn enabled_cfg() -> EngineConfig {
    let mut options = toml::Table::new();
    options.insert("enabled".to_string(), toml::Value::Boolean(true));
    EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width: 4,
        options,
    }
}

// ---------------------------------------------------------------------------
// Wave-2 metadata (no tool presence required)
// ---------------------------------------------------------------------------

#[test]
fn engine_metadata_java() {
    let engine = NativeToolEngine::for_language(Language::Java);
    assert_eq!(engine.name(), "google-java-format");
    assert_eq!(engine.languages(), &[Language::Java]);
    assert!(engine.capabilities().format);
    assert!(engine.capabilities().lint);
    assert!(!engine.capabilities().fix);
}

#[test]
fn engine_metadata_kotlin() {
    let engine = NativeToolEngine::for_language(Language::Kotlin);
    assert_eq!(engine.name(), "ktfmt");
    assert_eq!(engine.languages(), &[Language::Kotlin]);
    assert!(engine.capabilities().format);
    assert!(engine.capabilities().lint);
    assert!(!engine.capabilities().fix);
}

#[test]
fn engine_metadata_r() {
    let engine = NativeToolEngine::for_language(Language::R);
    assert_eq!(engine.name(), "styler");
    assert_eq!(engine.languages(), &[Language::R]);
    assert!(engine.capabilities().format);
    assert!(engine.capabilities().lint);
    assert!(!engine.capabilities().fix);
}

#[test]
fn engine_metadata_swift() {
    let engine = NativeToolEngine::for_language(Language::Swift);
    assert_eq!(engine.name(), "swift-format");
    assert_eq!(engine.languages(), &[Language::Swift]);
    assert!(engine.capabilities().format);
    assert!(engine.capabilities().lint);
    assert!(!engine.capabilities().fix);
}

#[test]
fn engine_metadata_dart() {
    let engine = NativeToolEngine::for_language(Language::Dart);
    assert_eq!(engine.name(), "dartfmt");
    assert_eq!(engine.languages(), &[Language::Dart]);
    assert!(engine.capabilities().format);
    assert!(engine.capabilities().lint);
    assert!(!engine.capabilities().fix);
}

#[test]
fn engine_metadata_gleam() {
    let engine = NativeToolEngine::for_language(Language::Gleam);
    assert_eq!(engine.name(), "gleamfmt");
    assert_eq!(engine.languages(), &[Language::Gleam]);
    assert!(engine.capabilities().format);
    assert!(engine.capabilities().lint);
    assert!(!engine.capabilities().fix);
}

// ---------------------------------------------------------------------------
// Default-off invariant
// ---------------------------------------------------------------------------

/// With an explicit `enabled = false`, `NativeToolEngine` for Go must produce
/// byte-identical output to a direct `TreeSitterEngine` call.
///
/// This test does NOT require `gofmt` to be installed and must always pass.
#[test]
fn disabled_is_byte_identical_to_tier2() {
    const SRC: &str = concat!(
        "package main\n",
        "import \"fmt\"\n",
        "func main() {\n",
        "fmt.Println(\"hi\")\n",
        "}\n",
    );

    let native_engine = NativeToolEngine::for_language(Language::Go);
    let src = make_src("main.go", Language::Go, SRC);

    let native_out = match native_engine.format(&src, &disabled_cfg()).unwrap() {
        FormatOutput::Formatted(s) => s,
        FormatOutput::Unchanged => SRC.to_string(),
    };

    let ts_out = match TreeSitterEngine.format(&src, &disabled_cfg()).unwrap() {
        FormatOutput::Formatted(s) => s,
        FormatOutput::Unchanged => SRC.to_string(),
    };

    assert_eq!(
        native_out, ts_out,
        "disabled NativeToolEngine(Go) must be byte-identical to TreeSitterEngine"
    );

    // Snapshot the tier-2 output for regression detection.
    insta::assert_snapshot!("native_tool_disabled_go_tier2_output", ts_out);
}

// ---------------------------------------------------------------------------
// Known-unformatted fixtures (tool-gated)
// ---------------------------------------------------------------------------

/// Known-unformatted Go: missing blank lines and unindented body.
const GO_UNFORMATTED: &str = concat!(
    "package main\n",
    "import \"fmt\"\n",
    "func main() {\n",
    "fmt.Println(\"hello\")\n",
    "}\n",
);

#[test]
fn go_known_unformatted_snapshot() {
    let engine = NativeToolEngine::for_language(Language::Go);
    if !engine.is_available() {
        eprintln!("gofmt not found on PATH — skipping go_known_unformatted_snapshot");
        return;
    }

    let src = make_src("main.go", Language::Go, GO_UNFORMATTED);
    let result = engine.format(&src, &enabled_cfg()).unwrap();

    let formatted = match result {
        FormatOutput::Formatted(s) => s,
        FormatOutput::Unchanged => panic!("expected gofmt to reformat the unformatted source"),
    };

    insta::assert_snapshot!("go_native_known_unformatted", formatted);
}

/// Known-unformatted Rust: cramped function body.
const RUST_UNFORMATTED: &str = "fn main(){println!(\"hello\");let x=1+2;}\n";

#[test]
fn rust_known_unformatted_snapshot() {
    let engine = NativeToolEngine::for_language(Language::Rust);
    if !engine.is_available() {
        eprintln!("rustfmt not found on PATH — skipping rust_known_unformatted_snapshot");
        return;
    }

    let src = make_src("main.rs", Language::Rust, RUST_UNFORMATTED);
    let result = engine.format(&src, &enabled_cfg()).unwrap();

    let formatted = match result {
        FormatOutput::Formatted(s) => s,
        FormatOutput::Unchanged => panic!("expected rustfmt to reformat the unformatted source"),
    };

    insta::assert_snapshot!("rust_native_known_unformatted", formatted);
}

/// Edition-awareness: rustfmt with `--edition` resolved from the workspace
/// `Cargo.toml` (2024) must leave an already-`cargo fmt`-clean source file
/// `Unchanged`. Without the edition flag rustfmt assumes edition 2015 and can
/// reformat clean edition-2024 source — a false positive on every `.rs` file.
///
/// Uses this crate's own `src/lib.rs` (kept `cargo fmt`-clean by the prek
/// hooks). Skipped when `rustfmt` is not on PATH.
#[test]
fn rustfmt_leaves_clean_2024_source_unchanged() {
    let engine = NativeToolEngine::for_language(Language::Rust);
    if !engine.is_available() {
        eprintln!("rustfmt not found on PATH — skipping rustfmt_leaves_clean_2024_source_unchanged");
        return;
    }

    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs");
    let content = std::fs::read_to_string(&path).expect("read crate src/lib.rs");
    let src = SourceFile {
        path,
        language: Language::Rust,
        content: content.into(),
    };

    let result = engine.format(&src, &enabled_cfg()).unwrap();
    assert!(
        matches!(result, FormatOutput::Unchanged),
        "rustfmt with --edition 2024 must leave a cargo-fmt-clean file unchanged, got: {result:?}"
    );
}

/// Known-unformatted Zig: missing indentation.
const ZIG_UNFORMATTED: &str = concat!(
    "const std = @import(\"std\");\n",
    "pub fn main() void {\n",
    "_ = std;\n",
    "}\n",
);

#[test]
fn zig_known_unformatted_snapshot() {
    let engine = NativeToolEngine::for_language(Language::Zig);
    if !engine.is_available() {
        eprintln!("zig not found on PATH — skipping zig_known_unformatted_snapshot");
        return;
    }

    let src = make_src("main.zig", Language::Zig, ZIG_UNFORMATTED);
    let result = engine.format(&src, &enabled_cfg()).unwrap();

    let formatted = match result {
        FormatOutput::Formatted(s) => s,
        FormatOutput::Unchanged => panic!("expected zig fmt to reformat the unformatted source"),
    };

    insta::assert_snapshot!("zig_native_known_unformatted", formatted);
}

// ---------------------------------------------------------------------------
// rustfmt.toml conformance tests
// ---------------------------------------------------------------------------

/// poly honours a project-level `rustfmt.toml`: a 95-char function signature
/// (clean at the 120-column poly default) is reformatted when the toml sets
/// `max_width = 60` — proving poly did NOT override it with `max_width = 120`.
///
/// Gated on `rustfmt` presence.
#[test]
fn rustfmt_honors_rustfmt_toml_max_width() {
    let engine = NativeToolEngine::for_language(Language::Rust);
    if !engine.is_available() {
        eprintln!("rustfmt not found on PATH — skipping rustfmt_honors_rustfmt_toml_max_width");
        return;
    }

    // Temp dir acts as the project root with rustfmt.toml max_width = 60.
    let tmp = tempfile::tempdir().expect("create temp dir for rustfmt.toml test");
    std::fs::write(tmp.path().join("rustfmt.toml"), "max_width = 60\n").expect("write rustfmt.toml");

    // 95-char signature (clean at max_width 120, too wide at max_width 60).
    // rustfmt at max_width 60 wraps the parameters → Formatted.
    // If poly ignores the toml and injects max_width 120, rustfmt leaves it
    // intact → Unchanged.  The assert proves poly honoured the toml.
    const SRC: &str = concat!(
        "fn function_with_long_params(",
        "param_one: String, param_two: String, param_three: u32) -> bool {\n",
        "    true\n",
        "}\n",
    );

    let src = SourceFile {
        path: tmp.path().join("lib.rs"),
        language: Language::Rust,
        content: SRC.into(),
    };

    let result = engine.format(&src, &enabled_cfg()).unwrap();
    assert!(
        matches!(result, FormatOutput::Formatted(_)),
        "rustfmt must reformat the 95-char signature when rustfmt.toml sets \
         max_width = 60; got Unchanged — poly likely forced max_width = 120 \
         and ignored the project toml"
    );
}

// ---------------------------------------------------------------------------
// Wave-2 opt-in backends (gated on tool presence)
// ---------------------------------------------------------------------------

/// Known-unformatted Java: missing blank lines between class members.
/// Skipped when `google-java-format` is not on PATH.
const JAVA_UNFORMATTED: &str = concat!(
    "public class Hello {\n",
    "public static void main(String[] args) {\n",
    "System.out.println(\"hello\");\n",
    "}\n",
    "}\n",
);

#[test]
fn java_native_known_unformatted_snapshot() {
    let engine = NativeToolEngine::for_language(Language::Java);
    if !engine.is_available() {
        eprintln!("google-java-format not found on PATH — skipping java_native_known_unformatted_snapshot");
        return;
    }
    let src = make_src("Hello.java", Language::Java, JAVA_UNFORMATTED);
    let result = engine.format(&src, &enabled_cfg()).unwrap();
    let formatted = match result {
        FormatOutput::Formatted(s) => s,
        FormatOutput::Unchanged => panic!("expected google-java-format to reformat the source"),
    };
    insta::assert_snapshot!("java_native_known_unformatted", formatted);
}

/// Known-unformatted Kotlin: missing blank lines and inconsistent indentation.
/// Skipped when `ktfmt` is not on PATH.
const KOTLIN_UNFORMATTED: &str = concat!("fun main() {\n", "println(\"hello\")\n", "val x=1+2\n", "}\n",);

#[test]
fn kotlin_native_known_unformatted_snapshot() {
    let engine = NativeToolEngine::for_language(Language::Kotlin);
    if !engine.is_available() {
        eprintln!("ktfmt not found on PATH — skipping kotlin_native_known_unformatted_snapshot");
        return;
    }
    let src = make_src("main.kt", Language::Kotlin, KOTLIN_UNFORMATTED);
    let result = engine.format(&src, &enabled_cfg()).unwrap();
    let formatted = match result {
        FormatOutput::Formatted(s) => s,
        FormatOutput::Unchanged => panic!("expected ktfmt to reformat the source"),
    };
    insta::assert_snapshot!("kotlin_native_known_unformatted", formatted);
}

/// Known-unformatted R: inconsistent spacing around operators.
/// Skipped when `Rscript` is not on PATH.
const R_UNFORMATTED: &str = concat!("x<-1+2\n", "y<-x*3\n", "print(y)\n",);

#[test]
fn r_native_known_unformatted_snapshot() {
    let engine = NativeToolEngine::for_language(Language::R);
    if !engine.is_available() {
        eprintln!("Rscript not found on PATH — skipping r_native_known_unformatted_snapshot");
        return;
    }
    let src = make_src("script.R", Language::R, R_UNFORMATTED);
    let result = engine.format(&src, &enabled_cfg()).unwrap();
    // `is_available()` only probes `Rscript`; the actual reformat needs the
    // `styler` package, which may be absent even when Rscript is present (e.g.
    // the Windows CI runner). A no-op then means "formatter effectively
    // unavailable" — skip, rather than assert the unformatted input against the
    // formatted snapshot.
    let FormatOutput::Formatted(formatted) = result else {
        eprintln!("R styler did not reformat (package likely absent) — skipping r_native_known_unformatted_snapshot");
        return;
    };
    insta::assert_snapshot!("r_native_known_unformatted", formatted);
}

/// Known-unformatted Swift: missing blank lines and inconsistent indentation.
/// Skipped when `swift-format` is not on PATH.
const SWIFT_UNFORMATTED: &str = concat!("func greet(name:String)->String{\n", "return \"Hello, \"+name\n", "}\n",);

#[test]
fn swift_native_known_unformatted_snapshot() {
    let engine = NativeToolEngine::for_language(Language::Swift);
    if !engine.is_available() {
        eprintln!("swift-format not found on PATH — skipping swift_native_known_unformatted_snapshot");
        return;
    }
    let src = make_src("hello.swift", Language::Swift, SWIFT_UNFORMATTED);
    let result = engine.format(&src, &enabled_cfg()).unwrap();
    let formatted = match result {
        FormatOutput::Formatted(s) => s,
        FormatOutput::Unchanged => SWIFT_UNFORMATTED.to_string(),
    };
    insta::assert_snapshot!("swift_native_known_unformatted", formatted);
}

/// Known-unformatted Dart: missing trailing commas and inconsistent spacing.
/// Skipped when `dart` is not on PATH.
const DART_UNFORMATTED: &str = "void main(){print('hello');}\n";

#[test]
fn dart_native_known_unformatted_snapshot() {
    let engine = NativeToolEngine::for_language(Language::Dart);
    if !engine.is_available() {
        eprintln!("dart not found on PATH — skipping dart_native_known_unformatted_snapshot");
        return;
    }
    let src = make_src("main.dart", Language::Dart, DART_UNFORMATTED);
    let result = engine.format(&src, &enabled_cfg()).unwrap();
    let formatted = match result {
        FormatOutput::Formatted(s) => s,
        FormatOutput::Unchanged => panic!("expected dart format to reformat the source"),
    };
    insta::assert_snapshot!("dart_native_known_unformatted", formatted);
}

/// Known-unformatted Gleam: missing spaces around operators.
/// Skipped when `gleam` is not on PATH.
const GLEAM_UNFORMATTED: &str = concat!("pub fn main()->Nil{\n", "io.println(\"hello\")\n", "}\n",);

#[test]
fn gleam_native_known_unformatted_snapshot() {
    let engine = NativeToolEngine::for_language(Language::Gleam);
    if !engine.is_available() {
        eprintln!("gleam not found on PATH — skipping gleam_native_known_unformatted_snapshot");
        return;
    }
    let src = make_src("main.gleam", Language::Gleam, GLEAM_UNFORMATTED);
    let result = engine.format(&src, &enabled_cfg()).unwrap();
    let formatted = match result {
        FormatOutput::Formatted(s) => s,
        FormatOutput::Unchanged => panic!("expected gleam format to reformat the source"),
    };
    insta::assert_snapshot!("gleam_native_known_unformatted", formatted);
}

/// Without a `rustfmt.toml`, poly imposes no width and rustfmt applies its own
/// built-in default (100), exactly as `cargo fmt` does. A ~110-char function
/// signature is over rustfmt's default, so `Formatted` proves poly did NOT
/// inject its old opinionated `max_width = 120`.
///
/// Gated on `rustfmt` presence.
#[test]
fn rustfmt_uses_own_default_without_config() {
    let engine = NativeToolEngine::for_language(Language::Rust);
    if !engine.is_available() {
        eprintln!("rustfmt not found on PATH — skipping rustfmt_uses_own_default_without_config");
        return;
    }

    // Fresh temp dir with no rustfmt.toml anywhere in its ancestry.
    let tmp = tempfile::tempdir().expect("create temp dir for no-config rustfmt test");
    debug_assert!(
        tmp.path()
            .ancestors()
            .all(|dir| { !dir.join("rustfmt.toml").exists() && !dir.join(".rustfmt.toml").exists() }),
        "expected no rustfmt.toml in the temp dir ancestry"
    );

    // ~110-char first line: clean at max_width 120, reformatted by rustfmt at
    // its built-in default of 100. Formatted → poly did not force max_width=120.
    const SRC: &str = concat!(
        "fn function_with_long_name_here(",
        "first_parameter: String, second_parameter: String, third_param: u32) -> bool {\n",
        "    true\n",
        "}\n",
    );

    let src = SourceFile {
        path: tmp.path().join("main.rs"),
        language: Language::Rust,
        content: SRC.into(),
    };

    let result = engine.format(&src, &enabled_cfg()).unwrap();
    assert!(
        matches!(result, FormatOutput::Formatted(_)),
        "a ~110-char signature must be Formatted when poly defers to rustfmt's \
         100-column default; got Unchanged — poly may still be forcing max_width = 120"
    );
}
