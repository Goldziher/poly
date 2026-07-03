//! Human-readable rendering of a [`MigrationPlan`]: the planned `poly.toml`, the
//! per-tool absorb verdicts, and the KEEP / DELETE / STRIP / REPORT decisions.

use std::fmt::Write as _;

use super::MigrationPlan;
use super::deletion::Action;
use super::importers::Absorb;

/// Render a full plan report. `write` indicates whether the plan has been (or is
/// about to be) applied, tuning the header wording only.
pub fn render_plan(plan: &MigrationPlan, write: bool) -> String {
    let mut out = String::new();
    let heading = if write { "migrated" } else { "planned migration for" };
    let _ = writeln!(out, "== {heading} {} ==", plan.dir.display());

    render_tools(&mut out, plan);
    render_actions(&mut out, plan);
    render_conflicts(&mut out, plan);
    render_poly_toml(&mut out, plan, write);
    out
}

fn render_tools(out: &mut String, plan: &MigrationPlan) {
    let absorbed: Vec<_> = plan.results.iter().filter(|r| r.absorb != Absorb::None).collect();
    if absorbed.is_empty() {
        let _ = writeln!(out, "\nNo absorbable tool configs found.");
        return;
    }
    let _ = writeln!(out, "\nAbsorbed configs:");
    for result in absorbed {
        let sources: Vec<String> = result.sources.iter().map(|p| p.display().to_string()).collect();
        let verdict = match &result.absorb {
            Absorb::Full => "FULL".to_string(),
            Absorb::Partial(keys) => format!("PARTIAL (unrepresented: {})", keys.join(", ")),
            Absorb::None => "NONE".to_string(),
        };
        let _ = writeln!(out, "  {:<12} {verdict}  <- {}", result.tool, sources.join(", "));
        for note in &result.notes {
            let _ = writeln!(out, "      note: {note}");
        }
    }
}

fn render_actions(out: &mut String, plan: &MigrationPlan) {
    let _ = writeln!(out, "\nFile decisions:");
    for action in plan.actions.iter().chain(plan.kept.iter()) {
        let line = match action {
            Action::DeleteFile(p) => format!("  DELETE  {}", p.display()),
            Action::StripPyproject { path, sections } => {
                let names: Vec<String> = sections.iter().map(|s| format!("[{}]", s.join("."))).collect();
                format!("  STRIP   {} ({})", path.display(), names.join(", "))
            }
            Action::Keep { path, reason } => format!("  KEEP    {} — {reason}", path.display()),
            Action::ReportOnly { path, note } => format!("  REPORT  {} — {note}", path.display()),
        };
        let _ = writeln!(out, "{line}");
    }
}

fn render_conflicts(out: &mut String, plan: &MigrationPlan) {
    if plan.conflicts.is_empty() {
        return;
    }
    let _ = writeln!(out, "\nConflicts (existing poly.toml values kept):");
    for conflict in &plan.conflicts {
        let _ = writeln!(out, "  {conflict}");
    }
}

fn render_poly_toml(out: &mut String, plan: &MigrationPlan, write: bool) {
    let verb = if write {
        "poly.toml written to"
    } else {
        "planned poly.toml (not written) at"
    };
    let _ = writeln!(out, "\n--- {verb} {} ---", plan.poly_toml_path.display());
    let rendered = plan.doc.to_string();
    let _ = write!(out, "{}", rendered);
    if !rendered.ends_with('\n') {
        let _ = writeln!(out);
    }
    let _ = writeln!(out, "--- end poly.toml ---");
}
