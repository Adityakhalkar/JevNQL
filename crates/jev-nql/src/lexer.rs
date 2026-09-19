//! NQL tokens.

use crate::NqlError;

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    /// Identifier or keyword (keywords are matched case-insensitively).
    Word(String),
    Int(i64),
    Float(f64),
    /// `'single quoted'`: a string literal.
    Str(String),
    /// `"double quoted"`: a semantic judgment.
    Judgment(String),
    Sym(&'static str),
    Eof,
}

#[derive(Debug, Clone)]
pub struct Token {
    pub tok: Tok,
    pub line: usize,
    pub col: usize,
}

const SYMBOLS: &[&str] = &["<=", ">=", "!=", "<>", "(", ")", ",", ":", ";", "=", "<", ">", "+", "-", "*", "/"];

pub fn lex(src: &str) -> Result<Vec<Token>, NqlError> {
    let chars: Vec<char> = src.chars().collect();
    let (mut i, mut line, mut col) = (0, 1, 1);
    let mut out = Vec::new();
    let advance = |i: &mut usize, line: &mut usize, col: &mut usize, n: usize| {
        for _ in 0..n {
            if chars[*i] == '\n' {
                *line += 1;
                *col = 1;
            } else {
                *col += 1;
            }
            *i += 1;
        }
    };
    while i < chars.len() {
        let c = chars[i];
        let (tl, tc) = (line, col);
        let push = |out: &mut Vec<Token>, tok| out.push(Token { tok, line: tl, col: tc });
        if c.is_whitespace() {
            advance(&mut i, &mut line, &mut col, 1);
        } else if c == '-' && chars.get(i + 1) == Some(&'-') {
            // comment to end of line
            while i < chars.len() && chars[i] != '\n' {
                advance(&mut i, &mut line, &mut col, 1);
            }
        } else if c == '\'' || c == '"' {
            let mut j = i + 1;
            let mut text = String::new();
            loop {
                match chars.get(j) {
                    None => return Err(NqlError::at(tl, tc, format!("unterminated {c}quoted{c} text"))),
                    // a doubled quote inside the text is a literal quote
                    Some(&q) if q == c && chars.get(j + 1) == Some(&c) => {
                        text.push(c);
                        j += 2;
                    }
                    Some(&q) if q == c => break,
                    Some(&ch) => {
                        text.push(ch);
                        j += 1;
                    }
                }
            }
            push(&mut out, if c == '\'' { Tok::Str(text) } else { Tok::Judgment(text) });
            let n = j + 1 - i;
            advance(&mut i, &mut line, &mut col, n);
        } else if c.is_ascii_digit() {
            let mut j = i;
            while j < chars.len() && (chars[j].is_ascii_digit() || chars[j] == '_') {
                j += 1;
            }
            let is_float = chars.get(j) == Some(&'.') && chars.get(j + 1).is_some_and(char::is_ascii_digit);
            if is_float {
                j += 1;
                while j < chars.len() && chars[j].is_ascii_digit() {
                    j += 1;
                }
            }
            let text: String = chars[i..j].iter().filter(|&&ch| ch != '_').collect();
            let tok = match is_float {
                true => Tok::Float(text.parse().expect("digits")),
                false => Tok::Int(text.parse().map_err(|_| NqlError::at(tl, tc, format!("number `{text}` is too large")))?),
            };
            push(&mut out, tok);
            let n = j - i;
            advance(&mut i, &mut line, &mut col, n);
        } else if c.is_alphabetic() || c == '_' {
            let mut j = i;
            while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '_') {
                j += 1;
            }
            push(&mut out, Tok::Word(chars[i..j].iter().collect()));
            let n = j - i;
            advance(&mut i, &mut line, &mut col, n);
        } else {
            let rest: String = chars[i..chars.len().min(i + 2)].iter().collect();
            let sym = SYMBOLS
                .iter()
                .find(|s| rest.starts_with(**s))
                .ok_or_else(|| NqlError::at(tl, tc, format!("unexpected character `{c}`")))?;
            push(&mut out, Tok::Sym(sym));
            advance(&mut i, &mut line, &mut col, sym.len());
        }
    }
    out.push(Token { tok: Tok::Eof, line, col });
    Ok(out)
}
