#!/usr/bin/env bash
# Compare Churust against actix-web, axum, Ktor and Go on identical work.
#
# The equivalence check below is not ceremony. Two apps that return different
# bytes are doing different work, and a throughput number comparing them is
# fiction — which is the usual reason framework benchmarks cannot be trusted.
set -euo pipefail

cd "$(dirname "$0")"

DURATION=${DURATION:-10s}
WRK_DURATION=${WRK_DURATION:-8s}
CONNECTIONS=${CONNECTIONS:-64}
WRK_THREADS=${WRK_THREADS:-4}
# 64, not TechEmpower's 16. At 16 every server here is still pinned to the
# kernel's loopback packet rate on this class of machine — the same thing that
# makes the keep-alive mode below unable to rank anything. See README.md.
DEPTH=${DEPTH:-64}
ROUNDS=${ROUNDS:-3}
ROUTES=(/plaintext /json /user/42)

# One port per app, and all five running at once, so the rounds below can
# interleave them: a machine that gets slower halfway through a run must not
# hand that slowdown to whichever app happened to be measured last.
CHURUST_PORT=${CHURUST_PORT:-8111}
AXUM_PORT=${AXUM_PORT:-8112}
ACTIX_PORT=${ACTIX_PORT:-8113}
GO_PORT=${GO_PORT:-8114}
KTOR_PORT=${KTOR_PORT:-8116}

# Which apps to run. Trim this when a toolchain is missing, rather than editing
# the script: `APPS="churust axum" ./run.sh`.
APPS=${APPS:-"churust actix axum ktor go"}

require() { # name [install-hint]
  command -v "$1" >/dev/null 2>&1 || {
    echo "missing: $1${2:+ — $2}" >&2
    exit 1
  }
}
# curl drives both the health-check poll and the equivalence check, and python3
# formats every number in the report — both are required before anything is
# built. The load generators are required later, right before they are used, so
# a missing one cannot stop this script from proving the equivalence gate works.
require curl
require python3

# PIDs of every server this run started, and the scratch dir for the
# equivalence-check output. Declared empty up front (rather than left unset) so
# `cleanup` can test them with plain `[ -n ... ]` under `set -u` however early
# it fires.
PIDS=""
WORKDIR=""

# Kills exactly the processes this run started, by PID, and removes the scratch
# dir. Not `pkill -f target/release/bench-churust`: that pattern matches *any*
# process on the machine with that string in its command line, so two checkouts
# of this repo running at once — ordinary here, the project uses worktrees —
# would have one run's exit trap kill the other run's servers mid-measurement.
cleanup() {
  for pid in $PIDS; do
    kill "$pid" >/dev/null 2>&1 || true
  done
  if [ -n "$WORKDIR" ]; then
    rm -rf "$WORKDIR"
  fi
}
# Installed before the first server starts, not after the last one: if the third
# app fails its health check, the two already running must still be reaped.
trap cleanup EXIT INT TERM

have() { # app-name — is this app in $APPS?
  case " $APPS " in *" $1 "*) return 0 ;; *) return 1 ;; esac
}

port_of() {
  case "$1" in
    churust) echo "$CHURUST_PORT" ;;
    axum) echo "$AXUM_PORT" ;;
    actix) echo "$ACTIX_PORT" ;;
    go) echo "$GO_PORT" ;;
    ktor) echo "$KTOR_PORT" ;;
  esac
}

java_home() {
  echo "${BENCH_JAVA_HOME:-$(/usr/libexec/java_home -v 21)}"
}

# ---------------------------------------------------------------- build

build() {
  if have churust; then (cd bench-churust && cargo build --release --quiet); fi
  if have axum; then (cd bench-axum && cargo build --release --quiet); fi
  if have actix; then (cd bench-actix && cargo build --release --quiet); fi
  if have go; then
    require go "https://go.dev/dl/"
    (cd bench-go && go build -o bench-go .)
  fi
  if have ktor; then
    require gradle "brew install gradle"
    # JAVA_HOME is pinned rather than inherited: the Kotlin compiler rejects
    # JDK version strings it does not recognise, and fails with an internal
    # compiler error rather than anything that names the cause.
    (cd bench-ktor && JAVA_HOME="$(java_home)" gradle fatJar --quiet --no-daemon)
  fi
}

start_one() { # name
  local name="$1" port
  port=$(port_of "$name")
  case "$name" in
    churust)
      (cd bench-churust && PORT="$port" PIPELINE_FLUSH="${PIPELINE_FLUSH:-1}" \
        exec ./target/release/bench-churust) &
      ;;
    axum) (cd bench-axum && PORT="$port" exec ./target/release/bench-axum) & ;;
    actix) (cd bench-actix && PORT="$port" exec ./target/release/bench-actix) & ;;
    go) (cd bench-go && PORT="$port" exec ./bench-go) & ;;
    ktor)
      (PORT="$port" exec "$(java_home)/bin/java" \
        -jar bench-ktor/build/libs/bench-ktor.jar) &
      ;;
  esac
  # `exec` inside the subshell replaces it with the server binary, so the PID
  # captured here is the server's own and not a `cd`-wrapper shell that would
  # leave the real process unreachable. Recorded before the health-check loop,
  # so a process that starts but never answers is still reaped by `cleanup`.
  PIDS="$PIDS $!"
  # 200 tries at 0.1s. A JVM needs several seconds to reach its first response,
  # and a deadline tight enough for a Rust binary would fail Ktor every time.
  for _ in $(seq 200); do
    curl -fsS "http://127.0.0.1:$port/plaintext" >/dev/null 2>&1 && return 0
    sleep 0.1
  done
  echo "$name never came up on $port" >&2
  exit 1
}

# --------------------------------------------------- equivalence gate

# Every response must match on status, headers and body, with two exceptions.
#
# `date` legitimately differs on every request and says nothing about whether
# two apps did the same work. Ktor omits it entirely, which is one header of
# work it does not do — recorded in the report rather than papered over.
#
# Header *name case* and header *order* are normalised, because RFC 9110 §5.1
# makes field names case-insensitive and §5.3 makes the order of differently
# named fields insignificant. Go writes `Content-Type` where hyper writes
# `content-type`, and a gate that called that a difference could never admit a
# Go server at all. Nothing else is filtered: a stray header, a wrong
# content-length or a missing content-type is a real difference and fails.
#
# Compared via files and `cmp -s`, not `a=$(...); b=$(...); [ "$a" != "$b" ]`:
# command substitution strips *all* trailing newlines, so a body ending in `\n`
# and the same body without one would compare equal.
#
# `curl` runs without `-f` on purpose: with `-f`, a non-2xx response makes curl
# exit non-zero, which under `set -e` would kill the script on curl's raw stderr
# before this function reported anything.
#
# Byte-identical is necessary but not sufficient: two empty files (a server that
# never answered) also compare equal, and so do two 404s from a route that
# stopped being registered everywhere at once. So each response's status line
# must actually be a 200 before the comparison is trusted.
fetch_normalised() { # url outfile
  local body="$2.body"
  # Headers and body captured separately, so the header block can be sorted
  # without disturbing a body that may contain anything at all.
  curl -sS -D "$2.hdr" -o "$body" "$1"
  {
    head -n1 "$2.hdr" | tr -d '\r'
    tail -n +2 "$2.hdr" \
      | tr -d '\r' \
      | grep -viE '^date:' \
      | grep -v '^$' \
      | awk -F': ' '{ printf "%s: %s\n", tolower($1), substr($0, index($0, ": ") + 2) }' \
      | sort
    echo "--body--"
    cat "$body"
  } >"$2"
}

check_equivalence() {
  local failed=0 reference="" ref_name=""
  WORKDIR=$(mktemp -d)

  for route in "${ROUTES[@]}"; do
    ref_name=""
    for app in $APPS; do
      local out="$WORKDIR/$app"
      fetch_normalised "http://127.0.0.1:$(port_of "$app")$route" "$out"

      local status
      status=$(head -n1 "$out")
      if [[ "$status" != "HTTP/1.1 200"* ]]; then
        echo "NOT OK on $route: $app returned '${status:-<empty response>}', not 200" >&2
        failed=1
        continue
      fi

      if [ -z "$ref_name" ]; then
        ref_name="$app"
        reference="$WORKDIR/reference"
        cp "$out" "$reference"
        continue
      fi
      if ! cmp -s "$reference" "$out"; then
        echo "NOT OK on $route: $app and $ref_name do not agree" >&2
        diff "$reference" "$out" >&2 || true
        failed=1
      fi
    done
  done

  if [ "$failed" -ne 0 ]; then
    echo >&2
    echo "refusing to measure: the apps are not doing the same work." >&2
    exit 1
  fi
  echo "equivalence: OK — $(echo "$APPS" | wc -w | tr -d ' ') apps agree on ${#ROUTES[@]} routes"
}

# ------------------------------------------------------------ measure

# CPU seconds a process has used so far, in seconds. The metric that survives a
# saturated network path: when every server is pinned to the same kernel limit,
# what still separates them is how much CPU each spent getting there.
cpu_secs() {
  ps -o time= -p "$1" 2>/dev/null | tr -d ' ' \
    | awk -F: '{ n=NF; s=0; m=1; for (i=n;i>=1;i--) { s += $i * m; m *= 60 } print s }'
}

pid_on_port() {
  lsof -nP -iTCP:"$1" -sTCP:LISTEN -t 2>/dev/null | head -1
}

RESULTS=""

record() { # app mode rps cpu_us_per_req
  RESULTS="$RESULTS$1|$2|$3|$4
"
}

measure_keepalive() { # app
  local port pid before after out rps secs
  port=$(port_of "$1")
  pid=$(pid_on_port "$port")
  before=$(cpu_secs "$pid")
  out=$(oha --no-tui -c "$CONNECTIONS" -z "$DURATION" \
    "http://127.0.0.1:$port/plaintext" 2>&1)
  after=$(cpu_secs "$pid")
  rps=$(echo "$out" | grep "Requests/sec" | awk '{print $2}')
  # oha reports a rate, not a count; the count is the rate over the wall clock.
  secs=${DURATION%s}
  record "$1" keepalive "$rps" \
    "$(python3 -c "print(f'{($after-$before)*1e6/max($rps*$secs,1):.2f}')")"
}

measure_pipelined() { # app
  local port pid before after out rps reqs
  port=$(port_of "$1")
  pid=$(pid_on_port "$port")
  before=$(cpu_secs "$pid")
  out=$(PIPELINE="$DEPTH" wrk -t"$WRK_THREADS" -c"$CONNECTIONS" -d"$WRK_DURATION" \
    -s pipeline.lua "http://127.0.0.1:$port/plaintext" 2>&1)
  after=$(cpu_secs "$pid")
  rps=$(echo "$out" | grep "Requests/sec" | awk '{print $2}')
  reqs=$(echo "$out" | grep "requests in" | awk '{print $1}')
  record "$1" pipelined "$rps" \
    "$(python3 -c "print(f'{($after-$before)*1e6/max($reqs,1):.2f}')")"
}

# ------------------------------------------------------------- report

report() {
  # Passed as an argument, not piped: a here-document on the same command
  # replaces stdin, so a piped `$RESULTS` would arrive as an empty report.
  python3 - "$ROUNDS" "$RESULTS" <<'PY'
import sys, statistics, collections

rounds = sys.argv[1]
rows = collections.defaultdict(list)
for line in sys.argv[2].splitlines():
    line = line.strip()
    if not line:
        continue
    app, mode, rps, cpu = line.split("|")
    rows[(mode, app)].append((float(rps), float(cpu)))

for mode, title in (
    ("pipelined", "Pipelined"),
    ("keepalive", "Keep-alive, no pipelining"),
):
    entries = [(app, v) for (m, app), v in rows.items() if m == mode]
    if not entries:
        continue
    # Median across rounds, not mean: one round that collided with something
    # else on the machine should not move the number reported beside it.
    ranked = sorted(
        (
            (
                app,
                statistics.median(r for r, _ in v),
                statistics.median(c for _, c in v),
            )
            for app, v in entries
        ),
        key=lambda t: -t[1],
    )
    best = ranked[0][1]
    print()
    print(f"### {title} (median of {rounds} rounds)")
    print()
    print("| framework | req/s | vs. best | server CPU µs/req |")
    print("|---|---:|---:|---:|")
    for app, rps, cpu in ranked:
        print(f"| {app} | {rps:,.0f} | {rps / best:.2f}x | {cpu:.2f} |")
PY
}

# --------------------------------------------------------------- main

main() {
  require oha "cargo install oha --locked"
  require wrk "brew install wrk"

  echo "building: $APPS"
  build

  echo "starting: $APPS"
  for app in $APPS; do start_one "$app"; done

  check_equivalence

  if have ktor; then
    # A JVM interprets its bytecode until the JIT has seen enough of it. Ktor
    # measured cold is a measurement of the interpreter, and it is roughly an
    # order of magnitude off. Every other app here is compiled ahead of time and
    # needs no equivalent.
    echo "warming the JVM (20s)"
    PIPELINE="$DEPTH" wrk -t"$WRK_THREADS" -c"$CONNECTIONS" -d20s -s pipeline.lua \
      "http://127.0.0.1:$KTOR_PORT/plaintext" >/dev/null 2>&1 || true
  fi

  for round in $(seq 1 "$ROUNDS"); do
    echo "round $round/$ROUNDS: pipelined (depth $DEPTH)"
    for app in $APPS; do measure_pipelined "$app"; done
  done

  # Churust restarts without `pipeline_flush` for the keep-alive pass. Answering
  # a client that does *not* pipeline with an aggregated flush is a deliberate
  # delay on every response, so measuring one configuration in both modes would
  # flatter one and libel the other.
  if have churust; then
    churust_pid=$(pid_on_port "$CHURUST_PORT")
    kill "$churust_pid" >/dev/null 2>&1 || true
    sleep 1
    PIPELINE_FLUSH=0 start_one churust
  fi

  for round in $(seq 1 "$ROUNDS"); do
    echo "round $round/$ROUNDS: keep-alive"
    for app in $APPS; do measure_keepalive "$app"; done
  done

  local stamp out
  stamp=$(date -u +%Y-%m-%d)
  mkdir -p results
  out="results/${stamp}-$(hostname -s).md"

  {
    echo "# Churust vs actix-web, axum, Ktor and Go — ${stamp}"
    echo
    echo "- host: \`$(hostname -s)\` — \`$(uname -srm)\`"
    echo "- rustc: \`$(rustc --version)\`"
    echo "- churust: \`$(grep -m1 '^version' ../Cargo.toml | cut -d'"' -f2)\`"
    echo "- load: \`wrk -t${WRK_THREADS} -c${CONNECTIONS} -d${WRK_DURATION}\` at pipeline depth ${DEPTH};"
    echo "  \`oha -c${CONNECTIONS} -z${DURATION}\` unpipelined"
    echo
    echo "Numbers from one machine at one moment. They are not a ranking, they do"
    echo "not transfer to other hardware, and every route here returns a constant."
    echo "The keep-alive table below ranks nothing — read benchmarks/README.md for"
    echo "why before quoting any of it."
    report
  } | tee "$out"

  echo
  echo "written to benchmarks/${out}"
}

main "$@"
