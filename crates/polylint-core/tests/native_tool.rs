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
//! - `disabled_is_tier2` — with no config (enabled=false, the default),
//!   `NativeToolEngine` must produce byte-identical output to `TreeSitterEngine`
//!   for Go — proving the default-off invariant.

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
    EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width: 4,
        options: toml::Table::new(),
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
// Default-off invariant
// ---------------------------------------------------------------------------

/// With no config (enabled = false, the default), `NativeToolEngine` for Go
/// must produce byte-identical output to a direct `TreeSitterEngine` call.
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
