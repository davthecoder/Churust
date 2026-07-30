#!/usr/bin/env bash
# Compare Churust against axum on identical work.
#
# The equivalence check below is not ceremony. Two apps that return different
# bytes are doing different work, and a throughput number comparing them is
# fiction — which is the usual reason framework benchmarks cannot be trusted.
set -euo pipefail

cd "$(dirname "$0")"

CHURUST_PORT=${CHURUST_PORT:-8111}
AXUM_PORT=${AXUM_PORT:-8112}
DURATION=${DURATION:-30s}
CONNECTIONS=${CONNECTIONS:-64}
ROUTES=(/plaintext /json /user/42)

require() { # name [install-hint]
  command -v "$1" >/dev/null 2>&1 || {
    echo "missing: $1${2:+ — $2}" >&2
    exit 1
  }
}
# curl is needed for both the health-check poll and the equivalence check, so
# it is required up front. oha is only needed once we already trust the two
# apps agree, and is required later in main(), right before it is used — a
# missing oha must not stop this script from proving the equivalence gate
# works.
require curl

start() { # name dir port
  (cd "$2" && PORT="$3" ./target/release/"$1" &)
  for _ in $(seq 50); do
    curl -fsS "http://127.0.0.1:$3/plaintext" >/dev/null 2>&1 && return 0
    sleep 0.1
  done
  echo "$1 never came up on $3" >&2
  exit 1
}

# Every response must match on status, headers and body, with the single
# exception of `date` — a header that legitimately differs on every request
# and says nothing about whether the two apps did the same work. Filtering
# out anything more than `date` would let a real difference (a stray header,
# a wrong content-length, a missing content-type) slip past unseen, so this
# strips exactly one line and compares everything else byte-for-byte.
check_equivalence() {
  local failed=0
  for route in "${ROUTES[@]}"; do
    local a b
    a=$(curl -fsS -D- "http://127.0.0.1:$CHURUST_PORT$route" \
        | tr -d '\r' | grep -viE '^date:')
    b=$(curl -fsS -D- "http://127.0.0.1:$AXUM_PORT$route" \
        | tr -d '\r' | grep -viE '^date:')
    if [ "$a" != "$b" ]; then
      echo "MISMATCH on $route" >&2
      diff <(echo "$a") <(echo "$b") >&2 || true
      failed=1
    fi
  done
  return $failed
}

measure() { # route port
  oha --no-tui -c "$CONNECTIONS" -z "$DURATION" --json \
      "http://127.0.0.1:$2$1" 2>/dev/null
}

main() {
  echo "building..."
  (cd bench-churust && cargo build --release -q)
  (cd bench-axum && cargo build --release -q)

  # Registered before the first server starts, not after both have: if
  # bench-axum fails its health check and start() exits, bench-churust (already
  # started) must still be reaped. A trap installed only after both starts
  # succeed would leak exactly that process on exactly that failure.
  trap 'pkill -f "target/release/bench-churust" >/dev/null 2>&1 || true; pkill -f "target/release/bench-axum" >/dev/null 2>&1 || true' EXIT

  start bench-churust bench-churust "$CHURUST_PORT"
  start bench-axum bench-axum "$AXUM_PORT"

  echo "checking the two apps agree..."
  if ! check_equivalence; then
    echo "refusing to measure: the apps do not return identical responses" >&2
    exit 1
  fi
  echo "equivalent."

  # Deferred to here on purpose: everything above this line — build, start,
  # health-check, equivalence gate — must work and be verifiable even on a
  # machine without oha installed. Only the measurement step needs it.
  require oha "install with: cargo install oha --locked"

  local stamp host out
  stamp=$(date -u +%Y-%m-%d)
  host=$(hostname | tr -d '\n')
  mkdir -p results
  out="results/${stamp}-${host}.md"

  {
    echo "# Churust vs axum — ${stamp}"
    echo
    echo "- host: \`${host}\`"
    echo "- os: \`$(uname -srm)\`"
    echo "- rustc: \`$(rustc --version)\`"
    echo "- churust: \`$(grep -m1 '^version' ../Cargo.toml | cut -d'"' -f2)\`"
    echo "- axum: \`$(cd bench-axum && cargo tree -p axum --depth 0 2>/dev/null | head -1)\`"
    echo "- command: \`oha -c ${CONNECTIONS} -z ${DURATION}\`"
    echo
    echo "Numbers from one machine at one moment. They are not a ranking, and"
    echo "they do not transfer to other hardware."
    echo
    echo "| route | churust req/s | axum req/s |"
    echo "|---|---|---|"
    for route in "${ROUTES[@]}"; do
      local c a
      c=$(measure "$route" "$CHURUST_PORT" | python3 -c 'import json,sys; print(f"{json.load(sys.stdin)["summary"]["requestsPerSec"]:.0f}")')
      a=$(measure "$route" "$AXUM_PORT" | python3 -c 'import json,sys; print(f"{json.load(sys.stdin)["summary"]["requestsPerSec"]:.0f}")')
      echo "| \`${route}\` | ${c} | ${a} |"
    done
  } | tee "$out"

  echo
  echo "written to ${out}"
}

main "$@"
