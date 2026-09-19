//! Natural language -> NQL, without a language model.
//!
//! Classic NLP over the actual data: the question is split into clauses;
//! words are linked to tables, columns and real column values; pattern rules
//! recognize quantities ("many orders", "more than 10 reviews"), rankings
//! ("top 20", "biggest spenders", "most reviews") and time windows ("this
//! year", "last 30 days"). Whatever a clause leaves unexplained becomes a
//! semantic judgment for Jev, read from the entity's related histories.
//!
//! The output is NQL text plus notes describing every interpretation, so the
//! user can see (and edit) exactly what the question was taken to mean.

use std::fmt;

/// What the translator knows about the data.
#[derive(Debug, Clone, Default)]
pub struct Vocabulary {
    pub tables: Vec<Table>,
    pub relations: Vec<Relation>,
}

#[derive(Debug, Clone)]
pub struct Table {
    pub name: String,
    pub columns: Vec<Column>,
}

#[derive(Debug, Clone)]
pub struct Column {
    pub name: String,
    pub kind: ColumnKind,
    /// Every distinct value, for low-cardinality text columns.
    pub values: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnKind {
    Text,
    Number,
    Date,
    Other,
}

/// `child` has many rows per `key` value of `parent`.
#[derive(Debug, Clone)]
pub struct Relation {
    pub parent: String,
    pub child: String,
    pub key: String,
    /// p25, p50, p75, p90 of child rows per parent key (keys with >= 1 row).
    pub count_percentiles: [f64; 4],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Translation {
    pub nql: String,
    /// How each part of the question was interpreted.
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NlError(pub String);

impl fmt::Display for NlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for NlError {}

/// Words that only glue a clause together.
const STOP: &[&str] = &[
    "a", "an", "the", "of", "to", "in", "on", "for", "by", "is", "are", "was", "were", "be", "been", "have", "has",
    "had", "having", "with", "who", "that", "which", "whose", "and", "their", "they", "them", "show", "me", "list",
    "find", "get", "give", "all", "any", "our", "we", "us", "please", "what", "whom", "from", "placed", "made",
    "make", "wrote", "written", "submitted", "opened", "filed", "bought", "left", "raised", "sent", "customers'",
    "there", "do", "does", "did", "one", "ones", "those", "these", "this", "year", "last", "days", "day", "weeks",
    "week", "months", "month", "number", "amount",
];
const CONNECTORS: &[&str] = &["who", "that", "which", "whose", "with", "and", "having", "from", "in", "where", ","];
const SPEND_WORDS: &[&str] = &["spend", "spent", "spending", "spender", "spenders", "revenue", "paying", "paid", "value", "valuable"];
const MONEY_COLUMNS: &[&str] = &["amount", "total", "price", "revenue", "value", "spend", "cost"];
const SUPERLATIVES: &[&str] = &["most", "biggest", "largest", "highest", "top", "best", "high"];
const FEWEST: &[&str] = &["fewest", "least", "lowest", "smallest"];
/// Verbs that open a judgment ("seems unhappy", "sounds like ...").
const CUE_VERBS: &[&str] = &["seem", "sound", "appear", "look", "feel", "complain", "mention", "talk", "want", "threaten", "sound"];

fn lemma(word: &str) -> String {
    let w = word.trim_end_matches("'s").trim_end_matches('\'');
    if w.len() > 4 && w.ends_with("ies") {
        format!("{}y", &w[..w.len() - 3])
    } else if w.len() > 3 && w.ends_with('s') && !w.ends_with("ss") && !w.ends_with("us") {
        w[..w.len() - 1].to_string()
    } else {
        w.to_string()
    }
}

#[derive(Debug, Clone)]
struct Tok {
    /// As written.
    text: String,
    norm: String,
    lemma: String,
}

fn tokenize(question: &str) -> Vec<Tok> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let flush = |cur: &mut String, out: &mut Vec<Tok>| {
        if !cur.is_empty() {
            let norm = cur.to_lowercase();
            out.push(Tok { text: cur.clone(), lemma: lemma(&norm), norm });
            cur.clear();
        }
    };
    for c in question.chars() {
        if c.is_alphanumeric() || c == '\'' || c == '’' || c == '-' {
            cur.push(if c == '’' { '\'' } else { c });
        } else {
            flush(&mut cur, &mut out);
            if c == ',' {
                out.push(Tok { text: ",".into(), norm: ",".into(), lemma: ",".into() });
            }
        }
    }
    flush(&mut cur, &mut out);
    out
}

fn number(word: &str) -> Option<i64> {
    const WORDS: &[(&str, i64)] = &[
        ("one", 1), ("two", 2), ("three", 3), ("four", 4), ("five", 5), ("six", 6), ("seven", 7), ("eight", 8),
        ("nine", 9), ("ten", 10), ("twenty", 20), ("fifty", 50), ("hundred", 100),
    ];
    word.parse().ok().or_else(|| WORDS.iter().find(|(w, _)| *w == word).map(|(_, n)| *n))
}

fn quote_text(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

fn quote_judgment(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// A related table's per-row aggregate, as an NQL WITH clause.
struct Aggregate {
    child: String,
    alias: String,
    /// `COUNT` or `SUM amount`.
    func: String,
    date_filter: Option<String>,
}

struct Plan<'v> {
    vocab: &'v Vocabulary,
    root: &'v Table,
    aggregates: Vec<Aggregate>,
    conditions: Vec<String>,
    judgments: Vec<String>,
    rank: Option<(String, bool)>,
    limit: Option<i64>,
    notes: Vec<String>,
}

pub fn translate(question: &str, vocab: &Vocabulary, today: (i32, u32, u32)) -> Result<Translation, NlError> {
    let toks = tokenize(question);
    let mentioned = toks.iter().find_map(|t| vocab.tables.iter().find(|tb| lemma(&tb.name) == t.lemma));
    // unnamed subject: the table most others relate to ("biggest spenders" -> customers)
    let main_entity = || {
        vocab
            .tables
            .iter()
            .filter(|t| vocab.relations.iter().any(|r| r.parent == t.name))
            .max_by_key(|t| vocab.relations.iter().filter(|r| r.parent == t.name).count())
    };
    let mentions_anything = toks.iter().any(|t| {
        SPEND_WORDS.contains(&t.norm.as_str())
            || vocab.tables.iter().any(|tb| tb.columns.iter().any(|c| c.values.iter().any(|v| v.to_lowercase() == t.norm)))
    });
    let root = match (mentioned, mentions_anything) {
        (Some(t), _) => t,
        (None, true) => main_entity().ok_or_else(|| NlError("which table is this about?".into()))?,
        (None, false) => {
            let names: Vec<&str> = vocab.tables.iter().map(|t| t.name.as_str()).collect();
            return Err(NlError(format!("which table is this about? Mention one of: {}", names.join(", "))));
        }
    };
    let mut plan = Plan {
        vocab,
        root,
        aggregates: Vec::new(),
        conditions: Vec::new(),
        judgments: Vec::new(),
        rank: None,
        limit: None,
        notes: Vec::new(),
    };

    if mentioned.is_none() {
        plan.notes.push(format!("no table named; assuming the question is about {}", root.name));
    }

    // clauses: split before connectors, keeping the connector with its clause
    let mut clauses: Vec<Vec<Tok>> = vec![Vec::new()];
    for t in toks {
        if CONNECTORS.contains(&t.norm.as_str()) && !clauses.last().expect("non-empty").is_empty() {
            clauses.push(Vec::new());
        }
        if t.norm != "," {
            clauses.last_mut().expect("non-empty").push(t);
        }
    }
    for clause in clauses.iter().filter(|c| !c.is_empty()) {
        plan.clause(clause, today);
    }
    Ok(plan.finish())
}

impl<'v> Plan<'v> {
    fn children(&self) -> impl Iterator<Item = &'v Relation> {
        let root = self.root.name.clone();
        self.vocab.relations.iter().filter(move |r| r.parent == root)
    }

    fn table(&self, name: &str) -> Option<&'v Table> {
        self.vocab.tables.iter().find(|t| t.name == name)
    }

    fn singular(&self, table: &str) -> String {
        lemma(table)
    }

    /// Interprets one clause; unexplained content becomes a judgment.
    fn clause(&mut self, clause: &[Tok], today: (i32, u32, u32)) {
        let mut used = vec![false; clause.len()];
        let norms: Vec<&str> = clause.iter().map(|t| t.norm.as_str()).collect();

        // top N / first N
        for i in 0..clause.len() {
            if matches!(norms[i], "top" | "first") && i + 1 < clause.len() {
                if let Some(n) = number(norms[i + 1]) {
                    self.limit = Some(n);
                    used[i] = true;
                    used[i + 1] = true;
                }
            }
        }
        // bare numbers before a noun: "20 customers", "the 10 biggest spenders"
        for i in 0..clause.len() {
            if !used[i] && self.limit.is_none() && number(norms[i]).is_some() && i + 1 < clause.len() {
                let next = &clause[i + 1];
                let is_noun = lemma(&self.root.name) == next.lemma || SUPERLATIVES.contains(&next.norm.as_str()) || SPEND_WORDS.contains(&next.norm.as_str());
                if is_noun && !self.children().any(|r| lemma(&r.child) == next.lemma) {
                    self.limit = number(norms[i]);
                    used[i] = true;
                }
            }
        }

        // the root table itself
        for (i, t) in clause.iter().enumerate() {
            if t.lemma == lemma(&self.root.name) {
                used[i] = true;
            }
        }

        // values of the root table's columns (up to 3 words), with negation
        let mut i = 0;
        while i < clause.len() {
            let mut matched = false;
            for len in (1..=3).rev() {
                if i + len > clause.len() || used[i..i + len].iter().any(|u| *u) {
                    continue;
                }
                let phrase = clause[i..i + len].iter().map(|t| t.norm.as_str()).collect::<Vec<_>>().join(" ");
                let (negated, phrase) = match phrase.strip_prefix("non-") {
                    Some(rest) => (true, rest.to_string()),
                    None => (false, phrase),
                };
                let hits: Vec<(&Column, &String)> = self
                    .root
                    .columns
                    .iter()
                    .flat_map(|c| c.values.iter().map(move |v| (c, v)))
                    .filter(|(_, v)| v.to_lowercase() == phrase)
                    .collect();
                if let Some((col, value)) = hits.first() {
                    let not = negated || (i > 0 && matches!(norms[i - 1], "not" | "non"));
                    if not && i > 0 && matches!(norms[i - 1], "not" | "non") {
                        used[i - 1] = true;
                    }
                    let op = if not { "!=" } else { "=" };
                    self.conditions.push(format!("{} {op} {}", col.name, quote_text(value)));
                    self.notes.push(format!("\"{phrase}\" → {}.{} {op} {}", self.root.name, col.name, quote_text(value)));
                    if hits.len() > 1 {
                        let others: Vec<String> = hits[1..].iter().map(|(c, _)| c.name.clone()).collect();
                        self.notes.push(format!("  (\"{phrase}\" also appears in {}; using {})", others.join(", "), col.name));
                    }
                    used[i..i + len].iter_mut().for_each(|u| *u = true);
                    i += len;
                    matched = true;
                    break;
                }
            }
            if !matched {
                i += 1;
            }
        }

        // spending: "biggest spenders", "spent the most", "high-value"
        if let Some(pos) = clause.iter().position(|t| {
            SPEND_WORDS.contains(&t.norm.as_str()) || t.norm == "high-value" || t.norm == "top-spending"
        }) {
            if let Some((rel, money)) = self.money_relation() {
                let alias = "spend".to_string();
                if !self.aggregates.iter().any(|a| a.alias == alias) {
                    self.aggregates.push(Aggregate { child: rel.child.clone(), alias: alias.clone(), func: format!("SUM {money}"), date_filter: None });
                }
                self.rank = Some((alias.clone(), true));
                self.limit.get_or_insert(20);
                self.notes.push(format!("\"{}\" → rank by total {}.{} (SUM), highest first", clause[pos].text, rel.child, money));
                used[pos] = true;
                for (j, n) in norms.iter().enumerate() {
                    if SUPERLATIVES.contains(n) || matches!(*n, "big" | "most" | "the") {
                        used[j] = true;
                    }
                }
            }
        }

        // quantities of related rows: "many orders", "more than 5 reviews", "no tickets", "most reviews"
        for j in 0..clause.len() {
            let Some(rel) = self.children().find(|r| lemma(&r.child) == clause[j].lemma) else { continue };
            if used[j] {
                continue;
            }
            used[j] = true;
            let alias = format!("{}_count", self.singular(&rel.child));
            let window: Vec<usize> = (j.saturating_sub(4)..j).filter(|&k| !used[k]).collect();
            let w: Vec<&str> = window.iter().map(|&k| norms[k]).collect();
            let has = |words: &[&str]| w.iter().any(|x| words.contains(x));
            let n = window.iter().rev().find_map(|&k| number(norms[k]));
            let [p25, _, p75, _] = rel.count_percentiles;
            // explicit numbers first: "at least 5" is not "the least"
            let (condition, note) = if let (Some(n), true) = (n, has(&["more", "over", "above", "exceeding"])) {
                (Some(format!("{alias} > {n}")), format!("more than {n} {}", rel.child))
            } else if let (Some(n), true) = (n, has(&["least", "minimum"])) {
                (Some(format!("{alias} >= {n}")), format!("at least {n} {}", rel.child))
            } else if let (Some(n), true) = (n, has(&["fewer", "less", "under", "below"])) {
                (Some(format!("({alias} IS NULL OR {alias} < {n})")), format!("fewer than {n} {}", rel.child))
            } else if has(&["most", "biggest", "highest"]) {
                self.rank = Some((alias.clone(), true));
                (None, format!("\"most {}\" → rank by {alias}, highest first", rel.child))
            } else if has(&FEWEST[..]) {
                self.rank = Some((alias.clone(), false));
                (None, format!("\"fewest {}\" → rank by {alias}, lowest first", rel.child))
            } else if has(&["no", "zero", "without", "never"]) {
                (Some(format!("{alias} IS NULL")), format!("\"no {}\" → none at all", rel.child))
            } else if has(&["many", "lot", "lots", "frequent", "frequently", "often", "active", "heavy"]) {
                let t = p75.ceil() as i64;
                (Some(format!("{alias} >= {t}")), format!("\"many {}\" → at least {t} (top 25% of {} by {} count)", rel.child, self.root.name, self.singular(&rel.child)))
            } else if has(&["few", "rarely", "occasional"]) {
                let t = p25.floor().max(1.0) as i64;
                (Some(format!("({alias} IS NULL OR {alias} <= {t})")), format!("\"few {}\" → at most {t} (bottom 25%)", rel.child))
            } else if let Some(n) = n {
                (Some(format!("{alias} >= {n}")), format!("{n} or more {}", rel.child))
            } else {
                (Some(format!("{alias} >= 1")), format!("\"with {}\" → at least one", rel.child))
            };
            for &k in &window {
                if number(norms[k]).is_some() || STOP.contains(&norms[k]) || [
                    "more", "than", "over", "above", "least", "at", "fewer", "less", "under", "below", "many", "lot",
                    "lots", "few", "no", "zero", "without", "never", "most", "fewest", "frequent", "frequently",
                    "often", "active", "heavy", "rarely", "occasional", "biggest", "highest", "minimum", "exceeding",
                ]
                .contains(&norms[k])
                {
                    used[k] = true;
                }
            }
            if !self.aggregates.iter().any(|a| a.alias == alias) {
                self.aggregates.push(Aggregate { child: rel.child.clone(), alias: alias.clone(), func: "COUNT".into(), date_filter: None });
            }
            if let Some(c) = condition {
                self.conditions.push(c);
            }
            self.notes.push(note);
        }

        // time windows apply to the latest related aggregate
        if let Some(filter) = time_window(&norms, &mut used, today) {
            let vocab = self.vocab;
            let date_col = |table: &str| {
                vocab.tables.iter().find(|t| t.name == table)?.columns.iter().find(|c| c.kind == ColumnKind::Date).map(|c| c.name.clone())
            };
            if let Some(agg) = self.aggregates.last_mut() {
                if let Some(col) = date_col(&agg.child) {
                    agg.date_filter = Some(filter.on(&col));
                    self.notes.push(format!("time window → {} counted only where {}", agg.child, filter.on(&col)));
                }
            } else if let Some(col) = date_col(&self.root.name) {
                self.conditions.push(filter.on(&col));
                self.notes.push(format!("time window → {}", filter.on(&col)));
            }
        }

        // whatever is left is a judgment
        let content: Vec<usize> = (0..clause.len())
            .filter(|&k| !used[k] && !STOP.contains(&norms[k]) && !CONNECTORS.contains(&norms[k]) && number(norms[k]).is_none())
            .collect();
        if content.is_empty() {
            return;
        }
        let start = clause.iter().position(|t| !CONNECTORS.contains(&t.norm.as_str())).unwrap_or(0);
        let words: Vec<&str> = clause[start..]
            .iter()
            .enumerate()
            .filter(|(k, _)| !used[start + k] || STOP.contains(&norms[start + k]))
            .map(|(_, t)| t.text.as_str())
            .collect();
        let judgment = self.phrase_judgment(&words);
        self.notes.push(format!("\"{}\" → judgment for Jev: {judgment}", words.join(" ")));
        self.judgments.push(judgment);
    }

    /// Turns a clause into a yes/no question about one root entity.
    fn phrase_judgment(&self, words: &[&str]) -> String {
        let entity = self.singular(&self.root.name);
        let about = if self.history_relations().is_empty() { "" } else { "Based on their history, " };
        let rest = words.join(" ");
        let first = words.first().map(|w| w.to_lowercase()).unwrap_or_default();
        let base = lemma(&first);
        if matches!(first.as_str(), "is" | "are" | "was" | "were") {
            let tail = words[1..].join(" ");
            format!("{about}is this {entity} {tail}?")
        } else if CUE_VERBS.contains(&base.as_str()) {
            let tail = words[1..].join(" ");
            format!("{about}does this {entity} {base} {tail}?").replace("  ", " ").replace(" ?", "?")
        } else {
            format!("{about}does this {entity} match the description: {rest}?")
        }
    }

    /// Related tables whose rows carry text a judgment can read.
    fn history_relations(&self) -> Vec<&'v Relation> {
        self.children()
            .filter(|r| {
                self.table(&r.child).is_some_and(|t| t.columns.iter().any(|c| c.kind == ColumnKind::Text && c.values.is_empty()))
            })
            .collect()
    }

    fn money_relation(&self) -> Option<(&'v Relation, String)> {
        self.children().find_map(|r| {
            let t = self.table(&r.child)?;
            let col = t.columns.iter().find(|c| c.kind == ColumnKind::Number && MONEY_COLUMNS.contains(&c.name.as_str()))?;
            Some((r, col.name.clone()))
        })
    }

    fn finish(mut self) -> Translation {
        let mut lines = vec![format!("FROM {}", self.root.name)];
        for a in &self.aggregates {
            let filter = a.date_filter.as_ref().map(|f| format!(" WHERE {f}")).unwrap_or_default();
            lines.push(format!("WITH {} AS {} ({}{filter})", a.child, a.alias, a.func));
        }
        if !self.judgments.is_empty() {
            for rel in self.history_relations() {
                let Some(t) = self.table(&rel.child) else { continue };
                let date = t.columns.iter().find(|c| c.kind == ColumnKind::Date).map(|c| c.name.clone());
                let fields: Vec<&str> = t
                    .columns
                    .iter()
                    .filter(|c| c.name != rel.key && !c.name.ends_with("_id") && c.kind != ColumnKind::Other)
                    .map(|c| c.name.as_str())
                    .collect();
                let order = date.as_ref().map(|d| format!("LAST 20 BY {d} ")).unwrap_or_default();
                let alias = format!("{}_history", self.singular(&rel.child));
                lines.push(format!("WITH {} AS {alias} ({order}FIELDS ({}))", rel.child, fields.join(", ")));
                self.notes.push(format!("judgments read {alias} (up to 20 most recent {})", rel.child));
            }
        }
        let mut conds: Vec<String> = self.conditions.clone();
        conds.extend(self.judgments.iter().map(|j| quote_judgment(j)));
        if !conds.is_empty() {
            lines.push(format!("FIND {} WHO: {}", self.root.name, conds.join("\n    AND ")));
        }
        let key = self.children().next().map(|r| r.key.clone()).or_else(|| self.root.columns.first().map(|c| c.name.clone()));
        let order = match (&self.rank, &key) {
            (Some((col, desc)), Some(k)) => Some(format!("{col}{} , {k}", if *desc { " DESC" } else { "" }).replace(" ,", ",")),
            (Some((col, desc)), None) => Some(format!("{col}{}", if *desc { " DESC" } else { "" })),
            (None, Some(k)) => Some(k.clone()),
            (None, None) => None,
        };
        if let Some(order) = order {
            let limit = self.limit.map(|n| format!(" LIMIT {n}")).unwrap_or_default();
            lines.push(format!("RANK BY {order}{limit}"));
        }
        Translation { nql: lines.join("\n"), notes: self.notes }
    }
}

/// A time window as bounds on a date column: `>= lo` and optionally `< hi`.
struct Window {
    lo: String,
    hi: Option<String>,
}

impl Window {
    fn on(&self, col: &str) -> String {
        match &self.hi {
            Some(hi) => format!("{col} >= {} AND {col} < {hi}", self.lo),
            None => format!("{col} >= {}", self.lo),
        }
    }
}

/// `this year`, `last 30 days`, `past 6 months`, `in 2025`.
fn time_window(norms: &[&str], used: &mut [bool], today: (i32, u32, u32)) -> Option<Window> {
    let (y, _, _) = today;
    for i in 0..norms.len() {
        let next = norms.get(i + 1).copied();
        if norms[i] == "this" && next == Some("year") {
            used[i..=i + 1].iter_mut().for_each(|u| *u = true);
            return Some(Window { lo: format!("DATE '{y}-01-01'"), hi: None });
        }
        if matches!(norms[i], "last" | "past") {
            if let (Some(n), Some(unit)) = (next.and_then(number), norms.get(i + 2)) {
                let unit = lemma(unit);
                if matches!(unit.as_str(), "day" | "week" | "month" | "year") {
                    used[i..=i + 2].iter_mut().for_each(|u| *u = true);
                    return Some(Window { lo: format!("current_date() - INTERVAL '{n} {unit}s'"), hi: None });
                }
            }
            if let Some(unit) = next.map(lemma).filter(|u| matches!(u.as_str(), "week" | "month" | "year")) {
                used[i..=i + 1].iter_mut().for_each(|u| *u = true);
                return Some(Window { lo: format!("current_date() - INTERVAL '1 {unit}'"), hi: None });
            }
        }
        if let Some(year) = next.filter(|_| norms[i] == "in").and_then(|w| w.parse::<i32>().ok()).filter(|y| (1900..2100).contains(y)) {
            used[i..=i + 1].iter_mut().for_each(|u| *u = true);
            return Some(Window { lo: format!("DATE '{year}-01-01'"), hi: Some(format!("DATE '{}-01-01'", year + 1)) });
        }
    }
    None
}
