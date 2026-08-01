#!/usr/bin/env python3
"""Self-test for check-bench-regression.py against committed fixtures.

`critcmp` always exits 0, so check-bench-regression.py is the only thing
that can fail a PR on a benchmark regression -- which makes the parser
itself the thing that most needs a check. A fixture proven once on someone's
laptop and never run again protects nothing against a future edit to the
regex or the unit table, so the fixtures live under fixtures/ and this
runner asserts every one of them against its expected exit code. Wired into
bench.yml as the first step after checkout, before either bench even starts,
so a broken parser is caught in seconds rather than after two full builds.
"""

import pathlib
import subprocess
import sys

HERE = pathlib.Path(__file__).resolve().parent
SCRIPT = HERE / "check-bench-regression.py"
FIXTURES = HERE / "fixtures"
# Must match the factor `.github/workflows/bench.yml`'s "Fail on a regression
# past 20%" step actually passes to check-bench-regression.py. There is no
# single source for this value -- it is duplicated here on purpose, so this
# suite exercises the same threshold CI enforces. If bench.yml's threshold
# ever changes without this one following, the fixtures below stop proving
# anything about the live gate; keep the two in sync by hand.
FACTOR = "1.20"

# fixture name -> (expected exit code, what it proves)
CASES = {
    "regressed.txt": (1, "a benchmark past the factor must fail"),
    "clean.txt": (0, "noise-level moves must not fail"),
    "improved.txt": (0, "a speedup must never be reported as a regression"),
    "mixed_units.txt": (
        1,
        "only the cross-unit regression fires; the cross-unit false alarm "
        "(ms vs us) must not",
    ),
    "empty.txt": (1, "an empty compare must fail loudly, not silently pass"),
    "malformed.txt": (
        1,
        "a compare step that produced no real table must fail loudly, not "
        "silently pass",
    ),
    "zero_base.txt": (
        1,
        "a zero base time is unanalysable, not a free pass no matter how "
        "large head becomes",
    ),
}


def main() -> int:
    failures = []
    for name, (expected, why) in CASES.items():
        path = FIXTURES / name
        if not path.exists():
            failures.append(f"{name}: fixture missing at {path}")
            print(f"[FAIL] {name}: fixture missing")
            continue

        result = subprocess.run(
            [sys.executable, str(SCRIPT), str(path), FACTOR],
            capture_output=True,
            text=True,
        )
        ok = result.returncode == expected
        print(f"[{'ok' if ok else 'FAIL'}] {name}: exit={result.returncode} want={expected} ({why})")
        if not ok:
            failures.append(
                f"{name}: exit {result.returncode}, expected {expected}\n"
                f"stdout: {result.stdout}\nstderr: {result.stderr}"
            )

    if failures:
        print(f"\n{len(failures)} fixture(s) did not match expectations:", file=sys.stderr)
        for f in failures:
            print(f"  - {f}", file=sys.stderr)
        return 1

    print(f"\nall {len(CASES)} fixtures matched expectations.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
