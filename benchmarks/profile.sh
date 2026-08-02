#!/usr/bin/env bash
# Sample one server under load and print where its cycles went.
#
# Deliberately not a benchmark. The load here is short, the profiler perturbs
# what it measures, and the numbers below are shares of samples rather than
# times. The question it answers is "which functions", and the answer is only
# ever a lead to confirm with run-linux.sh.
set -euo pipefail
# Reporting pipelines below end in `head`, which closes the pipe as soon as it
# has enough lines. That SIGPIPEs the producer, `pipefail` turns it into a
# failure, and `-e` then aborts the script — silently dropping every section
# after the first long one. Each such pipeline ends in `|| true` for that
# reason; do not tidy them away.

APP=${APP:-churust}
PORT=${PORT:-8080}
WORKERS=${WORKERS:-4}
DURATION=${DURATION:-20}
CONNECTIONS=${CONNECTIONS:-64}
THREADS=${THREADS:-4}
FREQ=${FREQ:-999}
SERVER_CPUS=${SERVER_CPUS:-0-7}
CLIENT_CPUS=${CLIENT_CPUS:-8-11}
# Both apps expose hyper's flush aggregation, and the keep-alive shape this
# profiles is the one where it is off. Same reason run-linux.sh sets it: a
# profile of a setting nobody measures explains a cost nobody pays.
export PIPELINE_FLUSH=${PIPELINE_FLUSH:-0}

case "$APP" in
  churust) BIN=/apps/bench-churust ;;
  hyper)   BIN=/apps/bench-hyper ;;
  *) echo "APP must be churust or hyper" >&2; exit 1 ;;
esac

taskset -c "$SERVER_CPUS" env PORT="$PORT" WORKERS="$WORKERS" "$BIN" >/dev/null 2>&1 &
SERVER_PID=$!
trap 'kill "$SERVER_PID" 2>/dev/null || true' EXIT INT TERM

for _ in $(seq 200); do
  curl -fsS "http://127.0.0.1:$PORT/plaintext" >/dev/null 2>&1 && break
  kill -0 "$SERVER_PID" 2>/dev/null || { echo "$APP exited during startup" >&2; exit 1; }
  sleep 0.1
done

# Warm first, then profile. Sampling the first seconds of a server catches page
# faults and first-touch allocation that a steady-state server never pays again,
# and they land in the profile as if they were the cost of serving.
taskset -c "$CLIENT_CPUS" wrk -t"$THREADS" -c"$CONNECTIONS" -d5s \
  "http://127.0.0.1:$PORT/plaintext" >/dev/null 2>&1 || true

taskset -c "$CLIENT_CPUS" wrk -t"$THREADS" -c"$CONNECTIONS" -d"${DURATION}s" \
  "http://127.0.0.1:$PORT/plaintext" >/dev/null 2>&1 &
LOAD_PID=$!

# `CALLGRAPH=dwarf` costs a stack dump per sample — a far bigger perf.data and a
# heavier probe — but frame-pointer unwinding does not survive the trip through
# kernel frames here, and a flat profile alone cannot say *who* called the
# expensive function. Use fp to find the cost, dwarf to find the caller.
CALLGRAPH=${CALLGRAPH:-fp}
# `cycles:u` counts only user-space cycles. It is what makes the call graph
# resolvable: with kernel samples included, most stacks begin in kernel frames
# that frame-pointer unwinding cannot walk, and every user-space caller behind
# them is lost. Restricting the event keeps each sample inside the frames the
# binary actually has pointers for. Use the default `cycles` for the flat
# profile, where the kernel share is itself the interesting part.
EVENT=${EVENT:-cycles}
[ "$CALLGRAPH" = dwarf ] && CG=dwarf,16384 || CG=fp
perf record -F "$FREQ" -e "$EVENT" -g --call-graph "$CG" -p "$SERVER_PID" -o /tmp/perf.data \
  -- sleep "$((DURATION - 2))" >/dev/null 2>&1 || true
wait "$LOAD_PID" 2>/dev/null || true

# `-g none` on purpose. Frame-pointer unwinding does not survive the trip
# through kernel frames here, so every entry came back trailing two unresolved
# addresses that made the output unreadable and told us nothing. Self time with
# no call graph is the part of this profile that is actually trustworthy.
echo "=== $APP · $WORKERS workers · flat profile (self time) ==="
perf report -i /tmp/perf.data --stdio --no-children -g none --percent-limit 0.3 \
  2>/dev/null | awk '/^ +[0-9]/ { pct = $1; $1 = ""; $2 = ""; sub(/^ +/, ""); print pct "\t" $0 }' \
  | head -50 || true

# A flat profile names the cost but not the reason. `SYMBOL=__memcpy_generic`
# asks who called it — which is the difference between "Churust memcpys more
# than hyper" and knowing which line to change.
# `ANNOTATE=<symbol>` shows which *source lines* inside one function cost, not
# just which function does. The flat profile can say the engine's service
# closure is 4.8% of user cycles; only this can say which of the header checks,
# the `Call` construction or the response building that 4.8% actually is.
# Needs the `-g` build this image already does.
if [ -n "${ANNOTATE:-}" ]; then
  echo
  echo "=== $APP · source lines inside $ANNOTATE ==="
  perf annotate -i /tmp/perf.data --stdio -l -s "$ANNOTATE" --percent-limit 0.8 \
    2>/dev/null | grep -vE "^\s*$" | head -70 || true
fi

if [ -n "${SYMBOL:-}" ]; then
  echo
  echo "=== $APP · callers of $SYMBOL ==="
  # Grepped out of the full report rather than isolated with `--symbols`, which
  # silently produced nothing here when combined with a call-graph mode.
  perf report -i /tmp/perf.data --stdio --no-children -g caller --percent-limit 0.8 \
    2>/dev/null | grep -A 30 -- "$SYMBOL" | head -60 || true
  echo
  echo "=== $APP · call-graph report, head ==="
  perf report -i /tmp/perf.data --stdio --no-children -g caller --percent-limit 2 \
    2>/dev/null | head -60 || true
fi
