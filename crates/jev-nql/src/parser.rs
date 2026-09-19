//! NQL parser: tokens -> [`Query`].

use jevir::expr::{col, lit};
use jevir::{AggFunc, BinaryOp, Expr, Function, Scalar, SortKey, UnaryOp};
use serde_json::json;

use crate::NqlError;
use crate::lexer::{Tok, Token};

#[derive(Debug, Clone)]
pub struct Query {
    pub from: String,
    pub withs: Vec<With>,
    pub conditions: Vec<Condition>,
    pub judges: Vec<Judge>,
    pub rank: Option<Rank>,
    pub returns: Option<Vec<ReturnItem>>,
}

#[derive(Debug, Clone)]
pub struct With {
    pub table: String,
    pub alias: String,
    pub on: Option<String>,
    pub kind: WithKind,
}

#[derive(Debug, Clone)]
pub enum WithKind {
    /// Related rows as a list: `(LAST 30 BY created_at FIELDS (text, rating))`.
    History { order: Vec<SortKey>, limit: Option<usize>, fields: Option<Vec<String>> },
    /// A per-row aggregate: `(SUM amount WHERE order_date >= DATE '2026-01-01')`.
    Aggregate { func: AggFunc, arg: Option<Expr>, filter: Option<Expr> },
}

#[derive(Debug, Clone)]
pub enum Condition {
    Expr(Expr),
    /// A double-quoted semantic judgment.
    Judgment { text: String, using: Option<Vec<String>> },
}

#[derive(Debug, Clone)]
pub enum Judge {
    Score { name: String, question: String, levels: Option<Vec<String>>, using: Option<Vec<String>> },
    Classify { name: String, question: String, labels: Vec<String>, using: Option<Vec<String>> },
}

#[derive(Debug, Clone)]
pub struct Rank {
    pub keys: Vec<SortKey>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct ReturnItem {
    pub expr: Expr,
    pub alias: Option<String>,
}

/// Words that start a clause; they end the previous clause.
const CLAUSES: &[&str] = &["WITH", "FIND", "WHERE", "SCORE", "CLASSIFY", "RANK", "LIMIT", "RETURN"];

pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

type R<T> = Result<T, NqlError>;

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Self { tokens, pos: 0 }
    }

    fn peek(&self) -> &Token {
        &self.tokens[self.pos]
    }

    fn peek_at(&self, n: usize) -> &Tok {
        &self.tokens[(self.pos + n).min(self.tokens.len() - 1)].tok
    }

    fn bump(&mut self) -> Token {
        let t = self.tokens[self.pos].clone();
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
        t
    }

    fn err<T>(&self, message: impl Into<String>) -> R<T> {
        let t = self.peek();
        Err(NqlError::at(t.line, t.col, message))
    }

    fn found(&self) -> String {
        match &self.peek().tok {
            Tok::Word(w) => format!("`{w}`"),
            Tok::Int(v) => format!("`{v}`"),
            Tok::Float(v) => format!("`{v}`"),
            Tok::Str(s) => format!("'{s}'"),
            Tok::Judgment(s) => format!("\"{s}\""),
            Tok::Sym(s) => format!("`{s}`"),
            Tok::Eof => "end of query".into(),
        }
    }

    fn is_kw(&self, kw: &str) -> bool {
        matches!(&self.peek().tok, Tok::Word(w) if w.eq_ignore_ascii_case(kw))
    }

    fn eat_kw(&mut self, kw: &str) -> bool {
        let yes = self.is_kw(kw);
        if yes {
            self.bump();
        }
        yes
    }

    fn expect_kw(&mut self, kw: &str) -> R<()> {
        if self.eat_kw(kw) {
            return Ok(());
        }
        self.err(format!("expected {kw}, found {}", self.found()))
    }

    fn is_sym(&self, sym: &str) -> bool {
        matches!(&self.peek().tok, Tok::Sym(s) if *s == sym)
    }

    fn eat_sym(&mut self, sym: &str) -> bool {
        let yes = self.is_sym(sym);
        if yes {
            self.bump();
        }
        yes
    }

    fn expect_sym(&mut self, sym: &str) -> R<()> {
        if self.eat_sym(sym) {
            return Ok(());
        }
        self.err(format!("expected `{sym}`, found {}", self.found()))
    }

    fn ident(&mut self, what: &str) -> R<String> {
        match &self.peek().tok {
            Tok::Word(w) => {
                let w = w.clone();
                self.bump();
                Ok(w)
            }
            _ => self.err(format!("expected {what}, found {}", self.found())),
        }
    }

    fn integer(&mut self, what: &str) -> R<usize> {
        match self.peek().tok {
            Tok::Int(v) if v > 0 => {
                self.bump();
                Ok(v as usize)
            }
            _ => self.err(format!("expected {what} (a positive whole number), found {}", self.found())),
        }
    }

    fn text(&mut self, what: &str) -> R<String> {
        match &self.peek().tok {
            Tok::Judgment(s) | Tok::Str(s) => {
                let s = s.clone();
                self.bump();
                Ok(s)
            }
            _ => self.err(format!("expected {what} in quotes, found {}", self.found())),
        }
    }

    /// `( a, b, c )` of identifiers.
    fn ident_list(&mut self, what: &str) -> R<Vec<String>> {
        self.expect_sym("(")?;
        let mut out = vec![self.ident(what)?];
        while self.eat_sym(",") {
            out.push(self.ident(what)?);
        }
        self.expect_sym(")")?;
        Ok(out)
    }

    fn using(&mut self) -> R<Option<Vec<String>>> {
        if !self.eat_kw("USING") {
            return Ok(None);
        }
        let mut cols = vec![self.ident("a column name")?];
        while self.eat_sym(",") {
            cols.push(self.ident("a column name")?);
        }
        Ok(Some(cols))
    }

    pub fn query(&mut self) -> R<Query> {
        self.expect_kw("FROM")?;
        let from = self.ident("a table name")?;
        let mut withs = Vec::new();
        while self.eat_kw("WITH") {
            withs.push(self.with()?);
        }
        let mut conditions = Vec::new();
        if self.eat_kw("FIND") {
            self.ident("what to find (e.g. `customers`)")?;
            self.expect_kw("WHO").or_else(|_| self.expect_kw("THAT")).or_else(|_| self.expect_kw("WHERE"))?;
            self.eat_sym(":");
            conditions = self.conditions()?;
        } else if self.eat_kw("WHERE") {
            conditions = self.conditions()?;
        }
        let mut judges = Vec::new();
        loop {
            if self.eat_kw("SCORE") {
                let name = self.ident("a name for the score")?;
                self.expect_sym(":")?;
                let question = self.text("the question")?;
                let levels = match self.eat_kw("LEVELS") {
                    true => Some(self.text_list("level descriptions")?),
                    false => None,
                };
                judges.push(Judge::Score { name, question, levels, using: self.using()? });
            } else if self.eat_kw("CLASSIFY") {
                let name = self.ident("a name for the label")?;
                self.expect_sym(":")?;
                let question = self.text("the question")?;
                self.expect_kw("INTO")?;
                let labels = self.text_or_ident_list()?;
                judges.push(Judge::Classify { name, question, labels, using: self.using()? });
            } else {
                break;
            }
        }
        let mut rank = None;
        if self.eat_kw("RANK") {
            self.expect_kw("BY")?;
            let keys = self.sort_keys()?;
            let limit = match self.eat_kw("LIMIT") {
                true => Some(self.integer("a row limit")?),
                false => None,
            };
            rank = Some(Rank { keys, limit });
        } else if self.is_kw("LIMIT") {
            return self.err("LIMIT needs an order: write `RANK BY <column> [DESC] LIMIT n`");
        }
        let mut returns = None;
        if self.eat_kw("RETURN") {
            let mut items = vec![self.return_item()?];
            while self.eat_sym(",") {
                items.push(self.return_item()?);
            }
            returns = Some(items);
        }
        self.eat_sym(";");
        if self.peek().tok != Tok::Eof {
            return self.err(format!(
                "unexpected {}; clauses go in the order FROM, WITH, FIND, SCORE/CLASSIFY, RANK BY, RETURN",
                self.found()
            ));
        }
        Ok(Query { from, withs, conditions, judges, rank, returns })
    }

    fn text_list(&mut self, what: &str) -> R<Vec<String>> {
        self.expect_sym("(")?;
        let mut out = vec![self.text(what)?];
        while self.eat_sym(",") {
            out.push(self.text(what)?);
        }
        self.expect_sym(")")?;
        Ok(out)
    }

    /// Labels may be bare words or quoted: `(billing, bug, "how-to")`.
    fn text_or_ident_list(&mut self) -> R<Vec<String>> {
        self.expect_sym("(")?;
        let mut out = Vec::new();
        loop {
            let label = match &self.peek().tok {
                Tok::Word(_) => self.ident("a label")?,
                _ => self.text("a label")?,
            };
            out.push(label);
            if !self.eat_sym(",") {
                break;
            }
        }
        self.expect_sym(")")?;
        Ok(out)
    }

    fn with(&mut self) -> R<With> {
        let table = self.ident("a table name")?;
        self.expect_kw("AS")?;
        let alias = self.ident("a name for the result")?;
        let on = match self.eat_kw("ON") {
            true => Some(self.ident("the shared key column")?),
            false => None,
        };
        self.expect_sym("(")?;
        let agg = match &self.peek().tok {
            Tok::Word(w) => match w.to_ascii_uppercase().as_str() {
                "COUNT" if matches!(self.peek_at(1), Tok::Word(d) if d.eq_ignore_ascii_case("DISTINCT")) => {
                    Some(AggFunc::CountDistinct)
                }
                "COUNT" => Some(AggFunc::Count),
                "SUM" => Some(AggFunc::Sum),
                "AVG" => Some(AggFunc::Avg),
                "MIN" => Some(AggFunc::Min),
                "MAX" => Some(AggFunc::Max),
                _ => None,
            },
            _ => None,
        };
        let kind = match agg {
            Some(func) => {
                self.bump();
                if func == AggFunc::CountDistinct {
                    self.bump();
                }
                let arg = match self.is_kw("WHERE") || self.is_sym(")") {
                    true => None,
                    false => Some(self.expr(1)?),
                };
                if arg.is_none() && func != AggFunc::Count {
                    return self.err("this aggregate needs a column, e.g. `SUM amount`");
                }
                let filter = match self.eat_kw("WHERE") {
                    true => Some(self.expr(1)?),
                    false => None,
                };
                WithKind::Aggregate { func, arg, filter }
            }
            None => self.history()?,
        };
        self.expect_sym(")")?;
        Ok(With { table, alias, on, kind })
    }

    fn history(&mut self) -> R<WithKind> {
        let (mut order, mut limit, mut fields, mut last) = (Vec::new(), None, None, false);
        loop {
            if self.eat_kw("LAST") {
                limit = Some(self.integer("how many rows")?);
                last = true;
            } else if self.eat_kw("FIRST") {
                limit = Some(self.integer("how many rows")?);
            } else if self.eat_kw("BY") {
                order = self.sort_keys()?;
            } else if self.eat_kw("FIELDS") {
                fields = Some(self.ident_list("a column name")?);
            } else if self.is_sym(")") {
                break;
            } else {
                return self.err(format!(
                    "expected LAST n, FIRST n, BY <column>, FIELDS (...) or an aggregate such as SUM <column>, found {}",
                    self.found()
                ));
            }
        }
        // LAST n BY x keeps the n newest by x (listed newest first)
        if last {
            for key in &mut order {
                key.descending = !key.descending;
            }
            if order.is_empty() {
                return self.err("LAST n needs an order, e.g. `LAST 30 BY created_at`");
            }
        }
        Ok(WithKind::History { order, limit, fields })
    }

    fn sort_keys(&mut self) -> R<Vec<SortKey>> {
        let mut keys = Vec::new();
        loop {
            let expr = self.expr(1)?;
            let descending = match () {
                _ if self.eat_kw("DESC") => true,
                _ => {
                    self.eat_kw("ASC");
                    false
                }
            };
            keys.push(SortKey { expr, descending });
            if !self.eat_sym(",") {
                return Ok(keys);
            }
        }
    }

    fn return_item(&mut self) -> R<ReturnItem> {
        let expr = self.expr(1)?;
        let alias = match self.eat_kw("AS") {
            true => Some(self.ident("a column name")?),
            false => None,
        };
        Ok(ReturnItem { expr, alias })
    }

    /// `atom AND atom AND ...`, each atom an expression or a quoted judgment.
    fn conditions(&mut self) -> R<Vec<Condition>> {
        let mut out = Vec::new();
        loop {
            match &self.peek().tok {
                Tok::Judgment(text) => {
                    let text = text.clone();
                    self.bump();
                    out.push(Condition::Judgment { text, using: self.using()? });
                }
                // stop before AND / OR so judgments and facts combine only by AND
                _ => out.push(Condition::Expr(self.expr(3)?)),
            }
            if self.is_kw("OR") {
                return self.err("combine conditions with OR inside parentheses, e.g. `(a = 1 OR b = 2)`; judgments can only be combined with AND");
            }
            if !self.eat_kw("AND") {
                return Ok(out);
            }
        }
    }

    /// Precedence climbing. Binding strength: OR 1, AND 2, NOT 3,
    /// comparison/IN/IS 4, + - 5, * / 6, unary minus 7.
    pub fn expr(&mut self, min: u8) -> R<Expr> {
        let mut left = self.prefix()?;
        loop {
            let (op, prec) = match &self.peek().tok {
                Tok::Word(w) if w.eq_ignore_ascii_case("OR") => (Some(BinaryOp::Or), 1),
                Tok::Word(w) if w.eq_ignore_ascii_case("AND") => (Some(BinaryOp::And), 2),
                Tok::Word(w) if w.eq_ignore_ascii_case("IN") || w.eq_ignore_ascii_case("IS") => (None, 4),
                Tok::Word(w) if w.eq_ignore_ascii_case("NOT") && matches!(self.peek_at(1), Tok::Word(x) if x.eq_ignore_ascii_case("IN")) => (None, 4),
                Tok::Sym(s) => match *s {
                    "=" => (Some(BinaryOp::Eq), 4),
                    "!=" | "<>" => (Some(BinaryOp::NotEq), 4),
                    "<" => (Some(BinaryOp::Lt), 4),
                    "<=" => (Some(BinaryOp::LtEq), 4),
                    ">" => (Some(BinaryOp::Gt), 4),
                    ">=" => (Some(BinaryOp::GtEq), 4),
                    "+" => (Some(BinaryOp::Add), 5),
                    "-" => (Some(BinaryOp::Sub), 5),
                    "*" => (Some(BinaryOp::Mul), 6),
                    "/" => (Some(BinaryOp::Div), 6),
                    _ => return Ok(left),
                },
                _ => return Ok(left),
            };
            if prec < min {
                return Ok(left);
            }
            match op {
                Some(op) => {
                    self.bump();
                    let right = self.expr(prec + 1)?;
                    left = Expr::binary(left, op, right);
                }
                None if self.eat_kw("IS") => {
                    let negated = self.eat_kw("NOT");
                    self.expect_kw("NULL")?;
                    left = Expr::IsNull { expr: Box::new(left), negated };
                }
                None => {
                    let negated = self.eat_kw("NOT");
                    self.expect_kw("IN")?;
                    self.expect_sym("(")?;
                    let mut list = vec![self.expr(1)?];
                    while self.eat_sym(",") {
                        list.push(self.expr(1)?);
                    }
                    self.expect_sym(")")?;
                    left = Expr::InList { expr: Box::new(left), list, negated };
                }
            }
        }
    }

    fn prefix(&mut self) -> R<Expr> {
        if self.eat_kw("NOT") {
            return Ok(Expr::Unary { op: UnaryOp::Not, expr: Box::new(self.expr(4)?) });
        }
        if self.eat_sym("-") {
            return Ok(Expr::Unary { op: UnaryOp::Negate, expr: Box::new(self.expr(7)?) });
        }
        let token = self.peek().clone();
        let fail = |m: String| NqlError::at(token.line, token.col, m);
        match token.tok {
            Tok::Int(v) => {
                self.bump();
                Ok(lit(Scalar::Int64(v)))
            }
            Tok::Float(v) => {
                self.bump();
                Ok(lit(Scalar::Float64(v)))
            }
            Tok::Str(s) => {
                self.bump();
                Ok(lit(Scalar::Utf8(s)))
            }
            Tok::Judgment(_) => {
                self.err("double-quoted judgments can only be FIND conditions; use 'single quotes' for text values")
            }
            Tok::Sym("(") => {
                self.bump();
                let e = self.expr(1)?;
                self.expect_sym(")")?;
                Ok(e)
            }
            Tok::Word(w) => {
                self.bump();
                let upper = w.to_ascii_uppercase();
                match upper.as_str() {
                    "TRUE" => return Ok(lit(Scalar::Boolean(true))),
                    "FALSE" => return Ok(lit(Scalar::Boolean(false))),
                    "NULL" => return Ok(lit(Scalar::Null)),
                    "DATE" | "INTERVAL" if matches!(self.peek().tok, Tok::Str(_)) => {
                        let text = self.text("a value")?;
                        let raw = json!({"value": text, "type": upper.to_ascii_lowercase()});
                        return serde_json::from_value::<Scalar>(raw).map(lit).map_err(|e| fail(e.to_string()));
                    }
                    _ => {}
                }
                if self.eat_sym("(") {
                    let name: Function = serde_json::from_value(json!(w.to_ascii_lowercase()))
                        .map_err(|_| fail(format!("unknown function `{w}`")))?;
                    let mut args = Vec::new();
                    if !self.eat_sym(")") {
                        args.push(self.expr(1)?);
                        while self.eat_sym(",") {
                            args.push(self.expr(1)?);
                        }
                        self.expect_sym(")")?;
                    }
                    return Ok(Expr::Function { name, args });
                }
                if CLAUSES.contains(&upper.as_str()) {
                    return Err(fail(format!("expected a value or column, found the keyword {upper}")));
                }
                Ok(col(w))
            }
            _ => self.err(format!("expected a value or column, found {}", self.found())),
        }
    }
}
