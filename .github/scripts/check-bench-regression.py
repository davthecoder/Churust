#!/usr/bin/env python3
"""Fail when a benchmark regressed past a factor.

`critcmp` always exits 0 — it reports, it does not judge — so the gate is here.
The factor is deliberately loose: GitHub-hosted runners swing 20-30% between
identical runs, and a gate that fires below its own noise floor is one people
learn to ignore.
"""

import re
import sys

UNITS = {"ns": 1e-9, "us": 1e-6, "µs": 1e-6, "ms": 1e-3, "s": 1.0}


def to_seconds(value: str, unit: str) -> float:
    return float(value) * UNITS[unit]


def main() -> int:
    path, factor = sys.argv[1], float(sys.argv[2])
    rows = []
    # Rows whose parsed base time is exactly 0 go here instead of into `rows`.
    # `head / base` either explodes or, guarded with a naive `base > 0 and
    # ...`, makes the row match neither branch: it counts toward `total` (so
    # the "empty table" guard below does not catch it either) yet can never
    # be flagged no matter how large `head` grows. An arbitrarily large real
    # regression would then exit 0. Zero is not a time a real benchmark
    # reports, so a zero base is treated as a row this script cannot judge,
    # not as one that is fine, and fails loudly instead of passing quietly.
    unanalysable = []
    # Every line that carries exactly two `value±error unit` measurements is a
    # real comparison row (critcmp's header and separator lines carry none).
    # Counting these separately from `rows` lets an empty or garbled table --
    # critcmp finding nothing in common between the two baselines, or the
    # compare step failing before it produced a real table -- be told apart
    # from a table that was read fine and simply had nothing to complain
    # about. Collapsing the two would let a broken compare step report
    # "no regressions" and be trusted exactly like a real clean run.
    total = 0
    for line in open(path):
        # critcmp rows look like:
        #   dispatch/bare_200   1.00   1.2±0.03µs   1.05   1.3±0.04µs
        times = re.findall(r"(\d+\.?\d*)±\d+\.?\d*(ns|us|µs|ms|s)", line)
        if len(times) != 2:
            continue
        total += 1
        name = line.split()[0]
        # Units differ across rows (and even across the two columns of one
        # row) whenever a benchmark's magnitude crosses a scale during a
        # run -- ns become µs, µs become ms. Comparing the numeric digits
        # without converting both sides to the same unit first would treat
        # "2.0ms" as smaller than "1900.0µs" and call a 5% improvement a
        # 950x regression, so every value is normalized to seconds before
        # the ratio is taken.
        base = to_seconds(*times[0])
        head = to_seconds(*times[1])
        if base <= 0:
            unanalysable.append(name)
            continue
        if head / base > factor:
            rows.append((name, base, head, head / base))

    if total == 0:
        print(
            f"no benchmark comparisons could be read from {path}; "
            "treating an empty or unparsable compare as a failure rather "
            "than silently reporting a clean run.",
            file=sys.stderr,
        )
        return 1

    for name, base, head, ratio in rows:
        print(f"REGRESSION {name}: {base:.3e}s -> {head:.3e}s ({ratio:.2f}x)")

    for name in unanalysable:
        print(
            f"UNANALYSABLE {name}: base time parsed as 0s, cannot compute a ratio",
            file=sys.stderr,
        )

    if rows or unanalysable:
        if rows:
            print(f"\n{len(rows)} benchmark(s) regressed past {factor:.2f}x.")
        if unanalysable:
            print(
                f"\n{len(unanalysable)} benchmark(s) had a zero base time and "
                "could not be judged."
            )
        return 1

    print(f"No benchmark regressed past {factor:.2f}x ({total} compared).")
    return 0


if __name__ == "__main__":
    sys.exit(main())
