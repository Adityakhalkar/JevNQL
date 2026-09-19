# JevNQL benchmark: naive vs optimized

Data: `large` (20,000 customers). Semantic backend: `simulated` (keyword stand-in, not Jev; accuracy reflects the simulator).

## unhappy_among_active

> Which customers with at least 8 reviews seem unhappy about pricing?

| plan | rows scanned | rows → Jev | distinct states | batches / requests | input tokens | est. cost | time |
|---|---|---|---|---|---|---|---|
| naive | 156,420 | 13,396 | 13,304 | 1 / 13,304 | 2,339,672 | $0.0983 | 655 ms |
| optimized | 156,420 | 3,056 | 3,056 | 1 / 3,056 | 1,308,489 | $0.0550 | 346 ms |

- Rows sent to the semantic backend: **13,396 → 3,056** (4.4× fewer)
- Estimated semantic cost: $0.0983 → $0.0550
- Results: identical (663 rows)
- Accuracy vs planted personas: precision 70% (467/663 returned rows are `pricing_sour`), recall 99% of 470

## top_spenders_unhappy

> How unhappy with pricing is each of our 100 biggest spenders this year?

| plan | rows scanned | rows → Jev | distinct states | batches / requests | input tokens | est. cost | time |
|---|---|---|---|---|---|---|---|
| naive | 2,078,210 | 19,715 | 13,040 | 1 / 13,040 | 2,736,089 | $0.1149 | 732 ms |
| optimized | 2,078,210 | 100 | 77 | 1 / 77 | 15,883 | $0.0007 | 71 ms |

- Rows sent to the semantic backend: **19,715 → 100** (197.2× fewer)
- Estimated semantic cost: $0.1149 → $0.0007
- Results: identical (100 rows)
- Accuracy vs planted personas: precision 86% (6/7 pricing_dissatisfaction >= 0.4 are `pricing_sour`)

## urgent_enterprise_tickets

> Which open enterprise tickets sound most urgent, and what are they about?

| plan | rows scanned | rows → Jev | distinct states | batches / requests | input tokens | est. cost | time |
|---|---|---|---|---|---|---|---|
| naive | 40,713 | 41,426 | 602 | 2 / 602 | 32,346 | $0.0014 | 66 ms |
| optimized | 40,713 | 1,628 | 242 | 2 / 242 | 16,514 | $0.0007 | 13 ms |

- Rows sent to the semantic backend: **41,426 → 1,628** (25.4× fewer)
- Estimated semantic cost: $0.0014 → $0.0007
- Results: identical (25 rows)
