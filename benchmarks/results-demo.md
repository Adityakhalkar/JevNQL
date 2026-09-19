# JevNQL benchmark: naive vs optimized

Data: `demo` (5,000 customers). Semantic backend: `simulated` (keyword stand-in, not Jev; accuracy reflects the simulator).

## unhappy_among_active

> Which customers with at least 8 reviews seem unhappy about pricing?

| plan | rows scanned | rows → Jev | distinct states | batches / requests | input tokens | est. cost | time |
|---|---|---|---|---|---|---|---|
| naive | 39,340 | 3,345 | 3,335 | 1 / 3,335 | 583,901 | $0.0245 | 165 ms |
| optimized | 39,340 | 730 | 730 | 1 / 730 | 322,778 | $0.0136 | 89 ms |

- Rows sent to the semantic backend: **3,345 → 730** (4.6× fewer)
- Estimated semantic cost: $0.0245 → $0.0136
- Results: identical (138 rows)
- Accuracy vs planted personas: precision 67% (92/138 returned rows are `pricing_sour`), recall 99% of 93

## top_spenders_unhappy

> How unhappy with pricing is each of our 100 biggest spenders this year?

| plan | rows scanned | rows → Jev | distinct states | batches / requests | input tokens | est. cost | time |
|---|---|---|---|---|---|---|---|
| naive | 219,670 | 4,653 | 3,102 | 1 / 3,102 | 641,290 | $0.0269 | 146 ms |
| optimized | 219,670 | 100 | 67 | 1 / 67 | 12,827 | $0.0005 | 17 ms |

- Rows sent to the semantic backend: **4,653 → 100** (46.5× fewer)
- Estimated semantic cost: $0.0269 → $0.0005
- Results: identical (100 rows)
- Accuracy vs planted personas: precision 60% (3/5 pricing_dissatisfaction >= 0.4 are `pricing_sour`)

## urgent_enterprise_tickets

> Which open enterprise tickets sound most urgent, and what are they about?

| plan | rows scanned | rows → Jev | distinct states | batches / requests | input tokens | est. cost | time |
|---|---|---|---|---|---|---|---|
| naive | 10,365 | 10,730 | 594 | 2 / 594 | 31,900 | $0.0013 | 28 ms |
| optimized | 10,365 | 420 | 114 | 2 / 114 | 7,338 | $0.0003 | 8 ms |

- Rows sent to the semantic backend: **10,730 → 420** (25.5× fewer)
- Estimated semantic cost: $0.0013 → $0.0003
- Results: identical (25 rows)
