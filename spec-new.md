# JevNQL Architecture Amendment: Rust Core

Before continuing implementation, inspect the current repository and implementation against this amendment.

Do NOT blindly rewrite working code. Preserve reusable components, tests, prompts, schemas, examples, documentation, and API work where appropriate.

The architectural direction has changed:

> JevNQL should be a real semantic query engine with a Rust systems core, not primarily a Python application orchestrating DuckDB and Jev.

The long-term architecture should be:

```text
Natural Language
      │
      ▼
NQL Compiler
(Python initially)
      │
      ▼
Logical JevIR
(Rust)
      │
      ▼
Semantic Query Optimizer
(Rust)
      │
      ▼
Physical JevIR / Execution Plan
(Rust)
      │
      ├──────────────────────┐
      ▼                      ▼
Relational Engine       Semantic Runtime
Arrow/DataFusion             Jev
      │                      │
      └──────────┬───────────┘
                 ▼
             Arrow Results
```

## Core architectural principle

Natural language must NEVER compile directly to SQL as the primary execution architecture.

The pipeline must remain:

```text
Natural Language
      ↓
Logical JevIR
      ↓
Optimization
      ↓
Physical Plan
      ↓
Execution
```

SQL may be generated or executed internally when appropriate, but SQL is an execution mechanism, not the intermediate representation.

---

# 1. Rust Core

Create a Rust workspace approximately structured as:

```text
crates/

  jevir/
      Logical IR
      Physical IR
      schemas
      serialization
      validation

  jev-optimizer/
      optimization rules
      cost model
      plan transformations

  jev-executor/
      physical execution
      scheduling
      batching

  jev-provider/
      TypeSafe Jev backend
      semantic execution interface

  jevnql-core/
      public engine API

  jevnql-cli/
      CLI
```

Names may be adjusted if the existing repository structure makes another organization cleaner.

---

# 2. Use Apache Arrow + DataFusion

Do NOT build relational database machinery from scratch.

Investigate using Apache DataFusion for:

* CSV/Parquet scanning
* Arrow representation
* filtering
* projection
* joins
* aggregation
* sorting
* TopK/limit
* relational logical plans
* relational physical execution

JevNQL should concentrate engineering effort on semantic computation.

The system should eventually support execution plans containing both:

```text
Relational operators

Scan
Filter
Project
Join
Aggregate
Sort
TopK
```

and:

```text
Semantic operators

SemanticFilter
SemanticScore
SemanticChoice
SemanticJoin
SemanticCompare
```

Only the first three semantic operators are required for the initial MVP.

---

# 3. Logical JevIR

JevIR must be a real typed IR rather than arbitrary dictionaries passed around the application.

Example conceptual plan:

```text
Scan(customers)
    ↓
Join(orders)
    ↓
Aggregate(total_spend)
    ↓
TopK(500)
    ↓
SemanticScore(
    context=review_history,
    question="customer appears increasingly dissatisfied"
)
    ↓
TopK(20)
```

Rust types should represent these operations.

The IR should support serialization/deserialization so the Python compiler can produce it.

JSON is acceptable as the frontend/core boundary initially.

Example:

```json
{
  "op": "semantic_score",
  "input": "...",
  "context": ["review_history"],
  "question": "customer appears increasingly dissatisfied",
  "output": "dissatisfaction_score"
}
```

Validate IR before execution.

---

# 4. Logical vs Physical Plans

Keep these concepts separate from the beginning.

Logical JevIR represents WHAT should happen.

Physical JevIR represents HOW it will happen.

Example logical operation:

```text
SemanticFilter(
    predicate="customer likely to churn"
)
```

Possible physical operation:

```text
JevBatchExec {
    predicate: "customer likely to churn",
    batch_size: N,
    concurrency: N,
    cache_policy: ...
}
```

Similarly:

```text
Logical:
Aggregate(total_spend)

Physical:
DataFusion HashAggregateExec
```

The optimizer bridges these layers.

---

# 5. Semantic Backend Interface

Jev must be treated as an execution backend, not hardcoded throughout the engine.

Create an abstraction conceptually equivalent to:

```text
SemanticBackend

evaluate_noul(...)
evaluate_score(...)
evaluate_choice(...)
```

Initial implementation:

```text
TypeSafeJevBackend
```

This backend may call the hosted TypeSafe Jev API.

All TypeSafe-specific HTTP/API logic must remain isolated in the provider crate.

The rest of the engine should know nothing about TypeSafe HTTP endpoints.

This leaves room for:

```text
Jev hosted API
future local Jev runtime
other System-One models
test/mock backend
```

without redesigning JevNQL.

---

# 6. Optimizer

Implement only a small optimizer initially, but design the boundary properly.

MVP optimization rules:

### Deterministic predicate pushdown

Perform cheap deterministic filters as early as possible.

### Projection pushdown

Avoid loading/sending unnecessary columns.

### Semantic late execution

Where logically equivalent, reduce candidate rows using deterministic computation before semantic evaluation.

### Semantic batching

Combine semantic judgments into efficient Jev batches.

Future architecture should allow:

```text
cost-based semantic planning
semantic caching
semantic predicate reuse
candidate retrieval
semantic indexes
semantic join optimization
```

Do NOT implement all of these now.

---

# 7. Python Layer

Python remains useful.

Keep or build:

```text
python/jevnql/
    compiler/
    api/
    bindings/
```

Python responsibilities:

```text
Natural language
      ↓
LLM interpretation
      ↓
Logical JevIR JSON
      ↓
Rust engine
```

Python should NOT own:

```text
query optimization
execution scheduling
semantic batching
physical planning
core IR implementation
```

Those belong in Rust.

Use PyO3/maturin when appropriate to expose the Rust engine through Python.

However, do not let Python bindings block getting the Rust core working. A CLI or JSON boundary is acceptable for the first working version.

---

# 8. EXPLAIN is important

JevNQL should make execution visible.

Design toward:

```text
JevNQL> EXPLAIN
"Find our highest-spending customers
 who seem increasingly unhappy."

LOGICAL PLAN

SemanticScore[dissatisfaction]
└── TopK[spend, 500]
    └── Aggregate[SUM(amount)]
        └── Scan[orders]


OPTIMIZED PHYSICAL PLAN

TopKExec[20]
└── JevBatchExec[dissatisfaction]
    └── TopKExec[spend,500]
        └── HashAggregateExec
            └── ParquetExec[orders]
```

Also collect execution metrics:

```text
Rows scanned
Rows after deterministic filtering
Rows semantically evaluated
Jev batches
Input tokens
Cache hits
Execution time
Estimated semantic cost
```

These metrics are part of the product, not merely debugging information.

---

# 9. Preserve the MVP

This architectural amendment must NOT turn the project into a six-month database rewrite.

The immediate MVP remains:

```text
Natural Language
      ↓
Logical JevIR
      ↓
simple optimizer
      ↓
Rust execution
      ↓
DataFusion + Jev
      ↓
Result
```

Required initial data formats:

```text
CSV
Parquet
```

Required semantic operations:

```text
SemanticFilter
SemanticScore
SemanticChoice
```

Required deterministic capabilities:

```text
Scan
Filter
Project
Aggregate
Sort
TopK
basic Join
```

Use DataFusion wherever possible.

---

# 10. Explicitly Do NOT Build Yet

Do not implement:

* custom storage engine
* distributed execution
* arbitrary-scale SemanticJoin
* semantic indexes
* vector database
* streaming execution
* cloud platform
* authentication
* custom relational operators already provided adequately by DataFusion
* custom SQL parser unless necessary

These belong on the roadmap.

---

# 11. Migration Instructions

Before modifying code:

1. Inspect everything currently implemented.
2. Identify components that can remain unchanged.
3. Identify Python components that should move into Rust.
4. Identify code that should be discarded because it violates the new architecture.
5. Produce a concise migration plan.
6. Then execute the migration.
7. Keep the repository runnable throughout the migration where practical.
8. Update tests alongside architectural changes.
9. Do not leave duplicate Python and Rust implementations of the core engine unless temporarily required during migration.

Do not optimize for preserving code merely because it already exists.

Optimize for preserving GOOD code while arriving at the correct architecture.

---

# 12. North-Star Architecture

The eventual project should support multiple frontends:

```text
                JevNQL
                   │
                JevSQL
                   │
              Python SDK
                   │
                   ▼
              Logical JevIR
                   │
                   ▼
          Semantic Optimizer
                   │
                   ▼
              Physical Plan
                   │
       ┌───────────┼────────────┐
       ▼           ▼            ▼
  DataFusion      Jev       Retrieval
       │           │            │
       └───────────┼────────────┘
                   ▼
                 Arrow
```

JevNQL is therefore not:

> English → SQL.

And it is not:

> SQL + a Jev API function.

It is:

> **A semantic query engine that compiles natural-language questions into optimized deterministic and semantic execution plans.**

The Rust core should be designed around that definition.

---

# 13. Priority

When forced to choose between features and architecture during this migration, prioritize:

1. Correct typed JevIR
2. Clean semantic backend abstraction
3. Logical/physical plan separation
4. Working DataFusion execution
5. Working Jev semantic execution
6. Basic optimizer
7. Natural-language compiler
8. CLI / UX
9. Additional features

Do not sacrifice items 1–5 to ship more surface-level features.

The goal is not to maximize code written this weekend.

The goal is to establish a foundation capable of becoming a serious open-source semantic query engine.
