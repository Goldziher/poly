//! Printing the `poly components` documents: pretty, JSON or TOON.

use serde::Serialize;

use super::ops::{Actions, Listing};
use crate::OutputFormat;

/// Print `document` in `format`; `pretty` renders the human form.
///
/// # Errors
///
/// When the document cannot be serialized.
pub fn emit<T: Serialize>(format: OutputFormat, document: &T, pretty: impl FnOnce(&T)) -> anyhow::Result<()> {
    match format {
        OutputFormat::Pretty => pretty(document),
        OutputFormat::Json => println!("{}", poly_core::report::render_json(document)?),
        OutputFormat::Toon => println!("{}", poly_core::report::render_toon(document)?),
    }
    Ok(())
}

/// The human form of `list` and `status`.
pub fn print_listing(listing: &Listing) {
    println!("flavor      {}", listing.flavor);
    println!("cache       {}", listing.cache_root);
    println!("offline     {}", listing.offline);
    if let Some(lock) = &listing.lock {
        println!("lock        none ({lock})");
    }
    if listing.components.is_empty() {
        println!("no downloadable components needed");
        return;
    }
    let width = listing.components.iter().map(|c| c.name.len()).max().unwrap_or(0);
    let state = listing
        .components
        .iter()
        .map(|c| c.state.label().len())
        .max()
        .unwrap_or(0);
    let version = listing
        .components
        .iter()
        .map(|c| c.version.as_deref().map_or(1, str::len))
        .max()
        .unwrap_or(1);
    for component in &listing.components {
        let mut line = format!(
            "{:<width$}  {:<state$}  {:<version$}  {}",
            component.name,
            component.state.label(),
            component.version.as_deref().unwrap_or("-"),
            component.engines.join(", ")
        );
        if component.opt_in {
            line.push_str("  (opt-in)");
        }
        match component.verified {
            Some(true) => line.push_str("  verified"),
            Some(false) => line.push_str("  VERIFICATION FAILED"),
            None => {}
        }
        if let Some(detail) = &component.detail {
            line.push_str(&format!("  [{detail}]"));
        }
        println!("{line}");
    }
    if let Some(missing) = listing.missing.as_ref().filter(|m| !m.is_empty()) {
        println!("missing: {} (run `poly components sync`)", missing.join(", "));
    }
}

/// The human form of the removing and installing commands.
pub fn print_actions(actions: &Actions) {
    for result in &actions.results {
        match &result.detail {
            Some(detail) => println!("{:<18} {}  {detail}", result.action, result.name),
            None => println!("{:<18} {}", result.action, result.name),
        }
    }
    if let Some(freed) = actions.freed_bytes {
        println!("freed {freed} bytes");
    }
    for note in &actions.notes {
        println!("note: {note}");
    }
}
