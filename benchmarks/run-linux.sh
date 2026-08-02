#!/usr/bin/env bash
# The comparison, on a Linux kernel, one server at a time.
#
# Two things make this different from run.sh, and both are about the number
# being about the software rather than about the host:
#
# 1. **A Linux loopback.** macOS saturates around 45-50k small round-trips a
#    second, below what any of these servers can answer, so an ordinary
#    keep-alive load there measures the host. Linux's ceiling is roughly an
#    order of magnitude higher, which is what lets the realistic workload — a
#    client that does not pipeline — be the headline instead of a workaround.
#
# 2. **One server at a time.** run.sh keeps all five alive and interleaves the
#    measurements, which protects against the machine drifting mid-run but
#    leaves four idle processes resident while the fifth is measured. Idle is
#    not free: a JVM runs GC and JIT-compiler threads with no traffic at all,
#    and five resident servers share caches and memory bandwidth. Here exactly
#    one server exists while it is being measured.
#
#    Losing the interleaving costs the protection it bought, so it is replaced:
#    the running order rotates every round, so each framework is measured first
#    in one round and last in another, and the reported figure is the median
#    across rounds. Drift that would otherwise land entirely on whoever went
#    last is spread across all of them.
set -euo pipefail

cd "$(dirname "$0")"

APPS=${APPS:-"churust actix axum ktor go"}
# Nine, not three or five. The between-run spread on this class of machine is
# wider than the differences being measured: the same code produced a 3.6%
# Churust lead, then 11%, then a 5.9% deficit, across three consecutive
# five-round runs. Nine rounds separated the same pair cleanly, with the
# ranges not overlapping at all. If a result matters, it has to survive more
# rounds than it takes to make it appear once.
ROUNDS=${ROUNDS:-9}
DURATION=${DURATION:-10s}
# 64, not 256. At 256 the servers queue deeply enough that a request can wait
# seconds, and on a laptop VM — whose vCPUs the host is itself timesharing with
# a desktop — those waits show up as timeouts that have nothing to do with the
# framework. 64 keeps every server comfortably inside its pinned cores, which
# is both the cleaner measurement and the more ordinary shape of traffic.
CONNECTIONS=${CONNECTIONS:-64}
# wrk's default request timeout is 2s, which is thousands of times the latency
# these servers actually serve at — so the only thing it ever catches is the
# burst when 256 connections are opened at once, and it catches it as an
# "error" that silently depresses the throughput of whichever server was
# slowest to accept them. Raised so that a stall has to be a real stall.
# Tail latency is reported instead, which is the honest way to show a server
# that occasionally takes far longer than its average.
TIMEOUT=${TIMEOUT:-10s}
THREADS=${THREADS:-4}
# One wrk thread per client CPU. Fewer client CPUs than threads is the mistake
# that makes the load generator the thing being measured: four threads on two
# cores thrash, the client stops being able to offer steady load, and every
# framework's number collapses and scatters at once. If you change THREADS,
# change CLIENT_CPUS with it.
DEPTH=${DEPTH:-16}
PORT=${PORT:-8080}
# Seconds of load before the clock starts. The JVM interprets its bytecode
# until the JIT has seen enough of it, and Ktor measured cold is a measurement
# of the interpreter — roughly an order of magnitude off. The others need far
# less, but they get the same treatment so that the warm-up is not itself a
# difference between them.
WARMUP=${WARMUP:-5}
JVM_WARMUP=${JVM_WARMUP:-25}
ROUTES=(/plaintext /json /user/42)

# The server and the load generator get disjoint CPUs.
#
# Without this they compete, and the competition is not evenly felt: a server
# that needs 6.5 cores to hit its number leaves the client less room than one
# that needs 5.2, so the more CPU-hungry framework is penalised twice — once for
# the CPU it uses and again for the CPU it denies the client. That is a
# measurement of how the two processes were scheduled against each other, not of
# either one.
#
# Pinning makes it a fixed question instead: what does this server do with
# exactly these cores, while the client has exactly those. Every framework gets
# the same split, so whatever the split costs, it costs all of them equally.
SERVER_CPUS=${SERVER_CPUS:-0-7}
CLIENT_CPUS=${CLIENT_CPUS:-8-11}

APP_DIR=${APP_DIR:-/apps}
SERVER_PID=""
WORKDIR=$(mktemp -d)
trap 'stop_server; rm -rf "$WORKDIR"' EXIT INT TERM

# --------------------------------------------------------------- lifecycle

start_server() { # name [extra-env...]
  local name="$1"
  shift
  local pin=(taskset -c "$SERVER_CPUS")
  case "$name" in
    churust) "${pin[@]}" env PORT="$PORT" "$@" "$APP_DIR/bench-churust" >/dev/null 2>&1 & ;;
    axum)    "${pin[@]}" env PORT="$PORT" "$@" "$APP_DIR/bench-axum"    >/dev/null 2>&1 & ;;
    actix)   "${pin[@]}" env PORT="$PORT" "$@" "$APP_DIR/bench-actix"   >/dev/null 2>&1 & ;;
    go)      "${pin[@]}" env PORT="$PORT" "$@" "$APP_DIR/bench-go"      >/dev/null 2>&1 & ;;
    ktor)    "${pin[@]}" env PORT="$PORT" "$@" java -jar "$APP_DIR/bench-ktor.jar" >/dev/null 2>&1 & ;;
    *) echo "unknown app: $name" >&2; exit 1 ;;
  esac
  SERVER_PID=$!
  for _ in $(seq 400); do
    if curl -fsS "http://127.0.0.1:$PORT/plaintext" >/dev/null 2>&1; then
      return 0
    fi
    # The process may have died rather than merely not be ready yet; without
    # this the loop waits the full 40s to report a server that exited at once.
    if ! kill -0 "$SERVER_PID" 2>/dev/null; then
      echo "$name exited during startup" >&2
      exit 1
    fi
    sleep 0.1
  done
  echo "$name never came up on $PORT" >&2
  exit 1
}

stop_server() {
  if [ -n "$SERVER_PID" ]; then
    kill "$SERVER_PID" >/dev/null 2>&1 || true
    wait "$SERVER_PID" 2>/dev/null || true
    SERVER_PID=""
  fi
  # The next server binds the same port, so the old listener has to be gone
  # rather than merely signalled.
  for _ in $(seq 100); do
    curl -fsS --max-time 1 "http://127.0.0.1:$PORT/plaintext" >/dev/null 2>&1 || return 0
    sleep 0.1
  done
}

# CPU seconds this process has used, summed across its threads. Read from
# /proc rather than `ps` so a JVM's many threads are counted the same way a
# Rust binary's few are.
cpu_secs() { # pid
  local hz utime stime
  hz=$(getconf CLK_TCK)
  # Fields 14 and 15 of /proc/pid/stat are utime and stime, in clock ticks.
  read -r utime stime < <(awk '{print $14, $15}' "/proc/$1/stat" 2>/dev/null || echo "0 0")
  python3 -c "print(($utime + $stime) / $hz)"
}

# ------------------------------------------------------- equivalence gate
#
# Captured one app at a time, since only one is ever running, then compared
# once every app has been seen. Same rule as run.sh: `date` is dropped, header
# name case and header order are normalised because RFC 9110 makes both
# insignificant, and nothing else is filtered.

capture() { # app
  local app="$1" route out
  for route in "${ROUTES[@]}"; do
    out="$WORKDIR/${app}$(echo "$route" | tr '/' '_')"
    curl -sS -D "$out.hdr" -o "$out.body" "http://127.0.0.1:$PORT$route"
    {
      head -n1 "$out.hdr" | tr -d '\r'
      tail -n +2 "$out.hdr" | tr -d '\r' | grep -viE '^date:' | grep -v '^$' \
        | awk -F': ' '{ printf "%s: %s\n", tolower($1), substr($0, index($0, ": ") + 2) }' \
        | sort
      echo "--body--"
      cat "$out.body"
    } >"$out"
  done
}

check_equivalence() {
  local failed=0 first="" route app
  for route in "${ROUTES[@]}"; do
    first=""
    for app in $APPS; do
      local f
      f="$WORKDIR/${app}$(echo "$route" | tr '/' '_')"
      if [[ "$(head -n1 "$f")" != "HTTP/1.1 200"* ]]; then
        echo "NOT OK on $route: $app did not return 200" >&2
        failed=1
        continue
      fi
      if [ -z "$first" ]; then first="$f"; continue; fi
      if ! cmp -s "$first" "$f"; then
        echo "NOT OK on $route: $app disagrees with the reference" >&2
        diff "$first" "$f" >&2 || true
        failed=1
      fi
    done
  done
  [ "$failed" -eq 0 ] || { echo "refusing to measure: the apps are not doing the same work." >&2; exit 1; }
  echo "equivalence: OK"
}

# ------------------------------------------------------------- measurement

RESULTS=""

measure() { # app mode
  local app="$1" mode="$2" before after out rps reqs warm script=()
  [ "$mode" = pipelined ] && script=(-s /bench/pipeline.lua)

  warm=$WARMUP
  [ "$app" = ktor ] && warm=$JVM_WARMUP
  PIPELINE="$DEPTH" taskset -c "$CLIENT_CPUS" wrk -t"$THREADS" -c"$CONNECTIONS" \
    --timeout "$TIMEOUT" -d"${warm}s" "${script[@]}" \
    "http://127.0.0.1:$PORT/plaintext" >/dev/null 2>&1 || true

  before=$(cpu_secs "$SERVER_PID")
  out=$(PIPELINE="$DEPTH" taskset -c "$CLIENT_CPUS" wrk -t"$THREADS" -c"$CONNECTIONS" \
    --timeout "$TIMEOUT" --latency -d"$DURATION" "${script[@]}" \
    "http://127.0.0.1:$PORT/plaintext" 2>&1)
  after=$(cpu_secs "$SERVER_PID")

  rps=$(echo "$out" | grep "Requests/sec" | awk '{print $2}')
  reqs=$(echo "$out" | grep "requests in" | awk '{print $1}')
  # `--latency` prints a percentile block; the 99% row is the one that shows a
  # server whose average is fine and whose worst requests are not.
  local p99
  p99=$(echo "$out" | awk '/^ *99%/ {print $2; exit}')
  p99=${p99:-n/a}

  # A round that hit socket errors or non-2xx responses did not measure what
  # the other rounds measured, and folding it into a median or a spread hides
  # that. wrk prints neither line when there was nothing wrong, so their
  # presence is the whole signal. Reported per round rather than silently
  # dropped: a harness that discards its own bad data without saying so is
  # indistinguishable from one that discards inconvenient data.
  #
  # Judged as a share of the requests served, not as a raw count. wrk counts
  # each connection's still-in-flight request when the clock runs out, so a
  # clean run of 64 connections reliably reports about 64 timeouts — against
  # eight million requests that is 0.0008% and means nothing. A count-based
  # flag fires on every run and stops being read; a proportional one fires when
  # something is actually wrong.
  local trouble="" errored=0 nonok=0
  errored=$(echo "$out" | awk '/Socket errors/ {for (i = 1; i <= NF; i++) {gsub(",", "", $i); if ($i ~ /^[0-9]+$/) t += $i}} END {print t + 0}')
  nonok=$(echo "$out" | awk '/Non-2xx/ {print $NF + 0; f = 1} END {if (!f) print 0}')
  if [ "$(python3 -c "print(1 if $errored > max($reqs, 1) * 0.001 else 0)")" = 1 ]; then
    trouble="${trouble}socket-errors($errored) "
  fi
  [ "$nonok" -gt 0 ] && trouble="${trouble}non-2xx($nonok) "
  [ -n "$trouble" ] && echo "   !! $app $mode: ${trouble}— $(echo "$out" | grep -E 'Socket errors|Non-2xx' | tr -s ' ' | tr '\n' ';')" >&2

  printf '   %-8s %12s req/s  p99 %-9s %s\n' "$app" "$rps" "$p99" "${trouble:-}"
  RESULTS="$RESULTS$app|$mode|$rps|$(python3 -c "print(f'{($after-$before)*1e6/max($reqs,1):.2f}')")|${trouble:-clean}|$p99
"
}

# The order rotates by one each round, so no framework is always measured first
# on a cold page cache or always last on a warm one.
rotated() { # round
  # Deliberate word splitting: APPS is a space-separated list, which is how it
  # arrives from the environment.
  # shellcheck disable=SC2206
  local -a apps=($APPS)
  local n=${#apps[@]} i out=""
  for ((i = 0; i < n; i++)); do
    out="$out ${apps[$(((i + $1) % n))]}"
  done
  echo "$out"
}

# --------------------------------------------------------------------- main

echo "host kernel: $(uname -sr) · $(nproc) cpus"
echo "load: wrk -t$THREADS -c$CONNECTIONS -d$DURATION · $ROUNDS rounds · one server at a time"
echo "cpus: server pinned to $SERVER_CPUS, load generator to $CLIENT_CPUS"
echo

echo "== capturing responses for the equivalence gate =="
for app in $APPS; do
  start_server "$app"
  capture "$app"
  stop_server
done
check_equivalence
echo

for mode in ${MODES:-keepalive pipelined}; do
  for round in $(seq 1 "$ROUNDS"); do
    echo "== $mode · round $round/$ROUNDS · order:$(rotated "$((round - 1))") =="
    for app in $(rotated "$((round - 1))"); do
      # Churust is the only app here whose pipelining behaviour is a choice, so
      # it is started the way each mode deserves: aggregating flushes for a
      # client that pipelines, and not for one that does not. Measuring one
      # setting in both modes would flatter one and libel the other.
      if [ "$app" = churust ] && [ "$mode" = pipelined ]; then
        start_server churust PIPELINE_FLUSH=1
      elif [ "$app" = churust ]; then
        start_server churust PIPELINE_FLUSH=0
      else
        start_server "$app"
      fi
      measure "$app" "$mode"
      stop_server
    done
  done
done

echo
python3 - "$ROUNDS" "$RESULTS" <<'PY'
import sys, statistics, collections


def to_ms(v):
    """wrk prints latency with a unit attached — 405.00us, 1.16ms, 2.25s.

    Parsed to a number before anything compares or sorts it. Sorting the raw
    strings is wrong in a way that looks right: "1.16ms" sorts before
    "405.00us", so the median of a set spanning two units came out as whichever
    value happened to land in the middle alphabetically. Every p99 this harness
    reported before this function existed was wrong.
    """
    v = v.strip()
    for suffix, scale in (("us", 0.001), ("ms", 1.0), ("s", 1000.0)):
        if v.endswith(suffix):
            try:
                return float(v[: -len(suffix)]) * scale
            except ValueError:
                return float("nan")
    return float("nan")


def fmt_ms(v):
    if v != v:
        return "n/a"
    return f"{v * 1000:.0f} us" if v < 1 else f"{v:.2f} ms"


rounds = sys.argv[1]
rows = collections.defaultdict(list)
for line in sys.argv[2].splitlines():
    line = line.strip()
    if not line:
        continue
    app, mode, rps, cpu, flag, p99 = line.split("|")
    rows[(mode, app)].append((float(rps), float(cpu), flag, to_ms(p99)))

for mode, title in (
    ("keepalive", "Keep-alive, no pipelining — the realistic shape"),
    ("pipelined", "Pipelined (depth 16) — dispatch headroom"),
):
    entries = [(a, v) for (m, a), v in rows.items() if m == mode]
    if not entries:
        continue
    ranked = sorted(
        (
            (a,
             statistics.median(r for r, _, _, _ in v),
             statistics.median(c for _, c, _, _ in v),
             min(r for r, _, _, _ in v),
             max(r for r, _, _, _ in v),
             sum(1 for _, _, f, _ in v if f != "clean"),
             statistics.median(p for _, _, _, p in v))
            for a, v in entries
        ),
        key=lambda t: -t[1],
    )
    best = ranked[0][1]
    print(f"\n### {title} (median of {rounds} rounds)\n")
    print("| framework | req/s | vs. best | server CPU µs/req | p99 latency | spread across rounds | bad rounds |")
    print("|---|---:|---:|---:|---:|---|---:|")
    for app, rps, cpu, lo, hi, bad, p99 in ranked:
        # A spread wider than 3x means one round measured something else
        # entirely; say so beside the number rather than leaving a reader to
        # notice it in the range.
        note = " ⚠" if hi > lo * 3 else ""
        print(f"| {app} | {rps:,.0f} | {rps / best:.2f}x | {cpu:.2f} | {fmt_ms(p99)} | {lo:,.0f}–{hi:,.0f}{note} | {bad} |")
    if any(hi > lo * 3 for _, _, _, lo, hi, _, _ in ranked):
        print("\n⚠ = at least one round differed from the others by more than 3x. "
              "Treat that row's median as provisional and re-run.")
PY
