#!/usr/bin/env bash
# The JevNQL demo: three NQL questions on generated data.
#   examples/demo.sh [demo|large]
# Semantic judgments use Jev when TYPESAFE_API_KEY is set, else the offline
# simulator (override with BACKEND=jev|simulated).
set -euo pipefail
cd "$(dirname "$0")/.."
scale="${1:-demo}"
data="examples/data/$scale"
jevnql="target/release/jevnql"
[ -x "$jevnql" ] || jevnql="target/debug/jevnql"
[ -x "$jevnql" ] || { echo "build first: cargo build --release -p jevnql-cli" >&2; exit 1; }
[ -d "$data" ] || python3 examples/generate.py --scale "$scale"

step() {
  printf '\n\033[1m%s\033[0m\n\n' "$1"
  grep -v '^--' "examples/queries/$2.nql"
  echo
  "$jevnql" --backend "${BACKEND:-auto}" query --file "examples/queries/$2.nql" "$data"/*.csv | sed -n "${3:-/^RESULT/,\$p}"
}

step "1. A purely relational question" most_reviews
step "2. Hybrid: deterministic narrowing, then semantic judgment" active_unhappy_pricing '/^PHYSICAL PLAN/,$p'
step "3. Another semantic operation on the result" likely_to_leave
