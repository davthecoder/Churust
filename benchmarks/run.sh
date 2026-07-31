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
WARMUP=${WARMUP:-3s}
ROUTES=(/plaintext /json /user/42)

require() { # name [install-hint]
  command -v "$1" >/dev/null 2>&1 || {
    echo "missing: $1${2:+ — $2}" >&2
    exit 1
  }
}
# curl is needed for both the health-check poll and the equivalence check, and
# python3 formats every number in the final report — both are required up
# front. oha is only needed once we already trust the two apps agree, and is
# required later in main(), right before it is used — a missing oha must not
# stop this script from proving the equivalence gate works. Requiring python3
# here rather than letting it surface later means a missing interpreter is
# reported before either app is even built, not after two release builds and
# a full measurement run.
require curl
require python3

# PIDs of the two servers, and the scratch dir for equivalence-check output.
# Declared empty up front (rather than left unset) so `cleanup` can test them
# with plain `[ -n ... ]` under `set -u` no matter how early it fires.
CHURUST_PID=""
AXUM_PID=""
WORKDIR=""

# Kills exactly the two processes this run started, by PID, and removes the
# scratch dir. Not `pkill -f target/release/bench-churust`: that pattern
# matches *any* process on the machine with that string in its command line,
# so two checkouts of this repo running at once — ordinary here, the project
# uses worktrees — would have one run's exit trap kill the other run's
# servers mid-measurement. `if`-guarded rather than `&&`-chained: this runs
# from an EXIT trap, still under `set -e`, and a false `&&` chain here would
# abort the rest of cleanup instead of just skipping one step.
cleanup() {
  if [ -n "$CHURUST_PID" ]; then
    kill "$CHURUST_PID" >/dev/null 2>&1 || true
  fi
  if [ -n "$AXUM_PID" ]; then
    kill "$AXUM_PID" >/dev/null 2>&1 || true
  fi
  if [ -n "$WORKDIR" ]; then
    rm -rf "$WORKDIR"
  fi
}

start() { # name dir port pidvar — records the real server PID into $pidvar
  # `exec` inside the subshell replaces the subshell with the server binary,
  # so the PID bash captures via `$!` right after backgrounding is the
  # server's own PID, not a `cd`-wrapper shell that would leave the real
  # process unreachable by PID. Written into `$pidvar` via `printf -v`
  # (portable to bash 3.2, which this machine's `/bin/bash` is — no
  # namerefs) immediately, before the health-check loop below, so a process
  # that starts but never answers still gets recorded and reaped by
  # `cleanup` even though `start` itself exits non-zero in that case.
  (cd "$2" && PORT="$3" exec ./target/release/"$1") &
  printf -v "$4" '%s' "$!"
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
#
# Compared via files and `cmp -s`, not `a=$(...); b=$(...); [ "$a" != "$b" ]`:
# command substitution strips *all* trailing newlines, so a body ending in
# `\n` and the same body without one would compare equal. Neither app's body
# ends in a newline today, but the gate's whole premise is that passing means
# byte-identical, and a round-trip through `$(...)` can't back that up.
#
# `curl` runs without `-f` here on purpose: with `-f`, a non-2xx response
# makes curl itself exit non-zero, which under `set -e` would kill the script
# on curl's raw stderr before `check_equivalence` ever got to report
# anything. Without `-f`, curl still exits 0 on an HTTP error response, still
# captures its status line and body, and the mismatch it represents is
# reported the same way any other divergence is — not treated as a crash.
#
# Byte-identical is necessary but not sufficient: two empty files (a server
# that never answered, `curl` unable to connect) also `cmp -s` equal, and so
# do two 404s from a route that stopped being registered in both apps at
# once. Neither is "equivalent" in any sense worth measuring, so each file's
# first line must actually be a 200 before the byte comparison is trusted.
check_equivalence() {
  local failed=0
  local afile="$WORKDIR/a" bfile="$WORKDIR/b"
  for route in "${ROUTES[@]}"; do
    curl -sS -D- "http://127.0.0.1:$CHURUST_PORT$route" \
        | tr -d '\r' | grep -viE '^date:' >"$afile"
    curl -sS -D- "http://127.0.0.1:$AXUM_PORT$route" \
        | tr -d '\r' | grep -viE '^date:' >"$bfile"

    local astatus bstatus
    astatus=$(head -n1 "$afile")
    bstatus=$(head -n1 "$bfile")
    if [[ "$astatus" != "HTTP/1.1 200"* ]]; then
      echo "NOT OK on $route: churust returned '${astatus:-<empty response>}', not 200" >&2
      failed=1
    fi
    if [[ "$bstatus" != "HTTP/1.1 200"* ]]; then
      echo "NOT OK on $route: axum returned '${bstatus:-<empty response>}', not 200" >&2
      failed=1
    fi

    if ! cmp -s "$afile" "$bfile"; then
      echo "MISMATCH on $route" >&2
      diff "$afile" "$bfile" >&2 || true
      failed=1
    fi
  done
  return $failed
}

measure() { # route port
  oha --no-tui -c "$CONNECTIONS" -z "$DURATION" --json \
      "http://127.0.0.1:$2$1" 2>/dev/null
}

# Short, throwaway run against a route, discarded rather than reported. The
# first `measure` call after a process starts pays for whatever is still
# cold — allocator warm-up, lazy-initialised statics, the OS's own page-cache
# and branch-predictor state — costs every later call in the same process
# does not pay again. Without this, only the first row of the table would
# carry that one-time cost, making it look slower relative to the other
# routes for a reason that has nothing to do with the route itself.
warm_up() { # route port
  oha --no-tui -c "$CONNECTIONS" -z "$WARMUP" --json \
      "http://127.0.0.1:$2$1" >/dev/null 2>&1
}

main() {
  echo "building..."
  (cd bench-churust && cargo build --release -q)
  (cd bench-axum && cargo build --release -q)

  WORKDIR=$(mktemp -d)

  # Registered before the first server starts, not after both have: if
  # bench-axum fails its health check and start() exits, bench-churust (already
  # started) must still be reaped. A trap installed only after both starts
  # succeed would leak exactly that process on exactly that failure.
  trap cleanup EXIT

  start bench-churust bench-churust "$CHURUST_PORT" CHURUST_PID
  start bench-axum bench-axum "$AXUM_PORT" AXUM_PID

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

  echo "warming up..."
  for route in "${ROUTES[@]}"; do
    warm_up "$route" "$CHURUST_PORT"
    warm_up "$route" "$AXUM_PORT"
  done

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
      c=$(measure "$route" "$CHURUST_PORT" | python3 -c 'import json,sys; print(round(json.load(sys.stdin)["summary"]["requestsPerSec"]))')
      a=$(measure "$route" "$AXUM_PORT" | python3 -c 'import json,sys; print(round(json.load(sys.stdin)["summary"]["requestsPerSec"]))')
      echo "| \`${route}\` | ${c} | ${a} |"
    done
  } | tee "$out"

  echo
  echo "written to ${out}"
}

main "$@"
