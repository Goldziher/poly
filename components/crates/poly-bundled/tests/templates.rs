//! A bare Jinja template is left untouched by `poly fmt`, with a note on how to opt in. The formatter
//! that raises the note is the web family's `markup_fmt`, so this runs against the bundled binary.
#![cfg(unix)]

use std::process::{Command, Output};

const POLY: &str = env!("CARGO_BIN_EXE_poly");

fn combined(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn bare_jinja_template_is_preserved_and_reports_how_to_opt_in() {
    let dir = tempfile::tempdir().expect("tempdir");
    let template = dir.path().join("service.jinja");
    let content = "{% if docs %}\n/// <summary>\n/// {{ docs }}\n/// </summary>\n{% endif %}\n";
    std::fs::write(&template, content).expect("write template");

    let output = Command::new(POLY)
        .args(["fmt", "--fix", "--no-cache", "service.jinja"])
        .current_dir(dir.path())
        .output()
        .expect("run poly");
    let text = combined(&output);

    assert_eq!(output.status.code(), Some(0), "got:\n{text}");
    assert!(text.contains("ambiguous template target"), "got:\n{text}");
    assert!(
        text.contains("add .html or .xml before the template extension"),
        "got:\n{text}"
    );
    assert_eq!(std::fs::read_to_string(template).expect("read template"), content);
}
