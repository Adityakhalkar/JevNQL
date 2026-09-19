# JevNQL

**JevNQL compiles data questions into optimized deterministic and semantic query plans.**

<p align="center">
  <a href="docs/demo.mp4"><img src="docs/demo.gif" alt="JevNQL answering a plain-English question: interpretation, cost confirmation, live Jev progress and results" width="900"></a>
  <br><sub>Click for the full-quality video.</sub>
</p>

Query languages operate on *values*: `WHERE revenue > 10000`. Real questions often operate on *meaning*:

> Find the customers who spent the most this year and seem increasingly unhappy with our pricing.

"Spent the most this year" is arithmetic. "Seem increasingly unhappy" is judgment. JevNQL expresses both in one query language, NQL. It compiles the query into one typed plan (JevIR), sends the arithmetic to a relational engine ([Apache DataFusion](https://datafusion.apache.org)), sends only the judgments to a semantic model ([Jev](https://docs.typesafe.ai)), and orders the work so the expensive part sees as few rows as possible.

Jev does two jobs: it **decides what a question means** (choosing between readings built from your data) and it **judges meaning in the data** (over the rows the optimizer narrows down). No generative LLM is involved.

```text
plain English ──► jev-nl (NLP candidates) ──► Jev decides ──► NQL
NQL ──► jev-nql (parser) ──► logical JevIR ──► optimizer ──► physical plan
                                                                  │
                                                      ┌───────────┴───────────┐
                                                      ▼                       ▼
                                               DataFusionExec            JevBatchExec
                                          scan/filter/join/agg/top-k   filter/score/classify
                                                      └───────────┬───────────┘
                                                                  ▼
                                                        Arrow results + metrics
```

## What it looks like

```sql
FROM customers
WITH orders  AS spend   (SUM amount WHERE order_date >= DATE '2026-01-01')
WITH reviews AS history (LAST 30 BY created_at FIELDS (created_at, rating, text))
SCORE dissatisfaction: "Across this customer's reviews, how increasingly dissatisfied do they appear with our pricing?"
    LEVELS ("No pricing complaints, or satisfaction with pricing is stable or improving",
            "Occasional pricing complaints with no clear worsening trend",
            "Pricing complaints that grow more frequent or more severe over time")
RANK BY spend DESC, customer_id LIMIT 20
RETURN customer_id, name, spend, dissatisfaction
```

Over **2,000,000 orders**, with live Jev:

```text
OPTIMIZER

  - semantic late execution: SemanticScore `dissatisfaction` after TopK[20]
  - semantic late execution: Fetch `history` after TopK[20]
  - projection pushdown: scan orders reads 3 of 4 columns
  …

PHYSICAL PLAN

DataFusionExec
└── Project[customer_id, name, spend, dissatisfaction]
    └── JevBatchExec[context: history | concurrency 16 | cache]
        │   score dissatisfaction: "Across this customer's reviews, how increasingly dissatisfied …"
        └── DataFusionExec
            └── Fetch[history <- customer_id.customer_id: created_at, rating, text | order created_at DESC | limit 30]
                ├── TopK[20: spend DESC, customer_id]
                │   └── Join[left: customer_id = customer_id]
                │       ├── Scan[customers: customer_id, name]
                │       └── Aggregate[by customer_id | SUM(amount) AS spend]
                │           └── Filter[order_date >= DATE '2026-01-01']
                │               └── Scan[orders: customer_id, amount, order_date]
                └── Scan[reviews: customer_id, rating, text, created_at]

METRICS

  Engine                      DataFusion → Jev → DataFusion
  Semantic backend            typesafe:jev-latest
  Rows scanned                2,098,210
  Rows to semantic operators  20
  Distinct states judged      11
  Execution time              1537.5 ms (semantic 1296.5 ms)
  Estimated semantic cost     $0.000296
```

The query is written in reading order: fetch every history, score everyone, then rank. The optimizer moved the fetch and the Jev score above the top-20 cut, so two million rows go through DataFusion and 20 reach Jev.

## Quickstart

Requirements: [Rust](https://rustup.rs) ≥ 1.88. Python 3 (standard library only) is used by the data generator and the benchmark script.

```bash
git clone https://github.com/Adityakhalkar/JevNQL && cd JevNQL
cargo build --release -p jevnql-cli             # first build takes a few minutes
python3 examples/generate.py                    # synthetic data -> examples/data/demo/
target/release/jevnql examples/data/demo/*.csv  # the shell
```

To install just the `jevnql` command for your own CSV or Parquet files:

```bash
cargo install --git https://github.com/Adityakhalkar/JevNQL jevnql-cli
jevnql path/to/*.csv
```

```text
JevNQL > FROM customers
       … WITH reviews AS history (LAST 30 BY created_at)
       … FIND customers WHO: "seem unhappy with our pricing"
       … RANK BY customer_id LIMIT 20;
```

Prefix a query with `EXPLAIN` to see the plans without running them. `\tables` lists tables, `\ir` prints the last compiled JevIR plan, and `\q` quits.

Semantic judgments use **Jev** when `TYPESAFE_API_KEY` is set. Otherwise they use an offline **simulator**, a keyword heuristic that is clearly labelled in every metrics block. Force either with `--backend jev|simulated`.

```bash
examples/demo.sh                                # the three-step demo (examples/queries/*.nql)
jevnql query --file q.nql data/*.csv            # one query; add --explain or --emit-ir
jevnql run --plan plan.json data/*.csv          # a JevIR plan document directly
```

## Plain English

Type questions as you would ask a colleague:

```text
❯ enterprise customers with many orders who sound like they're leaving
● Understood
  enterprise                  customers.segment = 'enterprise'
  many orders                 at least 43 (top 25% of customers by order count)
  sound like they're leaving  Jev judges: does this customer sound like they're leaving? (reads reviews + tickets)
```

- **Classic NLP proposes.** Words are linked to tables, columns and real values in your data ("enterprise" is a value of `segment`). Pattern rules handle quantities, comparisons, rankings and time windows. "many" means the top 25% of *your* data.
- **Jev decides.** Phrases the rules can't settle become decisions with candidate readings: a fact computable from a related table (order count, total spend) or a judgment read from a chosen source (reviews, tickets, or both). Jev picks one, and all of a question's decisions go in **one request**. For example, "frequent spenders" becomes `at least 43 orders` (Jev 51%, runner-up total spend 39%). That's 1 request instead of judging 5,000 customers.
- **Numbers never go to Jev as judgments.** "paid more than $20" is a filter on total spend; an unplaceable comparison is an error, not a guess.
- **You always see the reading** (and the generated NQL with `\verbose`). Anything starting with `FROM` is NQL, run as written.

## Token efficiency

Jev bills input tokens, so JevNQL minimizes tokens × judgments:

- **Fewer rows:** deterministic filters and top-k run before semantic work (predicate pushdown, late execution).
- **Fewer requests:** identical states are judged once, answers are cached for the session, and questions over the same state share one request.
- **Smaller states:** only the context columns are sent. Histories are capped (`LAST n`) and encoded as compact tables (field names once, one line per record), which measured about 33% fewer tokens than JSON records. Jev chooses the evidence, so a judgment reads only the sources it needs (reviews-only states measured about 48% smaller than reviews + tickets).
- **No surprises:** a live job over 1,000 requests shows its estimated tokens, cost and time and waits for `y`. No answer cancels.

## NQL

```text
FROM <table>
[WITH <table> AS <name> [ON <key>] (<aggregate> [<column>] [WHERE <condition>])]   per-row aggregate, joined
[WITH <table> AS <name> [ON <key>] ([LAST|FIRST n] [BY <column> [DESC]] [FIELDS (...)])]  per-row history, fetched
[FIND <things> WHO: <condition> AND "<judgment>" [USING <columns>] AND ...]
[SCORE <name>: "<question>" [LEVELS ("lowest", ..., "highest")] [USING <columns>]]
[CLASSIFY <name>: "<question>" INTO (label, ...) [USING <columns>]]
[RANK BY <expr> [DESC], ... [LIMIT n]]
[RETURN <expr> [AS <name>], ...]
```

- `'single quotes'` are text values. `"Double quotes"` are **judgments** evaluated by Jev. Everything else is a deterministic expression: arithmetic, comparisons, `IN`, `IS NULL`, `DATE '…'`, `INTERVAL '30 days'`, and functions such as `lower`, `contains`, `year` and `current_date`.
- Conditions combine with `AND`. Use parentheses for `OR` between deterministic conditions; a judgment can't be `OR`ed with a fact.
- `WITH` joins on the one column both tables share, or the one shared `*_id` column, or `ON <key>`.
- Judgments read the fetched histories by default, or the table's text columns if there are none. `USING` overrides this.
- Errors point at the query: `line 1, column 29: double-quoted judgments can only be FIND conditions`, or `` step `return`: unknown column `nmae`; available: … ``.

## JevIR

NQL compiles to JevIR, a typed plan document. Tools and future frontends can produce it directly.

```json
{"id": "score_dissatisfaction", "op": "semantic_score", "input": "with_history",
 "context": ["history"],
 "question": "How increasingly dissatisfied with our pricing does this customer appear?",
 "levels": ["No pricing complaints", "Occasional complaints", "Complaints growing over time"],
 "output": "dissatisfaction"}
```

| Relational | Semantic |
|---|---|
| `scan` `filter` `project` `join` `aggregate` `sort` `top_k` | `semantic_filter`: keep rows where a statement holds |
| `fetch`: attach each row's related rows as a list | `semantic_score`: a 0–1 position on ordered levels |
| | `semantic_choice`: one label out of a set |

Expressions are a typed AST, not SQL text. Every step is type-checked before anything runs.

## Optimizer

The rules are semantics-preserving. After rewriting, the plan is re-type-checked and its output schema must be unchanged.

- **Predicate pushdown.** Deterministic filters move below semantic operators, Fetch, projections, joins and aggregates.
- **Semantic late execution.** Score, classify and Fetch move above a TopK that doesn't sort by their output. SemanticFilter never moves past a TopK, because that would change which rows are kept.
- **Projection pushdown.** Scans read only the columns that are needed. Semantic operators whose output nobody reads are removed.
- **Batching.** Adjacent semantic operators over the same context share one request per row, but never past a SemanticFilter, where the extra question would be asked for rows that get dropped.

`JevBatchExec` deduplicates identical states, caches answers for the whole session, sends requests concurrently, and refuses to send more than `--max-semantic-rows` rows to one operator (default 10,000).

## Benchmarks

`python3 benchmarks/run.py [--backend jev] [--data examples/data/large]` runs each question twice, in fresh engines: once as written, in natural reading order ("naive"), and once optimized.

**Live Jev, 5,000 customers and 200,000 orders** ([report](benchmarks/results-demo-jev.md)):

| Question | Rows → Jev | Time | Cost | Accuracy vs planted truth |
|---|---|---|---|---|
| Customers with ≥ 8 reviews unhappy about pricing | 3,345 → 730 | 91 s → 21 s | $0.077 → $0.031 | 100% precision, 100% recall (93/93) |
| Pricing unhappiness of the 100 biggest spenders | 4,653 → 100 | 84 s → 2.8 s | $0.074 → $0.0015 | 100% precision |
| Most urgent open enterprise tickets | 10,730 → 420 | 17 s → 4.9 s | $0.0094 → $0.0018 | n/a |

The optimizer's rewrites are verified with the deterministic simulator: naive and optimized results are identical at both scales ([2M-order report](benchmarks/results-large.md): 4.4×, 197× and 25× fewer rows to the semantic backend). **Live Jev is slightly nondeterministic.** The same input returned 0.80, 0.795 and 0.81 on three calls, so results across two separate runs can differ a little. Case 2 differs by at most 0.015. In case 3, 6 of the top 25 tickets change, because many tickets tie on urgency. Within one session, the answer cache keeps judgments consistent.

The data comes from `examples/generate.py`: synthetic customers, orders, reviews and tickets, with hidden personas used as ground truth. These are clean, synthetic signals, so accuracy on real data needs its own evaluation.

## Status

Working and tested (67 Rust tests):
- Plain English (NLP + Jev decisions), NQL, JevIR, the optimizer, and DataFusion execution on CSV and Parquet.
- The interactive shell.
- Live Jev, verified against the TypeSafe API.

Known limits:
- **Two-stage rankings don't fit in NQL yet.** "Among the top 500 spenders, the 20 most unhappy" needs a way to rank twice; JevIR can express it (`examples/plans/high_value_unhappy.json`).
- **Missed rewrite:** a Fetch that only a later operator needs isn't yet moved past a SemanticFilter.
- **Joins:** semantic operators aren't moved across joins, which needs cardinality estimates.
- **Floats:** sums may differ in the last bits between plans (parallel aggregation order).
- **Cache:** in-memory, per session.

## Repository

```text
crates/
  jevir/          logical + physical IR, types, expressions, JSON, validation, EXPLAIN
  jev-nl/         plain English → NQL: NLP candidates and decisions for Jev
  jev-nql/        NQL lexer, parser and compiler to JevIR
  jev-optimizer/  rewrite rules, logical → physical planning, batch fusion
  jev-executor/   DataFusion lowering, JevBatchExec runtime, metrics, table profiles
  jev-provider/   SemanticBackend trait; TypeSafe Jev, simulated and mock backends
  jevnql-core/    engine API (open, prepare, execute)
  jevnql-cli/     `jevnql`: shell, query, run, explain, catalog, validate, serve
examples/         data generator, NQL queries, JevIR plans, demo.sh
benchmarks/       naive vs optimized suite and reports
```

## Roadmap

- More Jev decisions: aggregate choice ("rated above 3": average, any or every review), ambiguous values, thresholds; switch to the runner-up reading with one key.
- Two-stage rankings in NQL; late Fetch past SemanticFilter.
- Retrieval (full-text search, embeddings) as an opt-in candidate pre-filter with stated recall.
- Cost-based semantic planning, a persistent semantic cache, semantic indexes.
