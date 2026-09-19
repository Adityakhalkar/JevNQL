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
    /// For a child with a money column: that column and p25..p90 of its
    /// per-parent total.
    pub sum_percentiles: Option<(String, [f64; 4])>,
}

/// The column that holds money in a table (`amount`, `price`, ...), if any.
pub fn money_column(table: &Table) -> Option<&Column> {
    table.columns.iter().find(|c| c.kind == ColumnKind::Number && MONEY_COLUMNS.contains(&c.name.as_str()))
}

/// A phrase the rules could not settle, with candidate readings built from
/// the schema and data. A semantic model picks one; without one, the first
/// option is used.
#[derive(Debug, Clone)]
pub struct Decision {
    pub phrase: String,
    /// What to ask the model.
    pub question: String,
    pub options: Vec<DecisionOption>,
}

#[derive(Debug, Clone)]
pub struct DecisionOption {
    /// Short identifier (the model answers with it).
    pub label: String,
    /// What this reading means, in words.
    pub description: String,
    effect: Effect,
}

#[derive(Debug, Clone)]
enum Effect {
    /// Many related rows: `<alias> >= threshold` over COUNT.
    Count { child: String, threshold: i64 },
    /// High total of a money column: `spend >= threshold` over SUM.
    Sum { child: String, column: String, threshold: f64 },
    /// A judgment read from the given related tables (empty: the row's own text).
    Judgment { text: String, sources: Vec<String> },
}

/// The model's pick for one decision.
#[derive(Debug, Clone)]
pub struct Chosen {
    pub option: usize,
    pub confidence: Option<f64>,
    /// The next most likely option and its probability.
    pub runner_up: Option<(usize, f64)>,
}

/// A question analyzed up to its open decisions.
pub struct Analysis<'v> {
    plan: Plan<'v>,
    pub decisions: Vec<Decision>,
}

impl Analysis<'_> {
    /// Applies the chosen readings (missing entries use the first option).
    pub fn resolve(mut self, chosen: &[Chosen]) -> Translation {
        let decisions = std::mem::take(&mut self.decisions);
        for (i, d) in decisions.iter().enumerate() {
            let pick = chosen.get(i);
            let option = pick.map_or(0, |c| c.option.min(d.options.len() - 1));
            let tag = match pick.and_then(|c| c.confidence) {
                Some(conf) => {
                    let alt = pick
                        .and_then(|c| c.runner_up)
                        .map(|(j, p)| format!(", else {} {:.0}%", d.options[j].label, p * 100.0))
                        .unwrap_or_default();
                    format!("  [Jev {:.0}%{alt}]", conf * 100.0)
                }
                None => String::new(),
            };
            self.plan.apply(&d.phrase, &d.options[option], &tag);
        }
        self.plan.finish()
    }
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
    "week", "months", "month", "number", "amount", "app", "apps", "product", "service", "average", "avg", "on",
    "given", "gave", "give", "total", "overall", "$",
];
const CONNECTORS: &[&str] = &["who", "that", "which", "whose", "with", "and", "having", "from", "in", "where", ","];
const SPEND_WORDS: &[&str] = &["spend", "spent", "spending", "spender", "spenders", "revenue", "paying", "paid", "value", "valuable"];
const MONEY_COLUMNS: &[&str] = &["amount", "total", "price", "revenue", "value", "spend", "cost"];
const SUPERLATIVES: &[&str] = &["most", "biggest", "largest", "highest", "top", "best", "high"];
const FEWEST: &[&str] = &["fewest", "least", "lowest", "smallest", "worst"];
/// Words that refer to a rating column.
const RATING_WORDS: &[&str] = &["rating", "ratings", "rated", "rate", "star", "stars", "score"];
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
        while cur.ends_with('.') {
            cur.pop();
        }
        if !cur.is_empty() {
            let norm = cur.to_lowercase();
            out.push(Tok { text: cur.clone(), lemma: lemma(&norm), norm });
            cur.clear();
        }
    };
    for c in question.chars() {
        let decimal_point = c == '.' && cur.chars().last().is_some_and(|p| p.is_ascii_digit());
        if c.is_alphanumeric() || c == '\'' || c == '’' || c == '-' || decimal_point {
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
    question: String,
    aggregates: Vec<Aggregate>,
    conditions: Vec<String>,
    /// Judgment text and the related tables it reads (empty: the row's own text).
    judgments: Vec<(String, Vec<String>)>,
    decisions: Vec<Decision>,
    rank: Option<(String, bool)>,
    limit: Option<i64>,
    notes: Vec<String>,
    /// Comparisons that could not be linked to any column.
    unresolved: Vec<String>,
}

/// Translates with default readings (no semantic model involved).
pub fn translate(question: &str, vocab: &Vocabulary, today: (i32, u32, u32)) -> Result<Translation, NlError> {
    Ok(analyze(question, vocab, today)?.resolve(&[]))
}

/// Interprets everything the rules can settle; the rest becomes decisions.
pub fn analyze<'v>(question: &str, vocab: &'v Vocabulary, today: (i32, u32, u32)) -> Result<Analysis<'v>, NlError> {
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
        question: question.trim().to_string(),
        aggregates: Vec::new(),
        conditions: Vec::new(),
        judgments: Vec::new(),
        decisions: Vec::new(),
        rank: None,
        limit: None,
        notes: Vec::new(),
        unresolved: Vec::new(),
    };

    if mentioned.is_none() {
        plan.notes.push(format!("no table named; assuming the question is about {}", root.name));
    }

    // clauses: split before connectors, keeping the connector with its clause
    // "with" only opens a clause when a table or quantity follows ("with many
    // orders"), not inside a judgment ("happy with the product")
    let opens_clause = |i: usize| {
        let t = &toks[i];
        if !CONNECTORS.contains(&t.norm.as_str()) {
            return false;
        }
        if t.norm != "with" {
            return true;
        }
        toks[i + 1..toks.len().min(i + 5)].iter().any(|n| {
            vocab.tables.iter().any(|tb| lemma(&tb.name) == n.lemma)
                || number(&n.norm).is_some()
                || ["many", "few", "no", "most", "fewest", "more", "fewer", "least", "lots", "without"].contains(&n.norm.as_str())
        })
    };
    let mut clauses: Vec<Vec<Tok>> = vec![Vec::new()];
    for (i, t) in toks.iter().enumerate() {
        if opens_clause(i) && !clauses.last().expect("non-empty").is_empty() {
            clauses.push(Vec::new());
        }
        if t.norm != "," {
            clauses.last_mut().expect("non-empty").push(t.clone());
        }
    }
    for clause in clauses.iter().filter(|c| !c.is_empty()) {
        plan.clause(clause, today);
    }
    if let Some(phrase) = plan.unresolved.first() {
        return Err(NlError(format!(
            "couldn't tell which column \"{phrase}\" is about; name it (e.g. \"average rating above 3\", \"total spend over 20\") or write NQL"
        )));
    }
    let decisions = std::mem::take(&mut plan.decisions);
    Ok(Analysis { plan, decisions })
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
                    // the column's own name next to its value: "high priority", "status open"
                    for k in [i.checked_sub(1), Some(i + len)].into_iter().flatten() {
                        if clause.get(k).is_some_and(|t| t.lemma == lemma(&col.name)) {
                            used[k] = true;
                        }
                    }
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
        let ranks_or_compares = norms.iter().any(|n| SUPERLATIVES.contains(n) || matches!(*n, "big" | "heavy" | "high-value" | "top-spending"))
            || comparison(&norms, &mut used.clone()).is_some();
        if let Some(pos) = clause.iter().position(|t| {
            ranks_or_compares && (SPEND_WORDS.contains(&t.norm.as_str()) || t.norm == "high-value" || t.norm == "top-spending")
        }) {
            if let Some((rel, money)) = self.money_relation() {
                let alias = "spend".to_string();
                if !self.aggregates.iter().any(|a| a.alias == alias) {
                    self.aggregates.push(Aggregate { child: rel.child.clone(), alias: alias.clone(), func: format!("SUM {money}"), date_filter: None });
                }
                used[pos] = true;
                match comparison(&norms, &mut used) {
                    // "paid more than $20": a filter, not a ranking
                    Some((op, n, phrase)) => {
                        self.conditions.push(format!("{alias} {op} {n}"));
                        self.notes.push(format!("\"{} {phrase}\" → total {}.{} {op} {n}", clause[pos].text, rel.child, money));
                    }
                    None => {
                        self.rank = Some((alias.clone(), true));
                        self.limit.get_or_insert(20);
                        self.notes.push(format!("\"{}\" → rank by total {}.{} (SUM), highest first", clause[pos].text, rel.child, money));
                    }
                }
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

        // numeric columns: "rated more than 3 stars", "highest rated"
        if let Some(k) = (0..clause.len()).find(|&k| !used[k] && self.numeric_column(&clause[k].lemma).is_some()) {
            let (table, column) = self.numeric_column(&clause[k].lemma).expect("found");
            used[k] = true;
            let (expr, what) = match table == self.root.name {
                true => (column.clone(), format!("{table}.{column}")),
                false => {
                    let alias = format!("avg_{column}");
                    if !self.aggregates.iter().any(|a| a.alias == alias) {
                        self.aggregates.push(Aggregate { child: table.clone(), alias: alias.clone(), func: format!("AVG {column}"), date_filter: None });
                    }
                    (alias, format!("average {column} across their {table}"))
                }
            };
            if let Some((op, n, phrase)) = comparison(&norms, &mut used) {
                self.conditions.push(format!("{expr} {op} {n}"));
                self.notes.push(format!("\"{} {phrase}\" → {what} {op} {n}", clause[k].text));
            } else if let Some(j) = (0..clause.len()).find(|&j| !used[j] && (SUPERLATIVES.contains(&norms[j]) || FEWEST.contains(&norms[j]))) {
                let desc = !FEWEST.contains(&norms[j]);
                used[j] = true;
                self.rank = Some((expr, desc));
                self.limit.get_or_insert(20);
                self.notes.push(format!("\"{} {}\" → rank by {what}, {}", norms[j], clause[k].text, if desc { "highest first" } else { "lowest first" }));
            }
            for (j, n) in norms.iter().enumerate() {
                if RATING_WORDS.contains(n) {
                    used[j] = true;
                }
            }
        }

        // a leftover number with a comparison is a fact we could not place:
        // never send it to Jev as a judgment
        if let Some((_, _, phrase)) = comparison(&norms, &mut used.clone()) {
            self.unresolved.push(phrase);
            return;
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
        self.decide(words.join(" "), self.phrase_judgment(&words));
    }

    /// Candidate readings for an unexplained phrase: a judgment over each
    /// plausible evidence source, and facts computable from related tables.
    fn decide(&mut self, phrase: String, judgment: String) {
        let entity = self.singular(&self.root.name);
        let sources: Vec<String> = self.history_relations().iter().map(|r| r.child.clone()).collect();
        let mut options = Vec::new();
        let judge = |label: String, description: String, sources: Vec<String>| DecisionOption {
            label,
            description,
            effect: Effect::Judgment { text: judgment.clone(), sources },
        };
        match sources.as_slice() {
            [] => options.push(judge("judge_text".into(), format!("a judgment about meaning, read from the {entity}'s own text"), vec![])),
            [one] => options.push(judge(format!("judge_{one}"), format!("a judgment about meaning, read from each {entity}'s {one}"), sources.clone())),
            many => {
                options.push(judge(
                    "judge_all".into(),
                    format!("a judgment about meaning, read from each {entity}'s {}", many.join(" and ")),
                    sources.clone(),
                ));
                for s in many {
                    options.push(judge(format!("judge_{s}"), format!("a judgment about meaning, read only from each {entity}'s {s}"), vec![s.clone()]));
                }
            }
        }
        for rel in self.children() {
            let t = rel.count_percentiles[2].ceil() as i64;
            options.push(DecisionOption {
                label: format!("many_{}", rel.child),
                description: format!(
                    "a fact from the {} table: {} with many {} (at least {t}, the top 25% by number of {})",
                    rel.child, self.root.name, rel.child, rel.child
                ),
                effect: Effect::Count { child: rel.child.clone(), threshold: t },
            });
            if let Some((column, p)) = &rel.sum_percentiles {
                options.push(DecisionOption {
                    label: format!("high_total_{}", column),
                    description: format!(
                        "a fact from the {} table: {} whose total {} is high (at least {:.2}, the top 25%)",
                        rel.child, self.root.name, column, p[2]
                    ),
                    effect: Effect::Sum { child: rel.child.clone(), column: column.clone(), threshold: p[2] },
                });
            }
        }
        let decision = Decision {
            question: format!(
                "In the data question \"{}\", what does the phrase \"{phrase}\" ask for? Facts are exact numbers in the data; judgments need reading text.",
                self.question
            ),
            phrase,
            options,
        };
        match decision.options.len() {
            1 => self.apply(&decision.phrase, &decision.options[0], ""),
            _ => self.decisions.push(decision),
        }
    }

    /// Applies one reading of a phrase to the plan.
    fn apply(&mut self, phrase: &str, option: &DecisionOption, tag: &str) {
        match &option.effect {
            Effect::Count { child, threshold } => {
                let alias = format!("{}_count", self.singular(child));
                if !self.aggregates.iter().any(|a| a.alias == alias) {
                    self.aggregates.push(Aggregate { child: child.clone(), alias: alias.clone(), func: "COUNT".into(), date_filter: None });
                }
                self.conditions.push(format!("{alias} >= {threshold}"));
                self.notes.push(format!("\"{phrase}\" → at least {threshold} {child} (top 25%){tag}"));
            }
            Effect::Sum { child, column, threshold } => {
                let alias = "spend".to_string();
                if !self.aggregates.iter().any(|a| a.alias == alias) {
                    self.aggregates.push(Aggregate { child: child.clone(), alias: alias.clone(), func: format!("SUM {column}"), date_filter: None });
                }
                self.conditions.push(format!("{alias} >= {threshold:.2}"));
                self.notes.push(format!("\"{phrase}\" → total {child}.{column} ≥ {threshold:.2} (top 25%){tag}"));
            }
            Effect::Judgment { text, sources } => {
                let from = match sources.as_slice() {
                    [] => String::new(),
                    s => format!(" (reads {})", s.join(" + ")),
                };
                self.notes.push(format!("\"{phrase}\" → judgment for Jev: {text}{from}{tag}"));
                self.judgments.push((text.clone(), sources.clone()));
            }
        }
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

    /// A numeric column of the root table or a child table named by `word`.
    fn numeric_column(&self, word: &str) -> Option<(String, String)> {
        let matches = |c: &Column| {
            c.kind == ColumnKind::Number
                && !c.name.ends_with("_id")
                && (lemma(&c.name) == word || (c.name.contains("rating") && RATING_WORDS.contains(&word)))
        };
        let tables = std::iter::once(self.root.name.clone()).chain(self.children().map(|r| r.child.clone()));
        for table in tables {
            if let Some(c) = self.table(&table).and_then(|t| t.columns.iter().find(|c| matches(c))) {
                return Some((table, c.name.clone()));
            }
        }
        None
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
        let used_sources: Vec<String> = self.judgments.iter().flat_map(|(_, s)| s.clone()).collect();
        if !self.judgments.is_empty() {
            for rel in self.history_relations().into_iter().filter(|r| used_sources.contains(&r.child)) {
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
                self.notes.push(format!("{alias}: up to 20 most recent {} per {}", rel.child, self.singular(&self.root.name)));
            }
        }
        let mut conds: Vec<String> = self.conditions.clone();
        let own_text: Vec<String> =
            self.root.columns.iter().filter(|c| c.kind == ColumnKind::Text).map(|c| c.name.clone()).collect();
        for (text, sources) in &self.judgments {
            let using: Vec<String> = match sources.is_empty() {
                true => own_text.clone(),
                false => sources.iter().map(|s| format!("{}_history", self.singular(s))).collect(),
            };
            conds.push(match using.is_empty() {
                true => quote_judgment(text),
                false => format!("{} USING {}", quote_judgment(text), using.join(", ")),
            });
        }
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

/// "more than 20", "at least 3.5", "under 10", "20 or more", "exactly 5":
/// the operator, the number, and the phrase; marks its words as used.
fn comparison(norms: &[&str], used: &mut [bool]) -> Option<(&'static str, String, String)> {
    let num = |w: &str| w.trim_start_matches('$').parse::<f64>().ok().map(|_| w.trim_start_matches('$').to_string()).or_else(|| number(w).map(|n| n.to_string()));
    for i in 0..norms.len() {
        if used[i] {
            continue;
        }
        let at = |k: usize| norms.get(k).copied().unwrap_or("");
        let (op, words) = match (at(i), at(i + 1)) {
            ("more" | "greater" | "higher", "than") => (">", 2),
            ("over" | "above" | "exceeding", _) => (">", 1),
            ("at", "least") => (">=", 2),
            ("minimum" | "min", _) => (">=", 1),
            ("less" | "fewer" | "lower", "than") => ("<", 2),
            ("under" | "below", _) => ("<", 1),
            ("at", "most") => ("<=", 2),
            ("maximum" | "max", _) => ("<=", 1),
            ("exactly", _) => ("=", 1),
            _ => match (num(at(i)), at(i + 1), at(i + 2)) {
                (Some(n), "or", "more") => {
                    used[i..=i + 2].iter_mut().for_each(|u| *u = true);
                    return Some((">=", n.clone(), format!("{n} or more")));
                }
                (Some(n), "or", "less" | "fewer") => {
                    used[i..=i + 2].iter_mut().for_each(|u| *u = true);
                    return Some(("<=", n.clone(), format!("{n} or less")));
                }
                _ => continue,
            },
        };
        if let Some(n) = num(at(i + words)) {
            let phrase = norms[i..=i + words].join(" ");
            used[i..=i + words].iter_mut().for_each(|u| *u = true);
            return Some((op, n, phrase));
        }
    }
    None
}
