# JevNQL

**Natural language compiler for semantic data queries.**

## 1. Thesis

Traditional query languages operate on **values**.

```sql
WHERE revenue > 10000
WHERE country = 'India'
```

Real questions often operate on **meaning**.

> Find our highest-spending customers who seem increasingly dissatisfied with pricing.

JevNQL lets users express the entire query naturally, then compiles it into the cheapest combination of deterministic and semantic computation.

**Core idea:**

```text
Natural Language
       ↓
   JevNQL Compiler
       ↓
      JevIR
       ↓
     Optimizer
       ↓
 ┌─────┼─────────┐
 ↓     ↓         ↓
SQL   Jev    Retrieval
 ↓     ↓         ↓
 └─────┼─────────┘
       ↓
     Result
```

Jev is not the query language.

**Jev is one execution backend.**

---

# 2. Example

Dataset:

```text
customers.csv
orders.csv
reviews.csv
```

User asks:

> Find the 100 customers who spent the most this year and seem increasingly unhappy with our pricing.

JevNQL compiles:

```text
Scan(customers)

Join(orders, customer_id)

Filter(order_date >= 2026-01-01)

Aggregate(
    total_spend = SUM(order.amount)
)

TopK(total_spend, 100)

Join(reviews, customer_id)

SemanticScore(
    context = review_history,
    question =
      "Does this customer appear increasingly
       dissatisfied specifically with pricing?"
)

Sort(score DESC)
```

Execution:

```text
2,000,000 orders
       ↓
     DuckDB
       ↓
Top 100 customers
       ↓
Fetch review history
       ↓
      Jev
       ↓
Semantic ranking
       ↓
     Result
```

The important feature is that JevNQL does **not** send two million rows through Jev.

It understands which work belongs to the database and which requires judgment.

---

# 3. Architecture

## JevNQL

Human-facing source language.

Initially accept unrestricted natural language:

```text
Which customers purchase frequently but
appear increasingly frustrated?
```

Later introduce an optional structured NQL syntax:

```text
FROM customers

FIND customers who:
    purchase frequently
    AND seem increasingly frustrated

RANK BY:
    total revenue

RETURN:
    customer_id
    revenue
    frustration_score
```

Both compile into the same IR.

---

## JevIR

Typed intermediate representation.

Initial operators:

```text
Scan
Filter
Project
Join
Aggregate
Sort
TopK

SemanticFilter
SemanticScore
SemanticChoice
```

Example:

```json
{
  "op": "SemanticFilter",
  "input": "candidate_customers",
  "context": ["reviews"],
  "predicate": "customer appears dissatisfied with pricing",
  "threshold": 0.85
}
```

Later:

```text
SemanticJoin
SemanticCompare
SemanticGroup
Retrieve
Materialize
```

---

# 4. Query Planner

The compiler should understand the schema and decompose natural-language intent.

Question:

> Which high-value customers seem unhappy?

Becomes:

```text
"high-value"
      ↓
deterministic computation

SUM(orders.amount)
GROUP BY customer_id
TOP K

"seem unhappy"
      ↓
semantic computation

Jev(reviews + tickets)
```

The planner should distinguish:

### Deterministic

```text
count
sum
average
dates
comparisons
sorting
grouping
exact joins
```

→ DuckDB

### Semantic

```text
seems unhappy
sounds urgent
probably related
mentions indirectly
appears suspicious
shows buying intent
```

→ Jev

### Retrieval

```text
find potentially related records
find similar documents
candidate generation
```

→ FTS / embeddings later

---

# 5. Optimizer

This is the part that makes JevNQL an infrastructure project rather than text-to-SQL.

Initial optimization rules:

### Predicate pushdown

```text
Filter deterministic data BEFORE Jev.
```

### Projection pushdown

Only send Jev the fields necessary for its judgment.

### Semantic late execution

Run expensive semantic operations after deterministic candidate reduction whenever logically equivalent.

### Batch semantic evaluation

Combine judgments into efficient Jev batches.

Later:

```text
semantic caching
semantic predicate reuse
cost-based planning
semantic indexes
join reordering
candidate retrieval
```

---

# 6. Weekend MVP

Do **not** attempt to build the entire vision.

Ship:

```text
Natural Language
       ↓
     LLM
       ↓
     JevIR
       ↓
 simple optimizer
       ↓
 ┌─────┴─────┐
DuckDB      Jev
 └─────┬─────┘
       ↓
    Results
```

Support:

```text
CSV
Parquet
DuckDB
```

Implement approximately:

```text
Scan
Filter
Aggregate
Sort
TopK
Join

SemanticFilter
SemanticScore
SemanticChoice
```

That's enough.

---

# 7. CLI

Target experience:

```bash
pip install jevnql

jevnql reviews.csv
```

Then:

```text
JevNQL >

Which customers submitted the most reviews?

Planning...

SCAN reviews.csv
GROUP BY customer_id
COUNT reviews
ORDER DESC
LIMIT 10

Engine: DuckDB
Semantic evaluations: 0
```

Then:

```text
JevNQL >

Which frequent reviewers seem increasingly
unhappy with the product?

Planning...

SCAN reviews.csv
GROUP BY customer_id
FILTER frequent reviewers

SEMANTIC SCORE
"customer appears increasingly dissatisfied"

Engine:
DuckDB → Jev → DuckDB
```

---

# 8. Killer Demo

Use a realistic dataset containing:

```text
customers
orders
reviews
support tickets
```

Start easy:

> Which customer has submitted the most reviews?

Show:

```text
Jev evaluations: 0
```

Then:

> Which of our most active customers seem unhappy with pricing?

Show hybrid execution.

Then:

> Rank them by how likely they seem to leave.

Another semantic operation.

The visual execution plan should be part of the demo:

```text
SCAN reviews                       1,482,921
      │
GROUP customer_id                    82,192
      │
FILTER review_count > 5              12,481
      │
FETCH review_history                 12,481
      │
SEMANTIC SCORE
"dissatisfied with pricing"           1,193
      │
TOP 20
```

Include:

```text
Rows scanned
Rows semantically evaluated
Jev calls
Input tokens
Cache hits
Execution time
Estimated cost
```

This will make the project feel like infrastructure immediately.

---

# 9. Repository Structure

```text
jevnql/

├── compiler/
│   ├── parser
│   ├── schema
│   └── compiler
│
├── ir/
│   ├── operators
│   ├── types
│   └── validation
│
├── optimizer/
│   ├── pushdown
│   ├── semantic
│   └── planner
│
├── runtime/
│   ├── executor
│   ├── scheduler
│   └── batching
│
├── backends/
│   ├── duckdb
│   └── jev
│
├── cli/
│
├── examples/
│
└── benchmarks/
```

Keep the backend boundary clean:

```text
SemanticBackend

├── JevBackend
└── future providers
```

Don't bake TypeSafe HTTP calls throughout the codebase.

---

# 10. Things NOT to Build This Weekend

Avoid:

```text
Postgres extension
distributed execution
custom storage engine
vector database
semantic indexes
streaming
GUI
authentication
cloud hosting
fine-tuning
SemanticJoin at arbitrary scale
```

Those are future architecture.

Weekend goal is proving:

> **A compiler can intelligently split a natural-language data question into deterministic and semantic computation.**

---

# 11. Roadmap

### v0.1 — Compiler

```text
NQL → JevIR → DuckDB + Jev
```

### v0.2 — Semantic SQL

```text
JevSQL → JevIR
```

Two frontends, same runtime.

### v0.3 — Retrieval

```text
FTS
embeddings
candidate generation
```

Enables scalable SemanticJoin.

### v0.4 — Semantic optimizer

```text
cost estimation
semantic caching
batch fusion
predicate reuse
EXPLAIN SEMANTIC
```

### v0.5 — Semantic indexes

Explore:

```sql
CREATE SEMANTIC INDEX ...
```

### v1 — JevQL Engine

```text
             JevNQL
                │
             JevSQL
                │
                ▼
              JevIR
                │
         Semantic Optimizer
                │
     ┌──────────┼──────────┐
     ▼          ▼          ▼
 Relational   Semantic   Retrieval
   engine      engine      engine
```

---

# 12. Positioning

Do not describe JevNQL as:

> "AI that converts English to SQL."

That's an overcrowded category and undersells the architecture.

Use:

> **JevNQL is a compiler for natural-language data queries.**

Or:

> **Query values and meaning with the same language.**

Or the strongest technical description:

> **JevNQL compiles natural-language questions into optimized deterministic and semantic query plans.**

---

# 13. The North-Star Demo

Eventually this should work:

```text
JevNQL >

Find enterprise customers whose complaints
probably correspond to unresolved engineering
issues, rank them by customer value and urgency,
and explain which issue each customer is
probably experiencing.
```

Compiler:

```text
                 Natural Language
                        │
                        ▼
                      JevIR
                        │
                 Query Optimizer
                        │
          ┌─────────────┼─────────────┐
          ▼             ▼             ▼
       DuckDB        Retrieval       Jev
          │             │             │
 customer value   candidate issues  judgment
          │             │             │
          └─────────────┼─────────────┘
                        ▼
                     Results
```

The user describes **what they want to know**.

JevNQL figures out **how the computer should find out**.

That is the project.
