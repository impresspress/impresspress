#!/usr/bin/env bash
# Fails when a `Swatinem/rust-cache` step in any workflow does not save on
# `main` only.
#
# The action saves by default, and the rule has nothing else holding it: a new
# job that leaves the line out still passes, and goes back to writing entries
# that evict `main`'s. See the note in `.github/workflows/ci-shared.yml`.
set -euo pipefail

python3 - .github/workflows/*.yml <<'PY'
import sys
import yaml

expected = "${{ github.ref == 'refs/heads/main' }}"
checked = 0
failed = False

for path in sys.argv[1:]:
    with open(path) as f:
        jobs = yaml.safe_load(f).get("jobs", {})
    for job_id, job in jobs.items():
        for step in job.get("steps", []):
            if not str(step.get("uses", "")).startswith("Swatinem/rust-cache@"):
                continue
            checked += 1
            if (step.get("with") or {}).get("save-if") != expected:
                failed = True
                print(f"{path}: job `{job_id}`: rust-cache step needs `save-if: {expected}`")

if checked == 0:
    sys.exit("no Swatinem/rust-cache step found: this check is reading the wrong files")
if failed:
    sys.exit(1)
print(f"all {checked} rust-cache steps save on main only")
PY
