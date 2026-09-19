#!/usr/bin/env bash
# The JevNQL demo from the spec, without API keys: JevIR plans on generated
# data, semantic judgments by the offline simulator.
#   examples/demo.sh [demo|large]
set -euo pipefail
cd "$(dirname "$0")/.."
scale="${1:-demo}"
data="examples/data/$scale"
engine="target/release/jevnql-engine"
[ -x "$engine" ] || engine="target/debug/jevnql-engine"
[ -x "$engine" ] || { echo "build first: cargo build --release -p jevnql-cli" >&2; exit 1; }
[ -d "$data" ] || python3 examples/generate.py --scale "$scale"

step() {
  printf '\n\033[1m%s\033[0m\n> %s\n\n' "$1" "$2"
  "$engine" run --backend "${BACKEND:-simulated}" --plan "examples/plans/$3.json" "$data"/*.csv | sed -n "${4:-/^RESULT/,\$p}"
}

step "1. A purely relational question" \
  "Which customers have submitted the most reviews?" most_reviews
step "2. Hybrid: deterministic narrowing, then semantic judgment" \
  "Which of our most active customers seem unhappy with pricing?" active_unhappy_pricing '/^PHYSICAL PLAN/,$p'
step "3. Another semantic operation on the result" \
  "Rank them by how likely they seem to leave." likely_to_leave
