#!/usr/bin/env bash
# Fails when `ci-ok` in `.github/workflows/ci-shared.yml` does not list every
# other job of that workflow in its `needs:`.
#
# `ci-ok` is the one check branch protection requires, and it only reports on
# the jobs it needs. A job added to the workflow without being added there
# would run, could fail, and would still let a pull request merge. This check
# runs inside `ci-ok` itself, so that drift turns the required check red.
#
# It also fails when `needs:` names a job that is not in the file (GitHub
# rejects that workflow too, but with an error that is easy to misread), and
# when a job is listed twice.
set -euo pipefail

workflow="${1:-.github/workflows/ci-shared.yml}"

python3 - "$workflow" <<'PY'
import sys
import yaml

path = sys.argv[1]
with open(path) as f:
    jobs = yaml.safe_load(f)["jobs"]

if "ci-ok" not in jobs:
    sys.exit(f"{path}: no `ci-ok` job")

needs = jobs["ci-ok"].get("needs", [])
if isinstance(needs, str):
    needs = [needs]

expected = sorted(j for j in jobs if j != "ci-ok")
failed = False

missing = sorted(set(expected) - set(needs))
if missing:
    failed = True
    print(f"{path}: jobs missing from ci-ok.needs: {', '.join(missing)}")

unknown = sorted(set(needs) - set(expected))
if unknown:
    failed = True
    print(f"{path}: ci-ok.needs names jobs that do not exist: {', '.join(unknown)}")

dupes = sorted({n for n in needs if needs.count(n) > 1})
if dupes:
    failed = True
    print(f"{path}: ci-ok.needs lists a job more than once: {', '.join(dupes)}")

if failed:
    sys.exit(1)
print(f"ci-ok needs all {len(expected)} jobs of {path}")
PY
