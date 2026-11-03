//! Port of `hclwrite.Format`, the canonical layout `terraform fmt` and `terragrunt hclfmt` apply.

use std::ops::Range;

use super::lex::{self, Kind, Tok};

struct Line {
    lead: Range<usize>,
    assign: Option<Range<usize>>,
    comment: Option<Range<usize>>,
}

pub(super) fn format(src: &str, indent_width: usize) -> Option<String> {
    let mut toks = lex::lex(src)?;
    let lines = split_lines(&toks);
    indent(&mut toks, &lines, indent_width);
    spaces(&mut toks, &lines);
    cells(&mut toks, &lines);
    let mut out = String::with_capacity(src.len());
    for tok in &toks {
        out.extend(std::iter::repeat_n(' ', tok.spaces));
        out.push_str(&tok.text);
    }
    Some(out)
}

fn bracket_change(kind: Kind) -> i32 {
    match kind {
        Kind::OBrace | Kind::OBrack | Kind::OParen | Kind::Control | Kind::Interp => 1,
        Kind::CBrace | Kind::CBrack | Kind::CParen | Kind::SeqEnd => -1,
        _ => 0,
    }
}

fn ends_line(tok: &Tok) -> bool {
    tok.kind == Kind::Newline || (tok.kind == Kind::Comment && tok.text.ends_with('\n'))
}

fn split_lines(toks: &[Tok]) -> Vec<Line> {
    let mut ranges = Vec::new();
    let mut start = 0;
    for (index, tok) in toks.iter().enumerate() {
        if ends_line(tok) {
            ranges.push(start..index + 1);
            start = index + 1;
        }
    }
    ranges.push(start..toks.len());

    ranges
        .into_iter()
        .map(|range| {
            let mut line = Line {
                lead: range,
                assign: None,
                comment: None,
            };
            if line.lead.len() > 1 && toks[line.lead.end - 1].kind == Kind::Comment {
                line.comment = Some(line.lead.end - 1..line.lead.end);
                line.lead.end -= 1;
            }
            let equal = (line.lead.start + 1..line.lead.end).find(|&i| toks[i].kind == Kind::Equal);
            if let Some(index) = equal {
                let net: i32 = toks[index..line.lead.end].iter().map(|t| bracket_change(t.kind)).sum();
                if net == 0 {
                    line.assign = Some(index..line.lead.end);
                    line.lead.end = index;
                }
            }
            line
        })
        .collect()
}

fn indent(toks: &mut [Tok], lines: &[Line], width: usize) {
    let mut indents: Vec<i32> = Vec::new();
    for line in lines {
        if line.lead.is_empty() {
            continue;
        }
        let first = line.lead.start;
        if toks[first].kind == Kind::Newline {
            toks[first].spaces = 0;
            continue;
        }
        let mut net = 0;
        for tok in &toks[line.lead.clone()] {
            net += bracket_change(tok.kind);
            if tok.kind == Kind::OHeredoc {
                break;
            }
        }
        if let Some(assign) = &line.assign {
            net += toks[assign.clone()].iter().map(|t| bracket_change(t.kind)).sum::<i32>();
        }
        if net > 0 {
            toks[first].spaces = width * indents.len();
            indents.push(net);
            continue;
        }
        let mut closed = -net;
        while closed > 0 {
            let Some(top) = indents.last_mut() else { break };
            if closed > *top {
                closed -= *top;
                indents.pop();
            } else if closed < *top {
                *top -= closed;
                closed = 0;
            } else {
                indents.pop();
                closed = 0;
            }
        }
        toks[first].spaces = width * indents.len();
    }
}

fn spaces(toks: &mut [Tok], lines: &[Line]) {
    for line in lines {
        space_cell(toks, line.lead.clone(), false);
        if let Some(assign) = &line.assign {
            space_cell(toks, assign.clone(), true);
        }
    }
}

fn space_cell(toks: &mut [Tok], cell: Range<usize>, force_first: bool) {
    if force_first && !cell.is_empty() {
        toks[cell.start].spaces = 1;
    }
    for i in cell.clone() {
        if i + 1 >= cell.end {
            continue;
        }
        let before = (i > cell.start).then(|| toks[i - 1].kind);
        let wants = space_after(&toks[i], before, &toks[i + 1]);
        toks[i + 1].spaces = usize::from(wants);
    }
}

fn space_after(subject: &Tok, before: Option<Kind>, after: &Tok) -> bool {
    use Kind::{
        Bang, CBrace, CHeredoc, CQuote, Colon, Comma, Control, Dot, DoubleColon, Ellipsis, Equal, Ident, Interp, Minus,
        Newline, OBrace, OBrack, OHeredoc, OParen, OQuote, Other, QuotedLit, SeqEnd, StringLit,
    };
    let (s, a) = (subject.kind, after.kind);
    if a == Newline {
        return false;
    }
    if s == Ident && a == OParen {
        return false;
    }
    if (s == Ident && a == DoubleColon) || (s == DoubleColon && a == Ident) {
        return false;
    }
    if s == Dot || a == Dot || a == Comma || a == Ellipsis {
        return false;
    }
    if s == Comma {
        return true;
    }
    if matches!(s, QuotedLit | StringLit | OQuote | OHeredoc) || matches!(a, QuotedLit | StringLit | CQuote | CHeredoc)
    {
        return false;
    }
    if s == Ident && subject.text == "in" && before == Some(Ident) {
        return true;
    }
    if a == OBrack && (matches!(s, Ident | Kind::Number) || bracket_change(s) < 0) {
        return false;
    }
    if s == Bang {
        return false;
    }
    if s == Minus {
        return !matches!(
            before,
            None | Some(OParen | OBrace | OBrack | Equal | Colon | Comma | Minus | Other | Bang)
        );
    }
    if s == OBrace || a == CBrace {
        return !(s == OBrace && a == CBrace);
    }
    if matches!(s, Interp | Control) && a == OBrace {
        return true;
    }
    if s == CBrace && a == SeqEnd {
        return true;
    }
    if s == SeqEnd && matches!(a, Interp | Control) {
        return false;
    }
    if bracket_change(s) > 0 || bracket_change(a) < 0 {
        return false;
    }
    true
}

fn columns(toks: &[Tok], range: Option<&Range<usize>>) -> usize {
    range.map_or(0, |r| {
        toks[r.clone()].iter().map(|t| t.spaces + t.text.chars().count()).sum()
    })
}

fn cells(toks: &mut [Tok], lines: &[Line]) {
    let mut start = None;
    let mut max = 0;
    for i in 0..=lines.len() {
        let chained = lines.get(i).is_some_and(|l| l.assign.is_some());
        if chained {
            start.get_or_insert(i);
            max = max.max(columns(toks, Some(&lines[i].lead)));
        } else if let Some(first) = start.take() {
            for line in &lines[first..i] {
                let used = columns(toks, Some(&line.lead));
                if let Some(assign) = &line.assign {
                    toks[assign.start].spaces = max - used + 1;
                }
            }
            max = 0;
        }
    }

    for i in 0..=lines.len() {
        let chained = lines.get(i).is_some_and(|l| l.comment.is_some());
        if chained {
            start.get_or_insert(i);
            let used = columns(toks, Some(&lines[i].lead)) + columns(toks, lines[i].assign.as_ref());
            max = max.max(used);
        } else if let Some(first) = start.take() {
            for line in &lines[first..i] {
                let used = columns(toks, Some(&line.lead)) + columns(toks, line.assign.as_ref());
                if let Some(comment) = &line.comment {
                    toks[comment.start].spaces = max - used + 1;
                }
            }
            max = 0;
        }
    }
}
