# JevNQL

**JevNQL compiles natural-language questions into optimized deterministic and semantic query plans.**

Query languages operate on *values*: `WHERE revenue > 10000`. Real questions often operate on *meaning*:

> Find the customers who spent the most this year and seem increasingly unhappy with our pricing.

"Spent the most this year" is arithmetic. "Seem increasingly unhappy" is judgment. JevNQL compiles the whole question into one typed plan, sends the arithmetic to a relational engine ([Apache DataFusion](https://datafusion.apache.org)), sends only the judgment to a semantic model ([Jev](https://docs.typesafe.ai)), and orders the work so the expensive part sees as few rows as possible.

It is not English-to-SQL. Natural language compiles to **JevIR**, a typed intermediate representation. SQL never appears, and every backend consumes JevIR.

```text
Natural language ──► NL compiler (Python + Claude)
                          │  logical JevIR (JSON), type-checked by the core
                          ▼
                     Rust core: validate ─► optimize ─► physical plan
                          │
              ┌───────────┴───────────┐
              ▼                       ▼
      DataFusionExec             JevBatchExec
  scan/filter/join/agg/top-k   filter/score/choose by meaning
              └───────────┬───────────┘
                          ▼
                    Arrow results + metrics
```

## What it looks like

The question above, over **2,000,000 orders** ([`examples/plans/high_value_unhappy.json`](examples/plans/high_value_unhappy.json)):

```text
PHYSICAL PLAN

DataFusionExec
└── TopK[20: dissatisfaction DESC]
    └── JevBatchExec[context: review_history | concurrency 16 | cache]
        │   score dissatisfaction: "Across this customer's reviews in chronological order,
        │                           how increasingly dissatisfied do they appear with our pricing?"
        └── DataFusionExec
            └── Fetch[review_history <- customer_id.customer_id: created_at, rating, text | …]
                ├── TopK[500: total_spend DESC]
                │   └── Aggregate[by customer_id | SUM(amount) AS total_spend]
                │       └── Filter[order_date >= DATE '2026-01-01']
                │           └── Scan[orders: customer_id, amount, order_date]
                └── Scan[reviews: customer_id, rating, text, created_at]

METRICS

  Engine                      DataFusion → Jev(simulated) → DataFusion
  Rows scanned                2,078,210
  Rows to semantic operators  500
  Distinct states judged      361
  Jev batches / requests      1 / 361
  Input tokens                81,591
  Execution time              268.5 ms (semantic 19.0 ms)
  Estimated semantic cost     $0.003427
```

Two million rows go through DataFusion. The top 500 spenders reach the semantic operator. Customers without reviews all share one empty state, so 361 distinct judgments are made.

## Quickstart

Requirements: Rust ≥ 1.88, Python ≥ 3.10, [uv](https://docs.astral.sh/uv/).

```bash
cargo build --release -p jevnql-cli      # the engine (first build takes a few minutes)
(cd python && uv sync)                   # the `jevnql` shell and NL compiler
python3 examples/generate.py             # synthetic data -> examples/data/demo/
```

**No API keys needed.** Run JevIR plans with the offline simulated backend:

```bash
examples/demo.sh                         # the three-step demo below
cd python && uv run jevnql ../examples/data/demo/*.csv --plan ../examples/plans/active_unhappy_pricing.json
```

**Ask in natural language.** The compiler uses Claude:

```bash
export ANTHROPIC_API_KEY=...             # or `ant auth login`
cd python && uv run jevnql ../examples/data/demo/*.csv
```
```text
JevNQL > Which customer has submitted the most reviews?
JevNQL > Which of our most active customers seem unhappy with pricing?
JevNQL > Rank them by how likely they seem to leave.
JevNQL > EXPLAIN Which enterprise tickets sound urgent?
```

**Use Jev for semantic judgments.** Without a key, JevNQL falls back to the simulator and labels it in every metrics block:

```bash
export TYPESAFE_API_KEY=...              # --backend auto (default) then uses Jev
```

## The demo

`examples/demo.sh` runs the three questions from the spec as JevIR plans:

| Question | Engines | Rows to semantic operators |
|---|---|---|
| Which customers have submitted the most reviews? | DataFusion | 0 |
| Which of our most active customers seem unhappy with pricing? | DataFusion → Jev → DataFusion | 730 of 5,000 customers |
| Rank them by how likely they seem to leave. | DataFusion → Jev → DataFusion → Jev → DataFusion | +138 |

## JevIR

A plan is a list of typed steps. Inputs refer to earlier steps, so every document is acyclic.

```json
{"id": "scored", "op": "semantic_score", "input": "with_history",
 "context": ["review_history"],
 "question": "How increasingly dissatisfied with our pricing does this customer appear?",
 "levels": ["No pricing complaints", "Occasional complaints", "Complaints growing over time"],
 "output": "dissatisfaction"}
```

| Relational | Semantic |
|---|---|
| `scan` `filter` `project` `join` `aggregate` `sort` `top_k` | `semantic_filter`: keep rows where a statement holds (probability ≥ threshold) |
| `fetch`: attach each row's related rows as a list (a customer's review history) | `semantic_score`: a 0–1 position on ordered levels |
| | `semantic_choice`: one label out of a set |

Expressions are a typed AST (`{"kind": "binary", "op": ">=", …}`), not SQL text. Every step is type-checked before anything runs, and errors name the step:

```text
step `f`: unknown column `revenue`; available: order_id, customer_id, amount, order_date
```

The NL compiler feeds these errors back to the model until the plan type-checks.

## Optimizer

The rules are semantics-preserving. After rewriting, the plan is re-type-checked and its output schema must be unchanged.

- **Predicate pushdown.** Deterministic filters move below semantic operators, Fetch, projections, joins and aggregates (when they only read group columns).
- **Semantic late execution.** Score, choice and Fetch move above a TopK that doesn't sort by their output, so they run on *k* rows. **SemanticFilter never moves past a TopK**, because filtering then taking the top *k* is a different question from the reverse.
- **Projection pushdown.** Scans read only the columns that are needed. Semantic operators whose output nobody reads are removed.
- **Batching.** Adjacent semantic operators over the same context share one request per row. Fusion never extends past a SemanticFilter, because the extra question would be asked for rows the filter drops, which can cost more than it saves.

`JevBatchExec` deduplicates identical states, reuses cached answers across queries in a session, sends requests concurrently, and refuses to send more than `--max-semantic-rows` rows to one operator (default 10,000).

## Benchmarks

`benchmarks/run.py` runs each question twice in fresh engines: once exactly as written, in natural question order ("naive"), and once optimized. The results must match. Large scale, simulated backend ([full report](benchmarks/results-large.md)):

| Question | Rows → semantic (naive → optimized) | Est. semantic cost | Results |
|---|---|---|---|
| Customers with ≥ 8 reviews who seem unhappy about pricing | 13,396 → 3,056 (4.4×) | $0.098 → $0.055 | identical |
| Pricing unhappiness of the 100 biggest spenders this year | 19,715 → 100 (197×) | $0.115 → $0.0007 | identical |
| Most urgent open enterprise tickets, and their topic | 41,426 → 1,628 (25×) | $0.0014 → $0.0007 | identical |

Costs are estimated at Jev's list price ($0.042 per million input tokens). The simulator answers instantly, so wall-clock times understate the savings with a networked model. The data comes from `examples/generate.py`, and planted personas let the benchmark score accuracy. The simulator's precision on "unhappy about pricing" is about 70%. Run `--backend jev` to measure Jev on the same ground truth.

## Status

Working and tested (46 Rust tests, 14 Python tests):
- JevIR, the optimizer, and DataFusion + semantic execution on CSV and Parquet.
- The CLI and shell.
- The NL compiler, tested against scripted model replies and the real type checker.

Not yet verified:
- **Live Jev.** The TypeSafe backend is tested against the documented wire format. A live test runs when `TYPESAFE_API_KEY` is set.
- **Live Claude.** Same for the compiler with `ANTHROPIC_API_KEY`.
- **The simulated backend is a keyword heuristic, not a model.** It exists so the engine, demo and benchmarks run offline.

Known limits:
- The cache is in-memory, per session.
- Semantic operators are not yet moved across joins, which needs cardinality estimates.
- Float sums may differ in the last bits between plans (parallel aggregation order).
- Python talks to the engine through a JSON-lines process (`jevnql-engine serve`); PyO3 bindings are not built yet.

## Repository

```text
crates/
  jevir/          logical + physical IR, types, expressions, JSON, validation, EXPLAIN
  jev-optimizer/  rewrite rules, logical → physical planning, batch fusion
  jev-executor/   DataFusion lowering, JevBatchExec runtime, metrics, table profiles
  jev-provider/   SemanticBackend trait; TypeSafe Jev, simulated and mock backends
  jevnql-core/    engine API (open, prepare, execute)
  jevnql-cli/     `jevnql-engine`: catalog, validate, explain, run, serve
python/jevnql/    NL compiler (Claude), `jevnql` shell, engine bridge
examples/         data generator, demo plans, demo.sh
benchmarks/       naive vs optimized suite and reports
```

## Roadmap

- v0.2: JevSQL, a second frontend that compiles to the same JevIR.
- v0.3: retrieval (full-text search, embeddings) for candidate generation and scalable semantic joins.
- v0.4: cost-based semantic planning (selectivity estimates, speculative fusion), persistent semantic cache, predicate reuse.
- v0.5: semantic indexes.
