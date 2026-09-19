"""Generate a synthetic customers / orders / reviews / tickets dataset.

    python examples/generate.py                  # demo scale -> examples/data/demo/
    python examples/generate.py --scale large    # ~2M orders -> examples/data/large/

Each customer has a hidden persona that drives its review and ticket text:
  content        happy or neutral
  pricing_sour   increasingly unhappy with pricing (complaints grow over time,
                 often worded indirectly: "for what we pay each month...")
  quality        bugs, crashes, slowness
  churn          evaluating alternatives, threatening to cancel
  support_heavy  many how-to tickets, otherwise fine

Personas are written to <out>-truth/personas.csv, outside the data directory,
so `jevnql <out>/*.csv` never sees them; benchmarks use them as ground truth.
Deterministic for a given --seed.
"""

from __future__ import annotations

import argparse
import csv
import math
import random
from datetime import date, timedelta
from pathlib import Path

SCALES = {
    "demo": {"customers": 5_000, "orders": 200_000},
    "large": {"customers": 20_000, "orders": 2_000_000},
}
START, END = date(2025, 1, 1), date(2026, 9, 18)
SPAN = (END - START).days

PERSONAS = {"content": 0.55, "pricing_sour": 0.15, "quality": 0.12, "churn": 0.08, "support_heavy": 0.10}
SEGMENTS = {"enterprise": (0.15, 2400.0), "mid-market": (0.35, 700.0), "smb": (0.50, 150.0)}
COUNTRIES = ["India", "United States", "Germany", "United Kingdom", "Brazil", "Japan", "Canada", "France"]
FIRST = ["Asha", "Ben", "Chen", "Dia", "Elena", "Farid", "Grace", "Hiro", "Ines", "Jonas", "Kavya", "Liam", "Maya",
         "Noah", "Olu", "Priya", "Quinn", "Rosa", "Sven", "Tara", "Umar", "Vera", "Wei", "Yara", "Zoe"]
LAST = ["Labs", "Systems", "Works", "Analytics", "Foods", "Logistics", "Health", "Retail", "Media", "Energy",
        "Capital", "Studios", "Robotics", "Travel", "Bio"]

POSITIVE = [
    "Great product, does exactly what we need.",
    "Setup was quick and the team loves it.",
    "Reliable and easy to use.",
    "Support answered within minutes, very happy.",
    "Solid tool, would recommend it to other teams.",
    "The new dashboard is a big improvement.",
]
NEUTRAL = [
    "It's fine. Does the job.",
    "Some features are missing but overall okay.",
    "Decent, with a bit of a learning curve.",
    "Works as advertised, nothing special.",
]
PRICING_MILD = [
    "A bit pricey, but it works.",
    "The renewal quote was higher than we expected.",
    "Good product; hoping the cost stays reasonable.",
]
PRICING_STRONG = [
    "Another price increase this year. This is getting hard to justify.",
    "We're paying far more than last year for the same features.",
    "For what we pay each month, I expected a lot more.",
    "Honestly the value isn't there anymore at this level.",
    "The new tier doubled our bill. Not happy.",
    "Budget review flagged this as our most expensive tool again.",
    "Finance keeps asking why this line item keeps growing.",
]
QUALITY = [
    "The app crashes whenever we export reports.",
    "Sync has been broken for a week.",
    "Too many bugs since the last update.",
    "Performance got noticeably slower.",
]
CHURN = [
    "We're evaluating alternatives.",
    "Considering switching to a competitor at renewal.",
    "If this isn't fixed we will cancel.",
    "Our team has mostly stopped using it.",
]

ASIDES = [
    "We use it daily.", "Our team is {n} people.", "We've been customers for {n} months.",
    "Mostly used by our ops team.", "Rolled out to {n} more seats this quarter.", "Using it across {n} projects.",
]

TICKETS = {
    "content": [("How do I add a teammate?", "Where can I invite a new user to our workspace?")],
    "support_heavy": [
        ("How do I add a teammate?", "Where can I invite a new user to our workspace?"),
        ("Export question", "Is there a way to schedule a weekly CSV export for our {n} reports?"),
        ("SSO setup", "Can you share the steps to configure SAML?"),
        ("Dashboard filters", "How do I save a filter on the dashboard?"),
    ],
    "pricing_sour": [
        ("Invoice higher than expected", "Our invoice went up {n}% again this quarter. Can someone explain the charges?"),
        ("Renewal quote", "The renewal quote is well above our budget. Are there other plans?"),
        ("Billing question", "Why are we being charged for {n} seats we don't use?"),
    ],
    "quality": [
        ("Export crashes", "Exporting any report over {n},000 rows crashes the app."),
        ("Sync broken", "Data has not synced for {n} days. This is blocking our team."),
    ],
    "churn": [
        ("Data export before cancelling", "How do we export all our data? We may not renew."),
        ("Contract end date", "Please confirm when our contract ends and the notice period."),
    ],
}


def day(rng: random.Random, lo: float = 0.0, hi: float = 1.0) -> date:
    return START + timedelta(days=int(rng.uniform(lo, hi) * SPAN))


def review(rng: random.Random, persona: str, t: float) -> tuple[int, str]:
    """(rating, text) for a review written at time fraction t in [0, 1]."""
    if persona == "pricing_sour":
        # complaints become likelier and harsher over time
        if rng.random() < 0.15 + 0.75 * t:
            strong = rng.random() < t
            return (rng.choice([1, 2]) if strong else 3, rng.choice(PRICING_STRONG if strong else PRICING_MILD))
        return rng.choice([4, 5]), rng.choice(POSITIVE + NEUTRAL)
    if persona == "quality" and rng.random() < 0.6:
        return rng.choice([1, 2, 3]), rng.choice(QUALITY)
    if persona == "churn" and rng.random() < 0.5 + 0.4 * t:
        return rng.choice([1, 2]), rng.choice(CHURN + QUALITY[:1])
    if rng.random() < 0.7:
        return rng.choice([4, 5]), rng.choice(POSITIVE)
    return 3, rng.choice(NEUTRAL)


def weighted(rng: random.Random, table: dict) -> str:
    keys = list(table)
    return rng.choices(keys, weights=[table[k] if isinstance(table[k], float) else table[k][0] for k in keys])[0]


def generate(out: Path, customers: int, orders: int, seed: int) -> None:
    rng = random.Random(seed)
    out.mkdir(parents=True, exist_ok=True)
    truth = out.parent / f"{out.name}-truth"
    truth.mkdir(parents=True, exist_ok=True)

    people = []
    for cid in range(1, customers + 1):
        segment = weighted(rng, SEGMENTS)
        persona = weighted(rng, PERSONAS)
        # spend weight: lognormal, scaled by segment
        spend = SEGMENTS[segment][1] * math.exp(rng.gauss(0, 0.8))
        activity = math.exp(rng.gauss(0, 0.9))  # how much they write
        people.append((cid, f"{rng.choice(FIRST)} {rng.choice(LAST)}", segment, persona, spend, activity))

    with open(out / "customers.csv", "w", newline="") as f:
        w = csv.writer(f)
        w.writerow(["customer_id", "name", "segment", "country", "signup_date"])
        for cid, name, segment, *_ in people:
            w.writerow([cid, name, segment, rng.choice(COUNTRIES), day(rng, 0, 0.3)])

    with open(truth / "personas.csv", "w", newline="") as f:
        w = csv.writer(f)
        w.writerow(["customer_id", "persona"])
        w.writerows((p[0], p[3]) for p in people)

    with open(out / "orders.csv", "w", newline="") as f:
        w = csv.writer(f)
        w.writerow(["order_id", "customer_id", "amount", "order_date"])
        ids = [p[0] for p in people]
        cum, total = [], 0.0
        for p in people:
            total += p[4]
            cum.append(total)
        chunk = 100_000
        for start in range(0, orders, chunk):
            buyers = rng.choices(ids, cum_weights=cum, k=min(chunk, orders - start))
            for i, cid in enumerate(buyers, start=start + 1):
                mean = people[cid - 1][4] / 20
                w.writerow([i, cid, round(mean * math.exp(rng.gauss(0, 0.5)), 2), day(rng)])

    review_id = ticket_id = 0
    with open(out / "reviews.csv", "w", newline="") as rf, open(out / "tickets.csv", "w", newline="") as tf:
        rw, tw = csv.writer(rf), csv.writer(tf)
        rw.writerow(["review_id", "customer_id", "rating", "text", "created_at"])
        tw.writerow(["ticket_id", "customer_id", "created_at", "subject", "body", "status", "priority"])
        for cid, _, _, persona, _, activity in people:
            n_reviews = min(40, int(rng.expovariate(1 / (3 * activity))))
            for t in sorted(rng.random() for _ in range(n_reviews)):
                review_id += 1
                rating, text = review(rng, persona, t)
                if rng.random() < 0.5:
                    text += " " + rng.choice(ASIDES).format(n=rng.randint(2, 60))
                rw.writerow([review_id, cid, rating, text, day(rng, t, t)])
            n_tickets = int(rng.expovariate(1 / (4 if persona == "support_heavy" else 1.2)))
            for _ in range(min(n_tickets, 25)):
                ticket_id += 1
                subject, body = rng.choice(TICKETS.get(persona, TICKETS["content"]))
                body = body.format(n=rng.randint(2, 60))
                priority = "high" if persona in ("quality", "churn") and rng.random() < 0.5 else rng.choice(["low", "normal"])
                tw.writerow([ticket_id, cid, day(rng), subject, body, rng.choice(["open", "closed"]), priority])

    print(f"{out}: {customers:,} customers, {orders:,} orders, {review_id:,} reviews, {ticket_id:,} tickets")
    print(f"{truth}/personas.csv: ground truth (not for loading into jevnql)")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--scale", choices=SCALES, default="demo")
    parser.add_argument("--out", type=Path, help="output directory (default examples/data/<scale>)")
    parser.add_argument("--seed", type=int, default=7)
    args = parser.parse_args()
    out = args.out or Path(__file__).parent / "data" / args.scale
    generate(out, **SCALES[args.scale], seed=args.seed)


if __name__ == "__main__":
    main()
