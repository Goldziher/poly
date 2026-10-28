//! Parsers, against reports captured from the real tools.

use poly_catalog::Output;

use super::{Finding, parse};
use crate::engine::Severity;

fn one(findings: Vec<Finding>) -> Finding {
    assert_eq!(findings.len(), 1, "{findings:?}");
    findings.into_iter().next().expect("one finding")
}

#[test]
fn checkstyle_reports_become_rule_named_findings_with_decoded_messages() {
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<checkstyle version="12.3.1">
<file name="/repo/Bad.java">
<error line="3" column="15" severity="error" message="Method name &apos;Foo&apos; must match &quot;x&quot; &amp; y." source="com.puppycrawl.tools.checkstyle.checks.naming.MethodNameCheck"/>
<error line="9" severity="warning" message="a &gt; b" source="com.puppycrawl.tools.checkstyle.checks.whitespace.ParenPadCheck"/>
</file>
<file name="/repo/Clean.java">
</file>
</checkstyle>"#;
    let findings = parse(Output::CheckstyleXml, xml, "").expect("parses");
    assert_eq!(findings.len(), 2);
    assert_eq!(findings[0].path.as_deref(), Some("/repo/Bad.java"));
    assert_eq!((findings[0].line, findings[0].column), (Some(3), Some(15)));
    assert_eq!(findings[0].code.as_deref(), Some("MethodName"));
    assert_eq!(findings[0].message, "Method name 'Foo' must match \"x\" & y.");
    assert_eq!(findings[0].severity, Severity::Error);
    assert_eq!(findings[1].severity, Severity::Warning);
    assert_eq!(findings[1].message, "a > b");
    assert_eq!(findings[1].column, None);
}

#[test]
fn a_truncated_checkstyle_report_is_an_error_not_a_clean_run() {
    let crashed = "<?xml version=\"1.0\"?>\n<checkstyle version=\"12\">\n<file name=\"/x\">\n";
    assert!(parse(Output::CheckstyleXml, crashed, "").is_err());
    assert!(
        parse(
            Output::CheckstyleXml,
            "",
            "Files to process must be specified, found 0."
        )
        .is_err()
    );
}

#[test]
fn actionlint_findings_carry_kind_as_the_rule() {
    let json = r#"[{"message":"label \"x\" is unknown","filepath":".github/workflows/a.yml","line":25,"column":14,"kind":"runner-label","snippet":"","end_column":23}]"#;
    let finding = one(parse(Output::ActionlintJson, json, "").expect("parses"));
    assert_eq!(finding.path.as_deref(), Some(".github/workflows/a.yml"));
    assert_eq!(
        (finding.line, finding.column, finding.end_column),
        (Some(25), Some(14), Some(23))
    );
    assert_eq!(finding.code.as_deref(), Some("runner-label"));
    assert!(
        parse(Output::ActionlintJson, "", "")
            .expect("empty is clean")
            .is_empty()
    );
    assert!(parse(Output::ActionlintJson, "usage: actionlint", "").is_err());
}

#[test]
fn buf_prints_one_object_per_line() {
    let ndjson = concat!(
        r#"{"path":"a/b.proto","start_line":5,"start_column":1,"end_line":5,"end_column":53,"type":"COMPILE","message":"imported file does not exist"}"#,
        "\n",
        r#"{"path":"a/c.proto","start_line":2,"start_column":9,"end_line":2,"end_column":12,"type":"FIELD_LOWER_SNAKE_CASE","message":"bad"}"#,
        "\n"
    );
    let findings = parse(Output::BufJson, ndjson, "").expect("parses");
    assert_eq!(findings.len(), 2);
    assert_eq!(findings[1].code.as_deref(), Some("FIELD_LOWER_SNAKE_CASE"));
    assert_eq!(findings[1].end_column, Some(12));
    assert!(parse(Output::BufJson, "Failure: no such file", "").is_err());
}

#[test]
fn hadolint_levels_map_to_severities() {
    let json = r#"[{"code":"DL3007","column":1,"file":"./Dockerfile","level":"warning","line":2,"message":"pin"},
                   {"code":"DL1000","column":1,"file":"./Dockerfile","level":"error","line":1,"message":"bad"},
                   {"code":"SC2086","column":1,"file":"./Dockerfile","level":"style","line":4,"message":"quote"}]"#;
    let findings = parse(Output::HadolintJson, json, "").expect("parses");
    let severities: Vec<_> = findings.iter().map(|f| f.severity).collect();
    assert_eq!(severities, [Severity::Warning, Severity::Error, Severity::Hint]);
}

#[test]
fn golangci_issues_are_read_and_a_null_list_is_clean() {
    let json = r#"{"Issues":[{"FromLinter":"errcheck","Text":"unchecked","Severity":"","Pos":{"Filename":"pkg/a.go","Offset":10,"Line":7,"Column":3}}],"Report":{}}"#;
    let finding = one(parse(Output::GolangciJson, json, "").expect("parses"));
    assert_eq!(finding.code.as_deref(), Some("errcheck"));
    assert_eq!(
        (finding.path.as_deref(), finding.line, finding.column),
        (Some("pkg/a.go"), Some(7), Some(3))
    );
    assert_eq!(finding.severity, Severity::Error);
    assert!(
        parse(Output::GolangciJson, r#"{"Issues":null}"#, "")
            .expect("null")
            .is_empty()
    );
    assert!(parse(Output::GolangciJson, "level=error msg=boom", "").is_err());
}

#[test]
fn golangci_issues_with_suggested_fixes_or_a_replacement_are_fixable() {
    let json = r#"{"Issues":[
        {"FromLinter":"godot","Text":"a","Pos":{"Filename":"a.go","Line":1,"Column":1},"SuggestedFixes":[{"TextEdits":[]}]},
        {"FromLinter":"old","Text":"b","Pos":{"Filename":"a.go","Line":2,"Column":1},"Replacement":{"NeedOnlyDelete":true}},
        {"FromLinter":"none","Text":"c","Pos":{"Filename":"a.go","Line":3,"Column":1},"SuggestedFixes":null},
        {"FromLinter":"empty","Text":"d","Pos":{"Filename":"a.go","Line":4,"Column":1},"SuggestedFixes":[],"Replacement":null}
    ]}"#;
    let fixable: Vec<bool> = parse(Output::GolangciJson, json, "")
        .expect("parses")
        .iter()
        .map(|finding| finding.fixable)
        .collect();
    assert_eq!(fixable, [true, true, false, false]);
}

#[test]
fn buildifier_warnings_carry_category_url_and_fixability() {
    let json = r#"{"success":false,"files":[{"filename":"a/BUILD","formatted":true,"valid":true,"warnings":[{"start":{"line":3,"column":1},"end":{"line":3,"column":11},"category":"native-py","actionable":true,"autoFixable":true,"message":"Function \"py_library\" is not global anymore","url":"https://example/native-py"}]}]}"#;
    let finding = one(parse(Output::BuildifierJson, json, "").expect("parses"));
    assert_eq!(finding.code.as_deref(), Some("native-py"));
    assert!(finding.fixable);
    assert_eq!((finding.line, finding.end_column), (Some(3), Some(11)));
    assert_eq!(finding.url.as_deref(), Some("https://example/native-py"));
}

#[test]
fn a_buildifier_syntax_error_is_located_from_stderr() {
    let json = r#"{"success":false,"files":[{"filename":"/t/BUILD","formatted":false,"valid":false,"warnings":[]}]}"#;
    let finding = one(parse(Output::BuildifierJson, json, "/t/BUILD:3:1: syntax error\n").expect("parses"));
    assert_eq!(finding.code.as_deref(), Some("syntax"));
    assert_eq!((finding.line, finding.column), (Some(3), Some(1)));
    assert_eq!(finding.message, "syntax error");
    assert_eq!(finding.severity, Severity::Error);
}

#[test]
fn tflint_issues_and_errors_are_both_reported() {
    let json = r#"{"issues":[{"rule":{"name":"terraform_unused_declarations","severity":"warning","link":"https://l"},"message":"unused","range":{"filename":"main.tf","start":{"line":2,"column":1},"end":{"line":2,"column":9}}}],"errors":[{"message":"failed to load","range":{"filename":"x.tf","start":{"line":1,"column":1},"end":{"line":1,"column":2}}}]}"#;
    let findings = parse(Output::TflintJson, json, "").expect("parses");
    assert_eq!(findings.len(), 2);
    assert_eq!(findings[0].severity, Severity::Warning);
    assert_eq!(findings[0].code.as_deref(), Some("terraform_unused_declarations"));
    assert_eq!(findings[1].severity, Severity::Error);
}

#[test]
fn groovy_columns_are_one_based() {
    let json = r#"{"summary":{},"files":{"/r/a.groovy":{"errors":[{"id":0,"line":1,"rule":"NoDef","severity":"info","msg":"def should not be used","range":{"start":{"line":1,"character":0},"end":{"line":1,"character":3}}}]}}}"#;
    let finding = one(parse(Output::GroovyJson, json, "").expect("parses"));
    assert_eq!(
        (finding.line, finding.column, finding.end_column),
        (Some(1), Some(1), Some(4))
    );
    assert_eq!(finding.severity, Severity::Info);
    assert_eq!(finding.code.as_deref(), Some("NoDef"));
}

#[test]
fn helm_lint_lines_map_levels_and_template_errors_are_located() {
    let text = "==> Linting ./c\n[INFO] Chart.yaml: icon is recommended\n[WARNING] templates/rbac.yaml: v1beta1 is deprecated\n[ERROR] templates/: parse error at (c/templates/x.yaml:3): bad\n\n1 chart(s) linted, 1 chart(s) failed\n";
    let findings = parse(Output::HelmText, text, "").expect("parses");
    assert_eq!(findings.len(), 3);
    assert_eq!(findings[0].severity, Severity::Info);
    assert_eq!(findings[0].path.as_deref(), Some("Chart.yaml"));
    assert_eq!(findings[1].severity, Severity::Warning);
    assert_eq!(findings[2].severity, Severity::Error);
    assert_eq!(findings[2].path.as_deref(), Some("c/templates/x.yaml"));
    assert_eq!(findings[2].line, Some(3));

    let render = "Error: execution error at (coralogix/templates/secret.yaml:6:11): key is required!\n\nUse --debug flag to render out invalid YAML\n";
    let finding = one(parse(Output::HelmText, render, "").expect("parses"));
    assert_eq!(finding.path.as_deref(), Some("coralogix/templates/secret.yaml"));
    assert_eq!((finding.line, finding.column), (Some(6), Some(11)));
}

#[test]
fn a_summary_printed_after_the_report_is_ignored() {
    let json = "{\"Issues\":[]}\n7 issues:\n* gofmt: 5\n";
    assert!(parse(Output::GolangciJson, json, "").expect("parses").is_empty());
}

#[test]
fn path_line_reports_are_read_from_either_stream() {
    let findings = parse(Output::PathLine, "main.tf:12:3: bad block\n", "other.tf:4: also bad\n").expect("parses");
    assert_eq!(findings.len(), 2);
    assert_eq!((findings[0].line, findings[0].column), (Some(12), Some(3)));
    assert_eq!((findings[1].line, findings[1].column), (Some(4), None));
}

const HADOLINT: &str = include_str!("fixtures/hadolint.json");
const GOLANGCI: &str = include_str!("fixtures/golangci.json");
const TFLINT: &str = include_str!("fixtures/tflint.json");
const BUF_LINT: &str = include_str!("fixtures/buf_lint.ndjson");
const ACTIONLINT: &str = include_str!("fixtures/actionlint.json");
const CHECKSTYLE: &str = include_str!("fixtures/checkstyle.xml");
const HELM_LINT_CLEAN: &str = include_str!("fixtures/helm_lint_clean.txt");
const HELM_LINT_FAILING: &str = include_str!("fixtures/helm_lint_failing.txt");
const HELM_TEMPLATE_ERROR: &str = include_str!("fixtures/helm_template_error.txt");

#[test]
fn hadolint_2_15_report_is_read_from_a_real_run() {
    let findings = parse(Output::HadolintJson, HADOLINT, "").expect("parses");
    let codes: Vec<_> = findings.iter().map(|f| f.code.as_deref().unwrap_or_default()).collect();
    assert_eq!(codes, ["DL3006", "DL3015", "DL3014", "DL3008", "DL3003"]);
    let severities: Vec<_> = findings.iter().map(|f| f.severity).collect();
    assert_eq!(
        severities,
        [
            Severity::Warning,
            Severity::Info,
            Severity::Warning,
            Severity::Warning,
            Severity::Warning
        ]
    );
    assert_eq!(findings[0].path.as_deref(), Some("Dockerfile"));
    assert_eq!((findings[4].line, findings[4].column), (Some(3), Some(1)));
}

#[test]
fn golangci_lint_2_report_is_read_and_its_trailing_summary_ignored() {
    let finding = one(parse(Output::GolangciJson, GOLANGCI, "").expect("parses"));
    assert_eq!(finding.code.as_deref(), Some("errcheck"));
    assert_eq!(finding.message, "Error return value of `os.Remove` is not checked");
    assert_eq!(
        (finding.path.as_deref(), finding.line, finding.column),
        (Some("a.go"), Some(6), Some(11))
    );
    assert_eq!(finding.severity, Severity::Error);
}

#[test]
fn tflint_report_carries_rule_link_range_and_severity() {
    let findings = parse(Output::TflintJson, TFLINT, "").expect("parses");
    assert_eq!(findings.len(), 2);
    assert_eq!(findings[0].code.as_deref(), Some("terraform_typed_variables"));
    assert_eq!(findings[1].code.as_deref(), Some("terraform_unused_declarations"));
    assert!(findings.iter().all(|f| f.severity == Severity::Warning));
    assert!(findings[1].url.as_deref().is_some_and(|url| url.ends_with("terraform_unused_declarations.md")));
    assert_eq!(findings[0].path.as_deref(), Some("main.tf"));
    assert_eq!(
        (findings[0].line, findings[0].column, findings[0].end_column),
        (Some(1), Some(1), Some(18))
    );
    assert_eq!(findings[1].message, "variable \"unused\" is declared but not used");
}

#[test]
fn buf_lint_report_found_through_buf_yaml_names_the_module_relative_path() {
    let findings = parse(Output::BufJson, BUF_LINT, "").expect("parses");
    assert_eq!(findings.len(), 2);
    assert!(findings.iter().all(|f| f.path.as_deref() == Some("api/v1/thing.proto")));
    assert_eq!(findings[0].code.as_deref(), Some("MESSAGE_PASCAL_CASE"));
    assert_eq!(
        (findings[0].line, findings[0].column, findings[0].end_column),
        (Some(5), Some(9), Some(14))
    );
    assert_eq!(findings[1].code.as_deref(), Some("FIELD_LOWER_SNAKE_CASE"));
    assert_eq!((findings[1].line, findings[1].column), (Some(6), Some(10)));
}

#[test]
fn actionlint_report_locates_the_label_and_the_expression() {
    let findings = parse(Output::ActionlintJson, ACTIONLINT, "").expect("parses");
    assert_eq!(findings.len(), 2);
    assert_eq!(findings[0].code.as_deref(), Some("runner-label"));
    assert_eq!(findings[0].path.as_deref(), Some(".github/workflows/a.yml"));
    assert_eq!((findings[0].line, findings[0].column, findings[0].end_column), (Some(5), Some(14), Some(25)));
    assert_eq!(findings[1].code.as_deref(), Some("expression"));
    assert_eq!(findings[1].line, Some(7));
}

#[test]
fn checkstyle_14_report_names_the_check_after_its_source_class() {
    let findings = parse(Output::CheckstyleXml, CHECKSTYLE, "").expect("parses");
    let codes: Vec<_> = findings.iter().map(|f| f.code.as_deref().unwrap_or_default()).collect();
    assert_eq!(codes, ["TypeName", "MethodName"]);
    assert_eq!(findings[0].path.as_deref(), Some("/repo/Bad.java"));
    assert_eq!((findings[0].line, findings[0].column), (Some(1), Some(14)));
    assert_eq!(findings[0].message, "Name 'bad' must match pattern '^[A-Z][a-zA-Z0-9]*$'.");
}

#[test]
fn helm_lint_clean_run_has_only_the_icon_recommendation() {
    let finding = one(parse(Output::HelmText, HELM_LINT_CLEAN, "").expect("parses"));
    assert_eq!(finding.severity, Severity::Info);
    assert_eq!(finding.path.as_deref(), Some("Chart.yaml"));
}

#[test]
fn helm_lint_ignores_the_slog_line_and_keeps_the_long_warning_whole() {
    let findings = parse(Output::HelmText, HELM_LINT_FAILING, "").expect("parses");
    assert_eq!(findings.len(), 2, "{findings:?}");
    assert_eq!(findings[1].severity, Severity::Warning);
    assert_eq!(findings[1].path.as_deref(), Some("templates/cm.yaml"));
    assert!(findings[1].message.contains("a lowercase RFC 1123 subdomain"), "{findings:?}");
}

#[test]
fn helm_template_error_is_located_in_the_template() {
    let finding = one(parse(Output::HelmText, "", HELM_TEMPLATE_ERROR).expect("parses"));
    assert_eq!(finding.severity, Severity::Error);
    assert_eq!(finding.path.as_deref(), Some("c/templates/cm.yaml"));
    assert_eq!((finding.line, finding.column), (Some(6), Some(8)));
    assert_eq!(finding.message, "execution error at (c/templates/cm.yaml:6:8): name is required");
}
