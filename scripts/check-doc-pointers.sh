#!/usr/bin/env bash
# Two guards on what comments claim. Both run; the exit status covers both.
#
#   1. A cited repo-relative documentation path must resolve.
#   2. A comment must describe current behaviour, not the change that
#      produced it. See the second block, below the first guard.
#
# Guard 1: fail when a tracked file outside the documentation tree cites a
# repo-relative documentation path that does not resolve in this repo.
#
# The invariant: a repo-relative path written in a source comment must name a
# file that exists here. A document that lives in another repository must not
# be cited with a path that looks in-repo — inline the substance instead, or
# name the external source unambiguously. A pointer to nothing is worse than
# no pointer: it reads as "the answer is written down over there" when it is
# not written down anywhere.
#
# What counts as a citation: the literal directory name below, followed by a
# slash and a path, where the character before it is not alphanumeric, `/` or
# `.`. That preceding-character rule is what keeps URLs and longer paths out
# (an MDN link, a `/b/vector/api/<dir>/a` route) with no allow-list of files.
#
# A citation resolves if the path exists as written, or with `.md` appended.
# A citation that ends the line on `-` or `/` is a path wrapped across two
# comment lines: it can never be verified by grep or followed by a reader, so
# it fails with its own message. Both joiners have to count — a path broken
# after a `/` leaves a token that IS a real directory, so an existence check
# alone waves it through, which is exactly the defect this guard exists to
# catch. A directory named mid-line is untouched; only end-of-line is a wrap.
#
# The one exemption: `.sql` files under a `migrations/` directory. Not a
# convenience — it is forced by an invariant of the migration system itself.
# `crate::migration_helper`'s "A shipped .sql file is immutable, comments
# included" records it: `apply_if_blessed` hashes a migration's WHOLE text, so
# editing a `--` comment changes its hash exactly as much as editing a
# statement does, and every deployment that already applied it then logs
# `schema drift` on each boot until someone redeploys with `--run-migrations`.
# A guard that demanded that edit would be asking for something the runtime
# punishes, so it does not ask. That rule also says where the explanation goes
# instead: the block's `migrations/mod.rs`, beside the constant, where it is
# not hash-addressed — which is where the admin block's dangling citations
# are written out (`blocks/admin/migrations/mod.rs`). That `mod.rs` is NOT
# exempt: it is ordinary source, and the exemption is exactly as wide as the
# hashing.
#
# Run from anywhere in the working tree.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

# The directory whose contents this guard resolves citations against. Spelled
# once, via a variable, so this script's own text contains no citation for it
# to find.
ROOT_DIR="docs"

mapfile -t candidates < <(
  git grep -I -l -e "${ROOT_DIR}/" -- ":!${ROOT_DIR}/" ":!**/migrations/*.sql" || true
)

# Emit one `status<TAB>file<TAB>line<TAB>path` record per citation.
# status is WRAPPED for a citation broken across lines, else CHECK.
# No candidate files means no records; awk with no file arguments would read
# stdin and hang, so it is not run at all.
records=""
if [ "${#candidates[@]}" -gt 0 ]; then
  records=$(
    awk -v dir="$ROOT_DIR" '
      BEGIN {
        # Preceding char must not be alphanumeric, "/" or "." — plus start-of-line.
        re = "(^|[^A-Za-z0-9/.])" dir "/[A-Za-z0-9_./-]+"
      }
      {
        line = $0
        rest = line
        consumed = 0
        while (match(rest, re)) {
          tok = substr(rest, RSTART, RLENGTH)
          endpos = consumed + RSTART + RLENGTH - 1
          consumed += RSTART + RLENGTH - 1
          rest = substr(rest, RSTART + RLENGTH)
          # Drop the preceding separator the pattern had to consume.
          if (substr(tok, 1, 1) != substr(dir, 1, 1)) tok = substr(tok, 2)
          # A path that runs to end-of-line and stops on a joiner is continued on
          # the next line. Both joiners count: `-` inside a dated filename, and
          # `/` between segments. The `/` shape is the dangerous one — the token
          # left behind is a real directory, so a bare existence check passes and
          # the broken citation ships silently.
          if (tok ~ /[-\/]$/ && endpos == length(line)) {
            printf "WRAPPED\t%s\t%d\t%s\n", FILENAME, FNR, tok
            continue
          }
          # Trailing sentence punctuation is not part of the path.
          sub(/[.-]+$/, "", tok)
          printf "CHECK\t%s\t%d\t%s\n", FILENAME, FNR, tok
        }
      }
    ' "${candidates[@]}"
  )
fi

failures=0
checked=0

while IFS=$'\t' read -r status file line path; do
  [ -n "${status:-}" ] || continue
  checked=$((checked + 1))
  if [ "$status" = "WRAPPED" ]; then
    failures=$((failures + 1))
    printf '%s:%s: wrapped path — "%s" is broken across comment lines.\n' \
      "$file" "$line" "$path"
    printf '    A path split over two lines cannot be verified or followed. Keep it on one line.\n'
    continue
  fi
  if [ -e "$path" ] || [ -e "$path.md" ]; then
    continue
  fi
  failures=$((failures + 1))
  printf '%s:%s: dangling path — "%s" does not exist in this repository.\n' \
    "$file" "$line" "$path"
  printf '    Inline what the document said, or name the external source without an in-repo path.\n'
done <<< "$records"

if [ "$failures" -gt 0 ]; then
  echo
  echo "check-doc-pointers: $failures unresolvable citation(s) out of $checked checked."
else
  echo "check-doc-pointers: $checked citation(s), all resolve."
fi

# ===========================================================================
# Guard 2: comments that narrate the change that produced them.
#
# A comment describing what a change DID — the pull request it landed under,
# the batch of work it belonged to, what the code held before it — is true
# only on the day it is written. The code moves on; the sentence does not, so
# the narration outlives what it described and becomes a false statement about
# current behaviour sitting in the file. That is where the false comments
# found in review came from. Describe what the code does now; the history is
# in `git log`, where it stays accurate and where a reader can see which parts
# of it are still live.
#
# NARRATION_RE below matches two shapes: a reference to a numbered pull
# request or batch of work, and a sentence opening on the capitalised adverb
# for "before now".
#
# What is NOT matched, and why. The same adverb in lower case ("read-through
# previously always PUT") and the phrase for "formerly" ("that branch used to
# be a removal") are narration just as often — a read of all 64 lower-case
# occurrences found roughly 52 of them narrating, and a 15-line sample of the
# 388 occurrences of the phrase found 14. They are left out on VOLUME, not
# because they are clean: together they are 451 more comment lines in 175
# further files, which is a ratchet of its own to land and shrink, and the
# "formerly" phrase additionally has a live "employed to" sense ("the subject
# used to key the token") that has to be read per hit rather than counted.
# Widening to them is a follow-up wave, not a tightening of this one.
#
# Those counts are this scan's own, not hand-collected. To reproduce one, run
# this script with the pattern substituted and the allowlist emptied:
#
#   sed -e "s@^NARRATION_RE=.*@NARRATION_RE='(^|[^A-Za-z])used to'@" \
#       -e 's@^NARRATION_ALLOWLIST=.*@NARRATION_ALLOWLIST=/dev/null@' \
#       scripts/check-doc-pointers.sh | bash
#
# What counts as a comment. A block comment that OPENS a line (`/*`, `<!--`)
# puts every following line into the comment until its terminator, which is
# what makes a continuation line and a wrapped sentence visible. Otherwise the
# first opener-looking token on the line starts the comment, among the openers
# that file type actually has — a stylesheet has the block form only, so a
# `url(...)` is not read as a comment. "Opener-looking" is still the honest
# word: the scan is textual, so three classes can be read as a comment when
# they are not —
#   * a string or URL containing the line-comment token before the match;
#   * a shell parameter expansion, where the substitution sigil is not a
#     comment at all;
#   * a value in a config file carrying the same sigil.
# There are none in the tree today. A future one fails CI on a line that is
# not a comment, so the message points here: move the text out of the literal,
# or add the file to the allowlist with a note.
#
# Two exemptions, both structural:
#   * `.sql` under `migrations/` — hash-addressed and immutable, for the
#     reason written out above.
#   * vendored third-party sources — not ours to rewrite.
# Documentation and commit messages are not scanned at all: narrating history
# is their job.
#
# NARRATION_ALLOWLIST is a ratchet, not an exemption. One line per file that
# carries narration today, `<count><TAB><path>`, counting comment LINES: a
# line is reported once however many of the shapes it carries. A file that is
# NOT listed must have none, so a new file cannot narrate its way in. A listed
# file may not exceed its recorded count, and when the count drops the line
# must come down with it, so every listed file can only shrink. A count of
# zero is rejected: the line is deleted instead.
NARRATION_ALLOWLIST="scripts/history-narration-allowlist.txt"

# The substitution sigil, which is also this file's own comment opener. Held
# in a variable, like ROOT_DIR above, so no line of this script contains both
# a comment opener and a shape the guard matches — otherwise the guard flags
# its own pattern, and the exemption would be an accident of how the
# alternation happens to be ordered.
NARRATION_HASH='#'
NARRATION_RE="(Previously|[Ww]ave [0-9]+|[Tt]his (PR|commit)|PRs? ${NARRATION_HASH}?[0-9]+)"

mapfile -t sources < <(
  git ls-files \
    '*.rs' '*.ts' '*.tsx' '*.js' '*.mjs' '*.cjs' '*.go' '*.css' '*.html' '*.htm' '*.tmpl' \
    '*.sh' '*.sql' '*.toml' '*.yml' '*.yaml' \
    ':!:**/migrations/*.sql' ':!:**/vendor/**' ':!:**/node_modules/**'
)

# One `file<TAB>line<TAB>phrase` record per narrating comment line. As above,
# awk with no file arguments would read stdin, so it is not run on an empty
# list.
narration=""
if [ "${#sources[@]}" -gt 0 ]; then
  narration=$(
    awk -v re="$NARRATION_RE" -v hash="$NARRATION_HASH" '
      # `x.html.tmpl` is HTML; the generated suffix says nothing about syntax.
      function family(name,   base) {
        base = name
        sub(/\.tmpl$/, "", base)
        if (base ~ /\.(rs|ts|tsx|js|mjs|cjs|go)$/) return "slash"
        # CSS has the block form and NOT the line form: a `//` in a stylesheet
        # is inside a `url(...)`, never a comment.
        if (base ~ /\.css$/) return "block"
        if (base ~ /\.(html|htm)$/) return "angle"
        if (base ~ /\.(sh|toml|ya?ml)$/) return "sigil"
        if (base ~ /\.sql$/) return "dash"
        return ""
      }
      FNR == 1 { in_block = 0; kind = family(FILENAME) }
      kind == "" { next }
      {
        body = ""
        if (in_block) {
          # Inside a block that opened on an earlier line: everything up to
          # the terminator is comment text.
          e = index($0, closer)
          if (e == 0) {
            body = $0
          } else {
            body = substr($0, 1, e - 1)
            in_block = 0
          }
        } else if ((kind == "slash" || kind == "block") && $0 ~ /^[ \t]*\/\*/) {
          in_block = 1
          closer = "*/"
          match($0, /\/\*/)
          body = substr($0, RSTART + 2)
          e = index(body, closer)
          if (e > 0) { body = substr(body, 1, e - 1); in_block = 0 }
        } else if (kind == "angle" && $0 ~ /^[ \t]*<!--/) {
          in_block = 1
          closer = "-->"
          match($0, /<!--/)
          body = substr($0, RSTART + 4)
          e = index(body, closer)
          if (e > 0) { body = substr(body, 1, e - 1); in_block = 0 }
        } else if (kind == "slash") {
          i = index($0, "//")
          j = index($0, "/*")
          if (i == 0 || (j > 0 && j < i)) i = j
          if (i > 0) body = substr($0, i)
        } else if (kind == "block") {
          i = index($0, "/*")
          if (i > 0) body = substr($0, i)
        } else if (kind == "angle") {
          i = index($0, "<!--")
          if (i > 0) body = substr($0, i)
        } else if (kind == "sigil") {
          i = index($0, hash)
          if (i > 0) body = substr($0, i)
        } else {
          i = index($0, "--")
          if (i > 0) body = substr($0, i)
        }
        if (body == "") next
        if (match(body, re))
          printf "%s\t%d\t%s\n", FILENAME, FNR, substr(body, RSTART, RLENGTH)
      }
    ' "${sources[@]}"
  )
fi

declare -A narration_count=()
declare -A narration_detail=()
narration_total=0

while IFS=$'\t' read -r file line phrase; do
  [ -n "${file:-}" ] || continue
  narration_count["$file"]=$(( ${narration_count["$file"]:-0} + 1 ))
  narration_detail["$file"]+="    $file:$line: \"$phrase\"
"
  narration_total=$((narration_total + 1))
done <<< "$narration"

declare -A narration_budget=()
while IFS= read -r entry || [ -n "$entry" ]; do
  case "$entry" in '' | "$NARRATION_HASH"*) continue ;; esac
  count=${entry%%$'\t'*}
  path=${entry#*$'\t'}
  if [ "$count" = "$entry" ] || [ -z "$path" ] || ! [ "$count" -ge 1 ] 2>/dev/null; then
    printf '%s: malformed entry "%s" — expected <count><TAB><path>, count 1 or more.\n' \
      "$NARRATION_ALLOWLIST" "$entry"
    printf '    A file with none left has no line here; delete it.\n'
    exit 1
  fi
  narration_budget["$path"]=$count
done < "$NARRATION_ALLOWLIST"

narration_failures=0

# A file over its budget, listed or not. Sorted, so the report reads the same
# on every run.
while IFS= read -r file; do
  [ -n "$file" ] || continue
  found=${narration_count["$file"]}
  budget=${narration_budget["$file"]:-0}
  if [ "$found" -le "$budget" ]; then
    continue
  fi
  narration_failures=$((narration_failures + 1))
  if [ "$budget" -eq 0 ]; then
    printf '%s: %d comment line(s) narrate change history rather than behaviour:\n' \
      "$file" "$found"
  else
    printf '%s: %d narrating comment line(s), %d allowed — the file may not gain more:\n' \
      "$file" "$found" "$budget"
  fi
  printf '%s' "${narration_detail["$file"]}"
  printf '    Say what the code does now. The history belongs in the commit message.\n'
  printf '    If the line is not a comment at all, see the false-positive classes in %s.\n' \
    "$(basename "$0")"
done < <(printf '%s\n' "${!narration_count[@]}" | sort)

# The other direction: an entry that is no longer earned. Without this the
# budget survives the comment it was granted for and can be spent again by the
# next edit, which is how an allowlist stops shrinking.
while IFS= read -r file; do
  [ -n "$file" ] || continue
  budget=${narration_budget["$file"]}
  found=${narration_count["$file"]:-0}
  if [ ! -e "$file" ]; then
    narration_failures=$((narration_failures + 1))
    printf '%s: listed in %s but not in the tree — delete the line.\n' \
      "$file" "$NARRATION_ALLOWLIST"
    continue
  fi
  if [ "$found" -lt "$budget" ]; then
    narration_failures=$((narration_failures + 1))
    printf '%s: %s allows %d narrating comment line(s), the file has %d.\n' \
      "$file" "$NARRATION_ALLOWLIST" "$budget" "$found"
    if [ "$found" -eq 0 ]; then
      printf '    Delete the line: the allowlist is a ratchet and this file is clean.\n'
    else
      printf '    Lower the count to %d so the budget cannot be spent again.\n' "$found"
    fi
  fi
done < <(printf '%s\n' "${!narration_budget[@]}" | sort)

if [ "$narration_failures" -gt 0 ]; then
  echo
  echo "check-doc-pointers: $narration_failures file(s) with unallowed history narration."
else
  echo "check-doc-pointers: ${#sources[@]} source file(s) scanned, $narration_total" \
    "narrating comment line(s), all within the allowlist."
fi

if [ "$failures" -gt 0 ] || [ "$narration_failures" -gt 0 ]; then
  exit 1
fi
