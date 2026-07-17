#!/usr/bin/env bash
# `.cargo/audit.toml` exceptions carry `expiry=YYYY-MM-DD` markers (in the
# `[advisories] ignore` entries and in the yanked-crate documentation
# comments) so temporary cargo-audit exceptions get re-triaged instead of
# silently living there forever. Nothing previously enforced that date —
# this script fails if any marker's date has passed.
#
# Run from the Security Audit job in ci.yml (and ci-main.yml).
set -euo pipefail

AUDIT_TOML="${1:-.cargo/audit.toml}"
today=$(date -u +%Y-%m-%d)

if [ ! -f "$AUDIT_TOML" ]; then
  echo "ERROR: $AUDIT_TOML not found" >&2
  exit 1
fi

dates=$(grep -oE 'expiry=[0-9]{4}-[0-9]{2}-[0-9]{2}' "$AUDIT_TOML" | cut -d= -f2 || true)
if [ -z "$dates" ]; then
  echo "OK: no expiry markers found in $AUDIT_TOML."
  exit 0
fi

expired=0
while IFS= read -r expiry; do
  # YYYY-MM-DD is fixed-width and zero-padded, so lexical string comparison
  # is equivalent to chronological comparison — no `date -d` parsing needed.
  if [[ "$expiry" < "$today" ]]; then
    echo "EXPIRED: audit exception with expiry=$expiry has passed (today: $today)" >&2
    expired=1
  fi
done <<< "$dates"

if [ "$expired" -eq 1 ]; then
  echo "" >&2
  echo "One or more cargo-audit exceptions in $AUDIT_TOML have expired." >&2
  echo "Re-triage each: renew the expiry (with a reason) in a commit, or drop" >&2
  echo "the entry once the underlying dependency is upgraded past the" >&2
  echo "affected range. See the policy header in $AUDIT_TOML." >&2
  exit 1
fi

echo "OK: no expired cargo-audit exceptions in $AUDIT_TOML."
