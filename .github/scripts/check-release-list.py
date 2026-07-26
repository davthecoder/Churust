#!/usr/bin/env python3
"""Fail if the release workflow's crate list has drifted from the workspace.

This drifted once already: seven crates were added while the list stayed at the
original seven. Because `shared-version` bumps every crate in step, the umbrella
crate's dependencies then name versions that were never published, the umbrella
publish fails, and the release lands half-done -- the unrecoverable state
RELEASING.md exists to prevent. A comment asking people to remember is not a
mechanism; this is.
"""

import json
import re
import subprocess
import sys

WORKFLOW = ".github/workflows/release.yml"


def main() -> int:
    meta = json.loads(
        subprocess.run(
            ["cargo", "metadata", "--no-deps", "--format-version", "1"],
            capture_output=True,
            text=True,
            check=True,
        ).stdout
    )
    # `publish = false` marks the examples, which are never released.
    packages = {p["name"]: p for p in meta["packages"] if p.get("publish") != []}

    match = re.search(r"for crate in ((?:[^\n]*\\\n)*[^\n;]*); do", open(WORKFLOW).read())
    if not match:
        print(f"could not find the crate list in {WORKFLOW}", file=sys.stderr)
        return 1
    listed = match.group(1).replace("\\", "").split()

    problems = []
    for name in sorted(set(packages) - set(listed)):
        problems.append(f"{name} is publishable but missing from {WORKFLOW}")
    for name in sorted(set(listed) - set(packages)):
        problems.append(f"{name} is listed in {WORKFLOW} but is not a publishable crate")

    # Publishing a crate before something it depends on fails: the dependency's
    # new version is not on the registry yet.
    position = {name: i for i, name in enumerate(listed)}
    for name, pkg in packages.items():
        if name not in position:
            continue
        for dep in pkg["dependencies"]:
            if dep["name"] in position and position[dep["name"]] > position[name]:
                problems.append(
                    f"{name} is published before its dependency {dep['name']}"
                )

    if problems:
        print("release crate list is out of date:", file=sys.stderr)
        for p in problems:
            print(f"  - {p}", file=sys.stderr)
        return 1

    print(f"release crate list matches the workspace ({len(listed)} crates, order ok)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
