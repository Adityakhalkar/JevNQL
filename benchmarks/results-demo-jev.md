# JevNQL benchmark: naive vs optimized

Data: `demo` (5,000 customers). Semantic backend: `jev`.

## unhappy_among_active

> Which customers with at least 8 reviews seem unhappy about pricing?

| plan | rows scanned | rows → Jev | distinct states | batches / requests | input tokens | est. cost | time |
|---|---|---|---|---|---|---|---|
| naive | 39,340 | 3,345 | 3,335 | 1 / 3,335 | 1,829,206 | $0.0768 | 90,983 ms |
| optimized | 39,340 | 730 | 730 | 1 / 730 | 748,808 | $0.0314 | 20,555 ms |

- Rows sent to the semantic backend: **3,345 → 730** (4.6× fewer)
- Estimated semantic cost: $0.0768 → $0.0314
- Results: identical (93 rows)
- Accuracy vs planted personas: precision 100% (93/93 returned rows are `pricing_sour`), recall 100% of 93

## top_spenders_unhappy

> How unhappy with pricing is each of our 100 biggest spenders this year?

| plan | rows scanned | rows → Jev | distinct states | batches / requests | input tokens | est. cost | time |
|---|---|---|---|---|---|---|---|
| naive | 219,670 | 4,653 | 3,102 | 1 / 3,102 | 1,757,131 | $0.0738 | 83,883 ms |
| optimized | 219,670 | 100 | 67 | 1 / 67 | 36,333 | $0.0015 | 2,818 ms |

- Rows sent to the semantic backend: **4,653 → 100** (46.5× fewer)
- Estimated semantic cost: $0.0738 → $0.0015
- Results: same rows; values differ by up to 0.015 (semantic backend noise) (100 rows)
- Accuracy vs planted personas: precision 100% (4/4 pricing_dissatisfaction >= 0.4 are `pricing_sour`)

## urgent_enterprise_tickets

> Which open enterprise tickets sound most urgent, and what are they about?

| plan | rows scanned | rows → Jev | distinct states | batches / requests | input tokens | est. cost | time |
|---|---|---|---|---|---|---|---|
| naive | 10,365 | 10,730 | 594 | 2 / 594 | 223,029 | $0.0094 | 17,343 ms |
| optimized | 10,365 | 420 | 115 | 2 / 115 | 41,799 | $0.0018 | 4,886 ms |

- Rows sent to the semantic backend: **10,730 → 420** (25.5× fewer)
- Estimated semantic cost: $0.0094 → $0.0018
- Results: **6 of 25 rows differ** (25 rows)
