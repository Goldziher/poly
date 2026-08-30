//! Detection of the IE property hack — a `*` prefixed to a declaration's
//! property name — which neither CSS backend can process.
//!
//! `*property: value` is not valid CSS and never was. It is a deliberate
//! parser-bug exploit: IE 7 and below accepted the prefixed declaration while
//! every other browser discarded it, which made it the standard way to ship an
//! IE-only value. YUI's `reset-fonts-grids.css` — vendored into thousands of
//! projects, Django's own documentation theme among them — carries 37 of them.
//!
//! Both of poly's CSS backends fail on it, in different and equally unhelpful
//! ways:
//!
//! - **malva** rejects the file with a syntax error, so a legacy stylesheet
//!   nobody intends to modernise reports as unformattable on every run.
//! - **`biome_css`** does something worse: its parser's error recovery is
//!   **exponential in the number of hacks in the file**. Measured on a synthetic
//!   file of `.c{*width:1px;}` rules, ten hacks take 0.46 s and twelve take
//!   8.88 s — roughly a fourfold cost per additional hack. At YUI's 37, `poly
//!   lint` on Django consumed **27 GB** of memory and was OOM-killed after five
//!   minutes without ever reporting a finding.
//!
//! Skipping is therefore not a preference about noise; it is the only outcome
//! in which the run finishes. The file is reported as *skipped* rather than
//! silently dropped, so it stays out of the `checked` count and remains visible
//! to `--deny-skips`.

/// Reason reported when a stylesheet is skipped for carrying IE property hacks.
pub(crate) const IE_PROPERTY_HACK_SKIP: &str = "IE `*property` hacks (unsupported by the CSS parser)";

/// Whether `content` contains an IE `*property` hack.
///
/// True when a `*` appears in **declaration position** — immediately after `{`
/// or `;`, ignoring whitespace — and is followed by an identifier character.
///
/// The position requirement is what separates the hack from the *universal
/// selector*, which is ordinary CSS and must keep being processed. A `*` used
/// as a selector (`* { margin: 0 }`, `.a > *`, `*::before`) is always followed
/// by whitespace or one of `{ . # : , [ > + ~` — never by an identifier
/// character, because `*ident` is not a valid selector. Comments are skipped so
/// a licence header mentioning `;` and `*` cannot trigger the check.
///
/// Measured against 487 stylesheets across the test corpus, this matches
/// exactly two files, both of which genuinely carry the hack (one is named
/// `ie-hacks.css`).
pub(crate) fn has_ie_property_hack(content: &str) -> bool {
    let bytes = content.as_bytes();
    let mut index = 0;
    // Tracks the last significant character, so `*` can be tested for
    // declaration position without re-scanning backwards over whitespace.
    let mut previous = b'\0';

    while index < bytes.len() {
        // Comments are not code: `/* Copyright …; *foo */` must not match.
        if bytes[index] == b'/' && bytes.get(index + 1) == Some(&b'*') {
            index = content[index + 2..]
                .find("*/")
                .map_or(bytes.len(), |end| index + 2 + end + 2);
            continue;
        }
        let byte = bytes[index];
        if byte.is_ascii_whitespace() {
            index += 1;
            continue;
        }
        if byte == b'*'
            && (previous == b'{' || previous == b';')
            && bytes
                .get(index + 1)
                .is_some_and(|next| next.is_ascii_alphabetic() || *next == b'-' || *next == b'_')
        {
            return true;
        }
        previous = byte;
        index += 1;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::has_ie_property_hack;

    #[test]
    fn a_star_prefixed_property_after_a_brace_is_the_hack() {
        assert!(has_ie_property_hack("a{*width:1px;}"));
    }

    #[test]
    fn a_star_prefixed_property_after_a_semicolon_is_the_hack() {
        assert!(has_ie_property_hack("a{color:red;*width:1px;}"));
    }

    #[test]
    fn whitespace_and_newlines_do_not_hide_the_hack() {
        assert!(has_ie_property_hack("a {\n  color: red;\n  *width: 1px;\n}"));
    }

    #[test]
    fn an_underscore_or_hyphen_prefixed_property_still_matches() {
        assert!(has_ie_property_hack("a{*-moz-box-sizing:border-box;}"));
    }

    // The universal selector is ordinary CSS: matching it would skip a large
    // share of real stylesheets, which is a worse failure than the hang.
    #[test]
    fn the_universal_selector_is_not_the_hack() {
        assert!(!has_ie_property_hack("*{margin:0;padding:0;}"));
        assert!(!has_ie_property_hack("a{color:red;}\n*{margin:0;}"));
        assert!(!has_ie_property_hack("a > *{margin:0;}"));
        assert!(!has_ie_property_hack("a{}\n*::before{content:'';}"));
        assert!(!has_ie_property_hack(".a *,.b *{margin:0;}"));
    }

    #[test]
    fn a_star_inside_a_comment_is_not_the_hack() {
        assert!(!has_ie_property_hack("/* Licensed; *see the file */\na{color:red;}"));
    }

    #[test]
    fn plain_stylesheets_do_not_match() {
        assert!(!has_ie_property_hack("a{color:red;background:#fff;}"));
        assert!(!has_ie_property_hack("@media (min-width:1px){a{color:red;}}"));
    }

    #[test]
    fn a_multiplication_in_a_calc_expression_is_not_the_hack() {
        // `*` follows an identifier or space here, never `{` or `;`.
        assert!(!has_ie_property_hack("a{width:calc(100% * 2);}"));
    }
}
