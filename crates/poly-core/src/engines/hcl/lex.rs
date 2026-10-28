//! HCL scanner producing the token stream the `hclwrite` formatting port consumes.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    Newline,
    Comment,
    Ident,
    Number,
    OBrace,
    CBrace,
    OBrack,
    CBrack,
    OParen,
    CParen,
    OQuote,
    CQuote,
    QuotedLit,
    StringLit,
    OHeredoc,
    CHeredoc,
    Interp,
    Control,
    SeqEnd,
    Dot,
    Ellipsis,
    Comma,
    Equal,
    Colon,
    DoubleColon,
    Minus,
    Bang,
    FatArrow,
    Other,
}

#[derive(Clone, Debug)]
pub(super) struct Tok {
    pub kind: Kind,
    pub text: String,
    pub spaces: usize,
}

pub(super) fn lex(src: &str) -> Option<Vec<Tok>> {
    let mut lexer = Lexer {
        src: src.as_bytes(),
        text: src,
        pos: 0,
        out: Vec::new(),
    };
    lexer.code(false)?;
    Some(lexer.out)
}

struct Lexer<'a> {
    src: &'a [u8],
    text: &'a str,
    pos: usize,
    out: Vec<Tok>,
}

const OPERATORS: &[(&str, Kind)] = &[
    ("...", Kind::Ellipsis),
    ("::", Kind::DoubleColon),
    ("=>", Kind::FatArrow),
    ("==", Kind::Other),
    ("!=", Kind::Other),
    ("<=", Kind::Other),
    (">=", Kind::Other),
    ("&&", Kind::Other),
    ("||", Kind::Other),
    ("{", Kind::OBrace),
    ("}", Kind::CBrace),
    ("[", Kind::OBrack),
    ("]", Kind::CBrack),
    ("(", Kind::OParen),
    (")", Kind::CParen),
    (".", Kind::Dot),
    (",", Kind::Comma),
    ("=", Kind::Equal),
    (":", Kind::Colon),
    ("-", Kind::Minus),
    ("!", Kind::Bang),
    ("<", Kind::Other),
    (">", Kind::Other),
    ("+", Kind::Other),
    ("*", Kind::Other),
    ("/", Kind::Other),
    ("%", Kind::Other),
    ("?", Kind::Other),
];

impl Lexer<'_> {
    fn peek(&self, offset: usize) -> Option<u8> {
        self.src.get(self.pos + offset).copied()
    }

    fn rest(&self) -> &str {
        &self.text[self.pos..]
    }

    fn push(&mut self, kind: Kind, start: usize) {
        self.out.push(Tok {
            kind,
            text: self.text[start..self.pos].to_owned(),
            spaces: 0,
        });
    }

    fn code(&mut self, in_template: bool) -> Option<()> {
        let mut depth = 0usize;
        while let Some(byte) = self.peek(0) {
            let start = self.pos;
            match byte {
                b' ' | b'\t' => self.pos += 1,
                b'\r' => return None,
                b'\n' => {
                    self.pos += 1;
                    self.push(Kind::Newline, start);
                }
                b'#' => self.line_comment(start),
                b'/' if self.peek(1) == Some(b'/') => self.line_comment(start),
                b'/' if self.peek(1) == Some(b'*') => {
                    let end = self.rest().find("*/")?;
                    self.pos += end + 2;
                    self.push(Kind::Comment, start);
                }
                b'"' => {
                    self.pos += 1;
                    self.push(Kind::OQuote, start);
                    self.quoted()?;
                }
                b'<' if self.peek(1) == Some(b'<') => self.heredoc(start)?,
                b'~' if in_template && depth == 0 && self.peek(1) == Some(b'}') => {
                    self.pos += 2;
                    self.push(Kind::SeqEnd, start);
                    return Some(());
                }
                b'}' if in_template && depth == 0 => {
                    self.pos += 1;
                    self.push(Kind::SeqEnd, start);
                    return Some(());
                }
                b'0'..=b'9' => self.number(start),
                _ if is_ident_start(self.rest().chars().next()?) => {
                    let len = self
                        .rest()
                        .char_indices()
                        .find(|&(_, c)| !is_ident_continue(c))
                        .map_or(self.rest().len(), |(index, _)| index);
                    self.pos += len;
                    self.push(Kind::Ident, start);
                }
                _ => {
                    let (op, kind) = OPERATORS.iter().find(|(op, _)| self.rest().starts_with(op))?;
                    match kind {
                        Kind::OBrace => depth += 1,
                        Kind::CBrace => depth = depth.saturating_sub(1),
                        _ => {}
                    }
                    self.pos += op.len();
                    self.push(*kind, start);
                }
            }
        }
        (!in_template).then_some(())
    }

    fn line_comment(&mut self, start: usize) {
        let end = self.rest().find('\n').map_or(self.rest().len(), |index| index + 1);
        self.pos += end;
        self.push(Kind::Comment, start);
    }

    fn number(&mut self, start: usize) {
        self.digits();
        if self.peek(0) == Some(b'.') && self.peek(1).is_some_and(|b| b.is_ascii_digit()) {
            self.pos += 1;
            self.digits();
        }
        if matches!(self.peek(0), Some(b'e' | b'E')) {
            let sign = usize::from(matches!(self.peek(1), Some(b'+' | b'-')));
            if self.peek(1 + sign).is_some_and(|b| b.is_ascii_digit()) {
                self.pos += 1 + sign;
                self.digits();
            }
        }
        self.push(Kind::Number, start);
    }

    fn digits(&mut self) {
        while self.peek(0).is_some_and(|b| b.is_ascii_digit()) {
            self.pos += 1;
        }
    }

    fn quoted(&mut self) -> Option<()> {
        let mut lit_start = self.pos;
        loop {
            match self.peek(0)? {
                b'"' => {
                    self.flush_lit(Kind::QuotedLit, lit_start);
                    let start = self.pos;
                    self.pos += 1;
                    self.push(Kind::CQuote, start);
                    return Some(());
                }
                b'\n' | b'\r' => return None,
                b'\\' => self.pos += 2,
                _ => {
                    if !self.sequence(Kind::QuotedLit, &mut lit_start)? {
                        self.pos += 1;
                    }
                }
            }
        }
    }

    fn flush_lit(&mut self, kind: Kind, start: usize) {
        if self.pos > start {
            self.push(kind, start);
        }
    }

    fn sequence(&mut self, kind: Kind, lit_start: &mut usize) -> Option<bool> {
        let opener = match (self.peek(0)?, self.peek(1), self.peek(2)) {
            (b'$', Some(b'$'), Some(b'{')) | (b'%', Some(b'%'), Some(b'{')) => {
                self.pos += 3;
                return Some(true);
            }
            (b'$', Some(b'{'), _) => Kind::Interp,
            (b'%', Some(b'{'), _) => Kind::Control,
            _ => return Some(false),
        };
        self.flush_lit(kind, *lit_start);
        let start = self.pos;
        self.pos += if self.peek(2) == Some(b'~') { 3 } else { 2 };
        self.push(opener, start);
        self.code(true)?;
        *lit_start = self.pos;
        Some(true)
    }

    fn heredoc(&mut self, start: usize) -> Option<()> {
        self.pos += 2;
        if self.peek(0) == Some(b'-') {
            self.pos += 1;
        }
        let marker_start = self.pos;
        let len = self
            .rest()
            .char_indices()
            .find(|&(_, c)| !is_ident_continue(c))
            .map_or(self.rest().len(), |(index, _)| index);
        if len == 0 {
            return None;
        }
        let marker = self.text[marker_start..marker_start + len].to_owned();
        self.pos += len;
        if self.peek(0) != Some(b'\n') {
            return None;
        }
        self.pos += 1;
        self.push(Kind::OHeredoc, start);
        let mut lit_start = self.pos;
        loop {
            let line_start = self.pos;
            let line_end = self.text[line_start..]
                .find('\n')
                .map_or(self.text.len(), |i| line_start + i);
            if self.text[line_start..line_end].trim() == marker {
                self.flush_lit(Kind::StringLit, lit_start);
                self.pos = line_end;
                self.push(Kind::CHeredoc, line_start);
                return Some(());
            }
            loop {
                if self.pos >= self.text.len() {
                    return None;
                }
                if self.peek(0) == Some(b'\n') {
                    self.pos += 1;
                    break;
                }
                if !self.sequence(Kind::StringLit, &mut lit_start)? {
                    self.pos += 1;
                }
            }
        }
    }
}

fn is_ident_start(c: char) -> bool {
    c == '_' || c.is_alphabetic()
}

fn is_ident_continue(c: char) -> bool {
    c == '_' || c == '-' || c.is_alphanumeric()
}
