#!/bin/bash
#
# audit-wrap-grants.sh — static-analysis WRAP-grant coverage for impresspress-core
#
# Walks every `db::{list,create,update,delete,count,get,find_one}` callsite
# in `crates/impresspress-core/src/blocks/`, derives the calling block from the
# file path and the table-owning block from the table's `{org}__{block}__`
# prefix, and verifies the owning block declares a `ResourceGrant` covering
# the call.
#
# Background: WRAP enforces cross-block table access at runtime, but only
# when the calling site routes through the typed `db::*` client AND the
# owning block's `BlockInfo::grants` contains a matching `ResourceGrant`.
# Render-function unit tests don't exercise the call path, so missing
# grants ship to main green and only surface as 500s in production.
#
# This script catches the static gap in CI before the bug ships. Two such
# gaps were found at PR-time in May 2026 (PR #75 + PR #77) — both took
# multiple commits to land because the failure was discovered in CI not
# code review.
#
# Out of scope:
#   - Raw SQL paths (`db::query_raw`, `db::exec_raw`) — admin-only by design.
#   - HTTP-style cross-block calls (`ctx.call_block_buffered(...)`) — they
#     don't set `wrap.resource` meta, so WRAP doesn't gate them today.
#     That's a separate design question; see PR #81 description.
#   - Storage grants are audited in Phase 3.5 (added 2026-05-29).
#   - Network grants are NOT audited. impresspress/blocks/admin/mod.rs declares a
#     default-allow `ResourceGrant::read("*", "*").typed(Network)`, which makes
#     a meaningful "missing grant" finding impossible until the policy is
#     locked down. See docs/superpowers/specs/2026-05-29-wave-23-wrap-audit-
#     storage-design.md for the deferred-Network rationale.
#   - Tables outside the `{org}__{block}__` convention (none currently exist
#     in impresspress-core but flagged if found).
#
# Pragmas to silence individual findings (use sparingly, always with a reason):
#   // audit-allow: <reason>          — preceding line: skips one db::* callsite
#   // audit-allow-file: <reason>     — top of file (first 30 lines): skips all
#                                       db::* callsites in that file
#
# Use cases: legacy migrations probing renamed-block tables, generic helpers
# whose tables are passed in by callers, runtime-built table names. Reason is
# required after the colon — the audit isn't supposed to be silenced silently.

set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

BLOCKS_DIR="crates/impresspress-core/src/blocks"

if [ ! -d "$BLOCKS_DIR" ]; then
  echo "::error::$BLOCKS_DIR not found — run from a impresspress repo root."
  exit 2
fi

# ---------- Phase 1: collect all string constants ----------
# Two const shapes used in impresspress-core:
#   pub const TABLE: &str = "wafer_run__auth__users";
#   const FOO_COLLECTION: &str = "impresspress__foo__bar";
#   pub const AUTH_BLOCK_ID: &str = "wafer-run/auth";
#
# We index ALL of them globally by bare name. When a callsite refers to
# `module::TABLE`, we keep only the trailing identifier (`TABLE`) — the
# global map is keyed by the bare name. Collisions are rare in practice
# (only `TABLE` collides across auth/repo/* modules, but they all have the
# same convention so we record per-(file, name) and look up with a
# file-scope-aware fallback for `TABLE`).

declare -A CONST_VALUE          # bare_name -> value (latest seen — global fallback only)
declare -A CONST_AMBIGUOUS      # bare_name -> 1 when two files define the same bare name with DIFFERENT values; the global fallback must refuse these instead of returning whichever file grep happened to visit last
declare -A FILE_CONST_VALUE     # "${file}::${bare_name}" -> value (per-file definitions)
declare -A FILE_CONST_NAMES     # file -> space-separated list of locally-defined names
declare -A FILE_USE_ALIAS       # "${file}::${alias}" -> source_name (from `use ... as alias`)
declare -A FILE_USE_BLOCK       # "${file}::${alias}" -> block_name (path of the `use` statement, when known)
declare -A SIBLING_CONST        # "${dir}::${name}" -> value (for `super::NAME` lookups)
declare -A BLOCK_CONST          # "${block_name}::${name}" -> value (disambiguates colliding bare names across blocks)
declare -A MODULE_REEXPORT      # "${block_name}::${alias}" -> value (from `pub use ... as alias` in any file of the block)
declare -A FILE_USE_VALUE       # "${file}::${alias}" -> directly-resolved value (when Phase 1.6 could resolve the `use` target deterministically; skips the ambiguous bare-name fallback for cases like `use repo::users::TABLE as USERS_TABLE` where multiple files declare `pub const TABLE`)

# Strip the absolute prefix from a file path to get the block directory name.
# e.g. crates/impresspress-core/src/blocks/admin/mod.rs   -> admin
#      crates/impresspress-core/src/blocks/admin/pages/x.rs -> admin
#      crates/impresspress-core/src/blocks/network.rs    -> network
file_to_block_name() {
  local path="$1"
  local rel="${path#$BLOCKS_DIR/}"
  local first="${rel%%/*}"
  echo "${first%.rs}"
}

while IFS= read -r line; do
  # Format: file:lineno:    pub const NAME: &str = "VALUE";
  file="${line%%:*}"
  rest="${line#*:}"
  rest="${rest#*:}"
  re_const='const[[:space:]]+([A-Z_]+)[[:space:]]*:[[:space:]]*&str[[:space:]]*=[[:space:]]*"([^"]+)"'
  if [[ "$rest" =~ $re_const ]]; then
    name="${BASH_REMATCH[1]}"
    value="${BASH_REMATCH[2]}"
    if [ -n "${CONST_VALUE[$name]:-}" ] && [ "${CONST_VALUE[$name]}" != "$value" ]; then
      CONST_AMBIGUOUS["$name"]=1
    fi
    CONST_VALUE["$name"]="$value"
    FILE_CONST_VALUE["${file}::${name}"]="$value"
    FILE_CONST_NAMES["$file"]="${FILE_CONST_NAMES[$file]:-} $name"
    # Index the const at the directory level so siblings can look it up via `super::NAME`.
    dir="$(dirname "$file")"
    SIBLING_CONST["${dir}::${name}"]="$value"
    # Also index by block name so `crate::blocks::BLOCK::NAME` lookups
    # disambiguate when the same bare name exists in multiple blocks
    # (e.g. `VARIABLES_COLLECTION` exists in both admin and products).
    block_name="$(file_to_block_name "$file")"
    BLOCK_CONST["${block_name}::${name}"]="$value"
  fi
# Anchor at start-of-line so we skip function-scoped constants like
#   `        const PROVIDERS_COLLECTION = "..."`
# inside a fn body (typically migration helpers). Module-level consts always
# start at column 0 in this codebase. Visibility modifier may be `pub`,
# `pub(crate)`, `pub(super)`, etc.
done < <(grep -rEn "^(pub(\([^)]+\))?[[:space:]]+)?const [A-Z_]+: &str = \"[^\"]+\"" "$BLOCKS_DIR" 2>/dev/null || true)

# Parse `use ... as` aliases. Both crate-rooted and super-relative paths
# matter — many files import `use crate::blocks::auth::USERS_COLLECTION as USERS`.
# We don't model module scope precisely; we just record alias → source-bare-name
# and rely on the source name being globally unique (most `_COLLECTION`
# constants are; `COLLECTION` and `TABLE` are not — that's the case the
# alias map specifically resolves).
#
# Forms covered:
#   use super::FOO as BAR;
#   use super::{FOO as BAR, BAZ};
#   use crate::blocks::PATH::FOO as BAR;
#   use crate::blocks::PATH::{FOO as BAR, BAZ as QUX};
#   use self::PATH::{FOO as BAR};

re_use_simple_as='use[[:space:]]+(super|self|crate)::[A-Za-z_:]+::([A-Z_]+)[[:space:]]+as[[:space:]]+([A-Z_]+)'
re_use_super_simple='use[[:space:]]+super::([A-Z_]+)[[:space:]]+as[[:space:]]+([A-Z_]+)'
re_use_brace_item='([A-Z_]+)[[:space:]]+as[[:space:]]+([A-Z_]+)'

while IFS= read -r line; do
  file="${line%%:*}"
  rest="${line#*:}"
  rest="${rest#*:}"
  # Simple `use ROOT::PATH::NAME as ALIAS;`
  if [[ "$rest" =~ $re_use_simple_as ]]; then
    src="${BASH_REMATCH[2]}"
    alias="${BASH_REMATCH[3]}"
    FILE_USE_ALIAS["${file}::${alias}"]="$src"
    continue
  fi
  # `use super::NAME as ALIAS;` (no further path segments)
  if [[ "$rest" =~ $re_use_super_simple ]]; then
    src="${BASH_REMATCH[1]}"
    alias="${BASH_REMATCH[2]}"
    FILE_USE_ALIAS["${file}::${alias}"]="$src"
    continue
  fi
  # Brace form: pull out every `X as Y` substring inside the outermost `{...}`.
  if [[ "$rest" == *"{"*"as"*"}"* ]]; then
    inner="${rest#*\{}"
    inner="${inner%%\}*}"
    IFS=',' read -ra items <<< "$inner"
    for item in "${items[@]}"; do
      item="$(echo "$item" | xargs)"  # trim
      if [[ "$item" =~ $re_use_brace_item ]]; then
        src="${BASH_REMATCH[1]}"
        alias="${BASH_REMATCH[2]}"
        FILE_USE_ALIAS["${file}::${alias}"]="$src"
      fi
    done
  fi
done < <(grep -rEn "^use[[:space:]]" "$BLOCKS_DIR" 2>/dev/null || true)

# Catch nested-brace `use crate::{ blocks::{ auth::{ FOO_COLLECTION as FOO } } };`
# patterns that span multiple lines. The grep above matches only the `use ` line
# itself, missing the alias on a continuation line. The pattern `\bX as Y\b`
# where both X and Y are SCREAMING_SNAKE is unambiguous in this codebase.
# Use grep -oE per-file to extract every match (a single line can contain
# multiple alias pairs comma-separated inside one brace import).
while IFS= read -r file; do
  # First pass: pick up path-qualified aliases like `BLOCK::NAME as ALIAS`.
  # These also record the source block so the resolver disambiguates against
  # bare-name collisions across blocks. The pattern matches both:
  #   `blocks::admin::VARIABLES_COLLECTION as VARIABLES`  (single-line)
  #   `        admin::VARIABLES_COLLECTION as VARIABLES,` (nested brace,
  #     `blocks::` is on a previous line)
  # We accept the second by matching just `BLOCK::NAME as ALIAS` and
  # verifying BLOCK is a real block (has BLOCK_CONST entries).
  while IFS= read -r match; do
    [ -z "$match" ] && continue
    if [[ "$match" =~ ^([a-z_]+)::([A-Z_]{4,})[[:space:]]+as[[:space:]]+([A-Z_]{2,})$ ]]; then
      src_block="${BASH_REMATCH[1]}"
      src="${BASH_REMATCH[2]}"
      alias="${BASH_REMATCH[3]}"
      # Only accept if `src_block::src` is known — filters out non-block
      # path segments like `super::FOO as BAR`.
      if [ -n "${BLOCK_CONST[${src_block}::${src}]:-}" ]; then
        FILE_USE_ALIAS["${file}::${alias}"]="$src"
        FILE_USE_BLOCK["${file}::${alias}"]="$src_block"
      fi
    fi
  done < <(grep -oE "[a-z_]+::[A-Z_]{4,}[[:space:]]+as[[:space:]]+[A-Z_]{2,}" "$file" 2>/dev/null || true)
  # Second pass: bare `X as Y` for everything else (super::-style, simple aliases).
  while IFS= read -r match; do
    [ -z "$match" ] && continue
    if [[ "$match" =~ ^([A-Z_]{4,})[[:space:]]+as[[:space:]]+([A-Z_]{2,})$ ]]; then
      src="${BASH_REMATCH[1]}"
      alias="${BASH_REMATCH[2]}"
      # Don't overwrite a path-qualified entry from the first pass.
      if [ -z "${FILE_USE_ALIAS[${file}::${alias}]:-}" ] && [ -n "${CONST_VALUE[$src]:-}" ]; then
        FILE_USE_ALIAS["${file}::${alias}"]="$src"
      fi
    fi
  done < <(grep -oE "[A-Z_]{4,}[[:space:]]+as[[:space:]]+[A-Z_]{2,}" "$file" 2>/dev/null || true)
done < <(find "$BLOCKS_DIR" -name '*.rs' 2>/dev/null)

# ---------- Phase 1.6: re-exports + multi-line brace imports ----------
# After Cleanup A (May 2026) every `auth/repo/*.rs` declares `pub const TABLE`.
# Mod files re-export those under unique aliases:
#   pub(crate) use repo::users::TABLE as USERS_TABLE;
# Consumers then refer to the alias either fully-qualified
# (`crate::blocks::auth::USERS_TABLE`) or via brace import
# (`use crate::blocks::auth::{TOKENS_TABLE, USERS_TABLE}`).
#
# Phase 1.5 above is line-based: it misses multi-line braces and the
# non-aliased brace items (`{TOKENS_TABLE, USERS_TABLE}` has no `as`).
# Phase 1.6 fills both gaps by reading entire `use ...;` statements
# (multi-line aware) and resolving paths to their target files.

# Print every `use ...;` statement in $file as a single line, with internal
# whitespace collapsed. Handles multi-line brace forms.
read_use_statements() {
  awk '
    /^[[:space:]]*(pub(\([^)]+\))?[[:space:]]+)?use[[:space:]]/ {
      buf = $0
      while (buf !~ /;/) {
        if ((getline next_line) <= 0) break
        buf = buf " " next_line
      }
      gsub(/[[:space:]]+/, " ", buf)
      sub(/^ /, "", buf)
      print buf
    }
  ' "$1"
}

# Walk a Rust module path (e.g. "repo::users", "super::auth::repo::users",
# "crate::blocks::auth::repo::users") from $start_file's module location to
# the target .rs file. Returns empty string if the file doesn't exist.
resolve_module_path() {
  local start_file="$1"
  local path="$2"
  local start_dir
  start_dir="$(dirname "$start_file")"

  if [[ "$path" == crate::blocks::* ]]; then
    path="${path#crate::blocks::}"
    start_dir="$BLOCKS_DIR"
  elif [[ "$path" == crate::* ]]; then
    # crate:: outside blocks/ is uncommon for the names we care about; bail.
    echo ""; return
  fi
  # `super` walks one module level up. For a regular `foo.rs`, the file's
  # module is `foo` inside `dir`, so `super` = `dir` = the file's own
  # directory — no `dirname` needed. For `mod.rs`, the file's module IS
  # `dir`, so `super` = parent of `dir` — one `dirname` needed. The script's
  # `start_dir` is `dirname(start_file)` (= `dir` in both cases), so:
  #   * regular file: skip dirname on the FIRST `super::` only
  #   * mod.rs: dirname on every `super::`
  local skip_first_dirname=0
  if [[ "$(basename "$start_file")" != "mod.rs" ]]; then
    skip_first_dirname=1
  fi
  while [[ "$path" == super::* ]]; do
    if [ "$skip_first_dirname" -eq 1 ]; then
      skip_first_dirname=0
    else
      start_dir="$(dirname "$start_dir")"
    fi
    path="${path#super::}"
  done
  if [[ "$path" == self::* ]]; then
    path="${path#self::}"
  fi
  # Strip the trailing `::` segment if any const-only path snuck through.
  path="${path%::}"
  local fs_path="${path//::/\/}"

  if [ -z "$fs_path" ]; then
    [ -f "$start_dir/mod.rs" ] && { echo "$start_dir/mod.rs"; return; }
    echo ""; return
  fi
  [ -f "$start_dir/$fs_path.rs" ] && { echo "$start_dir/$fs_path.rs"; return; }
  [ -f "$start_dir/$fs_path/mod.rs" ] && { echo "$start_dir/$fs_path/mod.rs"; return; }
  echo ""
}

# Look up the value of $const_name in $target_file: prefer the file's own
# definition, then chase one level of re-export through MODULE_REEXPORT.
# Returns empty if neither has it.
lookup_const_in_file() {
  local target_file="$1" const_name="$2"
  local v="${FILE_CONST_VALUE[${target_file}::${const_name}]:-}"
  if [ -n "$v" ]; then echo "$v"; return; fi
  local target_block
  target_block="$(file_to_block_name "$target_file")"
  echo "${MODULE_REEXPORT[${target_block}::${const_name}]:-}"
}

# Parse a `use` statement body (everything between `use ` and `;`) into
# leaf entries. Each entry is printed on its own line in the form:
#   <source_path>|<alias>
# where source_path is the full module path (possibly empty for bare names)
# and alias is the local name the entry binds.
#
# Handles arbitrarily nested brace forms by recursing on `{...}` groups and
# concatenating the path prefix collected so far with each leaf:
#   use crate::{ blocks::{ auth::{X, Y as Z} } };
#     → crate::blocks::auth::X|X
#       crate::blocks::auth::Y|Z
explode_use_body() {
  local body="$1"
  body="${body# }"; body="${body% }"
  _explode_use_recur "" "$body"
}

_explode_use_recur() {
  local prefix="$1" content="$2"
  local n=${#content}
  local depth=0 i=0 ch run=""
  while [ "$i" -lt "$n" ]; do
    ch="${content:$i:1}"
    if [ "$ch" = "{" ]; then
      # `run` so far is the path before the brace. Find matching `}`.
      local before="${run# }"; before="${before% }"
      local combined_prefix
      if [ -z "$prefix" ]; then
        combined_prefix="$before"
      else
        combined_prefix="${prefix}${before}"
      fi
      depth=1
      local j=$((i + 1))
      while [ "$j" -lt "$n" ] && [ "$depth" -gt 0 ]; do
        local c2="${content:$j:1}"
        if [ "$c2" = "{" ]; then
          depth=$((depth + 1))
        elif [ "$c2" = "}" ]; then
          depth=$((depth - 1))
        fi
        [ "$depth" -gt 0 ] && j=$((j + 1))
      done
      local inner_len=$((j - i - 1))
      local inner="${content:$((i + 1)):$inner_len}"
      _explode_use_recur "$combined_prefix" "$inner"
      run=""
      i=$((j + 1))
      continue
    fi
    if [ "$ch" = "," ]; then
      _emit_use_leaf "$prefix" "$run"
      run=""
    else
      run="${run}${ch}"
    fi
    i=$((i + 1))
  done
  if [ -n "${run// /}" ]; then
    _emit_use_leaf "$prefix" "$run"
  fi
}

_emit_use_leaf() {
  local prefix="$1" item="$2"
  item="${item# }"; item="${item% }"
  [ -z "$item" ] && return
  local src alias
  if [[ "$item" =~ ^(.+)[[:space:]]+as[[:space:]]+([A-Za-z_][A-Za-z0-9_]*)$ ]]; then
    src="${BASH_REMATCH[1]}"
    alias="${BASH_REMATCH[2]}"
    src="${src% }"
  else
    src="$item"
    alias="${item##*::}"
  fi
  local full_src
  if [ -n "$prefix" ]; then
    full_src="${prefix}${src}"
  else
    full_src="$src"
  fi
  echo "${full_src}|${alias}"
}

while IFS= read -r file; do
  file_block="$(file_to_block_name "$file")"
  while IFS= read -r stmt; do
    [ -z "$stmt" ] && continue
    is_pub=0
    [[ "$stmt" == pub* ]] && is_pub=1
    # Strip `pub(...)? use ` prefix and trailing `;`.
    body="${stmt#*use }"
    body="${body%;*}"
    while IFS= read -r entry; do
      [ -z "$entry" ] && continue
      src_path="${entry%%|*}"
      alias="${entry#*|}"
      # Only consider SCREAMING_SNAKE_CASE aliases — those are our table consts.
      [[ "$alias" =~ ^[A-Z][A-Z0-9_]*$ ]] || continue
      # Source path must end in the actual const name.
      src_const="${src_path##*::}"
      [[ "$src_const" =~ ^[A-Z][A-Z0-9_]*$ ]] || continue
      module_part="${src_path%::*}"
      [ "$module_part" = "$src_path" ] && module_part=""

      # Resolve the source path to a target file.
      target_file=""
      if [ -n "$module_part" ]; then
        target_file="$(resolve_module_path "$file" "$module_part")"
      fi

      # 1) Populate FILE_USE_ALIAS / FILE_USE_BLOCK so resolve_token can
      #    chase qualified imports like `use crate::blocks::auth::{TOKENS_TABLE, USERS_TABLE}`.
      if [ -z "${FILE_USE_ALIAS[${file}::${alias}]:-}" ] && [ -n "$src_const" ]; then
        FILE_USE_ALIAS["${file}::${alias}"]="$src_const"
        if [ -n "$target_file" ]; then
          src_block="$(file_to_block_name "$target_file")"
          FILE_USE_BLOCK["${file}::${alias}"]="$src_block"
        fi
      fi

      # 2) If we can resolve src_path to a specific file's const right now,
      #    cache the value directly. This bypasses the ambiguous bare-name
      #    BLOCK_CONST fallback for cases like `pub use repo::users::TABLE as USERS_TABLE`
      #    where 10+ files declare `pub const TABLE` and the bare key collides.
      if [ -n "$target_file" ]; then
        value="$(lookup_const_in_file "$target_file" "$src_const")"
        if [ -n "$value" ]; then
          FILE_USE_VALUE["${file}::${alias}"]="$value"
        fi
      fi

      # 3) For `pub use ...` re-exports, populate MODULE_REEXPORT so callers
      #    referencing `${file_block}::${alias}` can resolve.
      if [ "$is_pub" -eq 1 ] && [ -n "$target_file" ]; then
        value="$(lookup_const_in_file "$target_file" "$src_const")"
        if [ -n "$value" ]; then
          MODULE_REEXPORT["${file_block}::${alias}"]="$value"
        fi
      fi
    done < <(explode_use_body "$body")
  done < <(read_use_statements "$file")
done < <(find "$BLOCKS_DIR" -name '*.rs' 2>/dev/null)

# Second pass through `pub use` statements: chain re-exports. If A's mod.rs
# re-exports a name from B's mod.rs (which is itself a re-export from B's
# repo file), the first pass populated B's entry but not A's because B's
# entry hadn't been computed yet when A was processed. Loop until stable.
for _ in 1 2 3; do
  changed=0
  while IFS= read -r file; do
    file_block="$(file_to_block_name "$file")"
    while IFS= read -r stmt; do
      [ -z "$stmt" ] && continue
      [[ "$stmt" == pub* ]] || continue
      body="${stmt#*use }"
      body="${body%;*}"
      while IFS= read -r entry; do
        [ -z "$entry" ] && continue
        src_path="${entry%%|*}"
        alias="${entry#*|}"
        [[ "$alias" =~ ^[A-Z][A-Z0-9_]*$ ]] || continue
        [ -n "${MODULE_REEXPORT[${file_block}::${alias}]:-}" ] && continue
        src_const="${src_path##*::}"
        [[ "$src_const" =~ ^[A-Z][A-Z0-9_]*$ ]] || continue
        module_part="${src_path%::*}"
        [ "$module_part" = "$src_path" ] && module_part=""
        [ -z "$module_part" ] && continue
        target_file="$(resolve_module_path "$file" "$module_part")"
        [ -z "$target_file" ] && continue
        value="$(lookup_const_in_file "$target_file" "$src_const")"
        if [ -n "$value" ]; then
          MODULE_REEXPORT["${file_block}::${alias}"]="$value"
          changed=1
        fi
      done < <(explode_use_body "$body")
    done < <(read_use_statements "$file")
  done < <(find "$BLOCKS_DIR" -name '*.rs' 2>/dev/null)
  [ "$changed" -eq 0 ] && break
done

# ---------- Phase 2: collect grants per-owning-block ----------
# Pattern:  ResourceGrant::{read,read_write}(GRANTEE, RESOURCE)[.typed(TYPE)]
# Grants live in a block's `BlockInfo::grants(vec![...])` — we attribute the
# grant to the file's owning block (the directory or .rs filename under
# blocks/).
#
# Each grant entry is encoded as:
#   "${owner_block_id}|${grantee}|${resource}|${type}"
# where TYPE is "Database" by default or whatever appears after .typed().

GRANTS=()

# Resolve a token (constant name, possibly-qualified path, or string literal)
# to its actual table-name string. File-aware: tokens are resolved against
# the file's local consts and `use ... as` aliases first, then the global
# CONST_VALUE map as last resort.
#
# Examples (from a callsite in `blocks/admin/logs.rs`):
#   "literal_name"            -> literal_name
#   COLLECTION                -> resolved via file-local + use-alias chain
#   crate::FOO::BAR           -> bare BAR via global fallback
#   super::USERS_COLLECTION   -> bare USERS_COLLECTION via parent-dir SIBLING_CONST
resolve_token() {
  local tok="$1"
  local file="$2"
  if [[ "$tok" =~ ^\"(.+)\"$ ]]; then
    echo "${BASH_REMATCH[1]}"
    return
  fi
  # Stripped bare identifier (drop `module::path::` prefix).
  local bare="${tok##*::}"
  # 0a. `crate::blocks::BLOCK::NAME` — full path. Disambiguates colliding
  #     bare names across blocks (e.g. `VARIABLES_COLLECTION` exists in
  #     both admin and products). NAME may be a direct const in BLOCK or a
  #     `pub use ... as NAME` re-export from BLOCK's mod.rs.
  if [[ "$tok" =~ blocks::([a-z_]+)::([A-Z_]+)$ ]]; then
    local qblock="${BASH_REMATCH[1]}"
    local qname="${BASH_REMATCH[2]}"
    if [ -n "${BLOCK_CONST[${qblock}::${qname}]:-}" ]; then
      echo "${BLOCK_CONST[${qblock}::${qname}]}"
      return
    fi
    if [ -n "${MODULE_REEXPORT[${qblock}::${qname}]:-}" ]; then
      echo "${MODULE_REEXPORT[${qblock}::${qname}]}"
      return
    fi
  fi
  # 0b. `super::SIBLING::NAME` from a top-level block file — `super` exits
  #     to the `blocks/` parent, then `SIBLING` enters the named sibling
  #     block. Used by grant declarations like
  #     `ResourceGrant::read(super::auth::AUTH_BLOCK_ID, ...)`.
  if [[ "$tok" =~ ^super::([a-z_]+)::([A-Z_]+)$ ]]; then
    local sblock="${BASH_REMATCH[1]}"
    local sname="${BASH_REMATCH[2]}"
    if [ -n "${BLOCK_CONST[${sblock}::${sname}]:-}" ]; then
      echo "${BLOCK_CONST[${sblock}::${sname}]}"
      return
    fi
    if [ -n "${MODULE_REEXPORT[${sblock}::${sname}]:-}" ]; then
      echo "${MODULE_REEXPORT[${sblock}::${sname}]}"
      return
    fi
  fi
  # 0c. Qualified module path whose final module segment is lowercase
  #     (e.g. `repo::offers::TABLE`, `super::repo::purchases::TABLE`). Walk
  #     the module path from this file to the defining .rs file and read the
  #     const there. Rust callsites often name a module brought into scope by
  #     a `use` of an ANCESTOR module (`repo::offers::TABLE` inside
  #     `handlers/*.rs` where `repo/` sits at the block root), so when the
  #     file-relative walk misses, retry from each ancestor directory up to
  #     the blocks root. A path that reaches a real file which lacks the
  #     const stays unresolved — never fall through to the bare-name maps,
  #     which are ambiguous for `TABLE`.
  if [[ "$tok" == *::* && "$tok" != Self::* && "$tok" != self::* ]]; then
    local qmodule="${tok%::*}"
    if [[ "$bare" =~ ^[A-Z][A-Z0-9_]*$ ]] && [[ "$qmodule" =~ (^|::)[a-z_]+$ ]]; then
      local qfile
      qfile="$(resolve_module_path "$file" "$qmodule")"
      if [ -z "$qfile" ] && [[ "$qmodule" != crate::* && "$qmodule" != super::* ]]; then
        local qdir qfs
        qdir="$(dirname "$file")"
        qfs="${qmodule//::/\/}"
        while [[ "$qdir" == "$BLOCKS_DIR"* ]]; do
          if [ -f "$qdir/$qfs.rs" ]; then qfile="$qdir/$qfs.rs"; break; fi
          if [ -f "$qdir/$qfs/mod.rs" ]; then qfile="$qdir/$qfs/mod.rs"; break; fi
          qdir="$(dirname "$qdir")"
        done
      fi
      if [ -n "$qfile" ]; then
        local qval
        qval="$(lookup_const_in_file "$qfile" "$bare")"
        if [ -n "$qval" ]; then
          echo "$qval"
          return
        fi
        echo "<unresolved:$tok>"
        return
      fi
    fi
  fi
  # 1. Per-file definition.
  if [ -n "${FILE_CONST_VALUE[${file}::${bare}]:-}" ]; then
    echo "${FILE_CONST_VALUE[${file}::${bare}]}"
    return
  fi
  # 1.5. Per-file `use` alias with a pre-resolved value. Phase 1.6 caches
  #      this when it can walk the use path to a specific target file. Wins
  #      over the bare-name BLOCK_CONST lookup below, which is ambiguous for
  #      `TABLE` (10+ auth/repo/*.rs all declare `pub const TABLE`).
  if [ -n "${FILE_USE_VALUE[${file}::${bare}]:-}" ]; then
    echo "${FILE_USE_VALUE[${file}::${bare}]}"
    return
  fi
  # 2. Per-file `use ... as` alias — if the alias was indexed with a
  #    specific source block, prefer that. Otherwise chase to the source
  #    name through sibling modules, then global.
  if [ -n "${FILE_USE_ALIAS[${file}::${bare}]:-}" ]; then
    local src="${FILE_USE_ALIAS[${file}::${bare}]}"
    if [ -n "${FILE_USE_BLOCK[${file}::${bare}]:-}" ]; then
      local src_block="${FILE_USE_BLOCK[${file}::${bare}]}"
      if [ -n "${BLOCK_CONST[${src_block}::${src}]:-}" ]; then
        echo "${BLOCK_CONST[${src_block}::${src}]}"
        return
      fi
      if [ -n "${MODULE_REEXPORT[${src_block}::${src}]:-}" ]; then
        echo "${MODULE_REEXPORT[${src_block}::${src}]}"
        return
      fi
    fi
    local parent_dir="$(dirname "$file")"
    if [ -n "${SIBLING_CONST[${parent_dir}::${src}]:-}" ]; then
      echo "${SIBLING_CONST[${parent_dir}::${src}]}"
      return
    fi
    if [ -n "${CONST_VALUE[$src]:-}" ]; then
      echo "${CONST_VALUE[$src]}"
      return
    fi
  fi
  # 3. `super::NAME` reference (no rename) — look in the parent dir.
  if [[ "$tok" =~ ^super:: ]]; then
    local parent_dir="$(dirname "$file")"
    if [ -n "${SIBLING_CONST[${parent_dir}::${bare}]:-}" ]; then
      echo "${SIBLING_CONST[${parent_dir}::${bare}]}"
      return
    fi
  fi
  # 4. Re-export brought into scope by `use super::{NAME}` (or by a brace
  #    import from this file's own block's mod.rs). For non-mod files in
  #    block X, `super::` resolves to X's mod.rs — so consult MODULE_REEXPORT
  #    keyed on this file's block.
  local file_block
  file_block="$(file_to_block_name "$file")"
  if [ -n "${MODULE_REEXPORT[${file_block}::${bare}]:-}" ]; then
    echo "${MODULE_REEXPORT[${file_block}::${bare}]}"
    return
  fi
  # 5. Global fallback — only safe if the bare name is unambiguous across
  #    the codebase. Used for grant declarations (top-level scope) where
  #    file-aware lookup isn't a fit. Names defined with different values in
  #    different files (`TABLE`) must stay unresolved here: returning
  #    whichever definition grep visited last fabricates cross-block
  #    references that fail the audit spuriously.
  if [ -n "${CONST_VALUE[$bare]:-}" ] && [ -z "${CONST_AMBIGUOUS[$bare]:-}" ]; then
    echo "${CONST_VALUE[$bare]}"
    return
  fi
  echo "<unresolved:$tok>"
}

# Block-directory name → registered block id, for the handful of blocks whose
# id is NOT `impresspress/{dir}`. The runtime de-leak (2026-07) moved the auth
# *service* into the runtime's own namespace: the code under `blocks/auth/`
# registers as `wafer-run/auth` (see `AUTH_BLOCK_ID` in `blocks/auth/mod.rs`)
# and owns the `wafer_run__auth__*` tables. Attributing both its `db::*`
# callsites and its `ResourceGrant` decls to that id is what makes its
# own-table access read as OWN and its cross-block grants match the real
# table owner — without this, every auth-table access is a false "missing
# grant". Const resolution still keys off the raw dir name (`file_to_block_name`
# is deliberately left untouched).
declare -A BLOCK_ID_OVERRIDE=(
  [auth]="wafer-run/auth"
)

# Convert a file path like crates/impresspress-core/src/blocks/files/mod.rs or
# crates/impresspress-core/src/blocks/network.rs into the block id
# `impresspress/{name}` (or a `BLOCK_ID_OVERRIDE` entry).
file_to_block_id() {
  local path="$1"
  # Strip the prefix to get blocks/<rest>
  local rel="${path#$BLOCKS_DIR/}"
  # Take the first path segment, stripping trailing .rs
  local first="${rel%%/*}"
  first="${first%.rs}"
  if [ -n "${BLOCK_ID_OVERRIDE[$first]:-}" ]; then
    echo "${BLOCK_ID_OVERRIDE[$first]}"
    return
  fi
  echo "impresspress/${first//_/-}"
}

# Convert a table name (e.g. wafer_run__auth__sessions) to its owner block
# id (wafer-run/auth). Returns empty string if the name doesn't follow
# the {org}__{block}__{rest} convention.
table_to_owner() {
  local table="$1"
  if [[ "$table" =~ ^([a-z0-9_]+)__([a-z0-9_]+)__ ]]; then
    local org="${BASH_REMATCH[1]//_/-}"
    local block="${BASH_REMATCH[2]//_/-}"
    echo "${org}/${block}"
    return
  fi
  echo ""
}

# Convert a storage path (e.g. "impresspress/files/cloud/key.png" or
# "@impresspress/files/foo") to its owner block id (impresspress/files).
# Returns empty string if the path doesn't have at least two slash segments.
storage_path_to_owner() {
  local path="$1"
  # Strip a leading "@" (cross-block resource indicator in source).
  path="${path#@}"
  if [[ "$path" =~ ^([a-z0-9_-]+)/([a-z0-9_-]+)(/|$) ]]; then
    echo "${BASH_REMATCH[1]}/${BASH_REMATCH[2]}"
    return
  fi
  echo ""
}

while IFS= read -r line; do
  file="${line%%:*}"
  rest="${line#*:}"
  rest="${rest#*:}"
  # Match: ResourceGrant::read("a", "b")  or  ResourceGrant::read_write(IDENT, IDENT)
  # The args may be string literals or constant identifiers (with optional `super::module::` qualifier).
  # Bash requires the regex stored in a variable when it contains parens.
  re_grant='ResourceGrant::(read|read_write)\(([^,]+),[[:space:]]*([^)]+)\)'
  if [[ "$rest" =~ $re_grant ]]; then
    kind="${BASH_REMATCH[1]}"
    grantee_raw="${BASH_REMATCH[2]// /}"
    resource_raw="${BASH_REMATCH[3]// /}"
    grantee="$(resolve_token "$grantee_raw" "$file")"
    resource="$(resolve_token "$resource_raw" "$file")"
    # Grant type: default Database; if the same line has .typed(...), pick that
    type="Database"
    re_typed='\.typed\(([^)]*ResourceType::)?([A-Za-z]+)\)'
    if [[ "$rest" =~ $re_typed ]]; then
      type="${BASH_REMATCH[2]}"
    fi
    # Owning block = the block this file lives in
    owner="$(file_to_block_id "$file")"
    GRANTS+=("${owner}|${grantee}|${resource}|${type}|${kind}")
  fi
done < <(grep -rEn "ResourceGrant::(read|read_write)\(" "$BLOCKS_DIR" 2>/dev/null || true)

# ---------- Phase 3: walk db::* callsites and check coverage ----------

# Returns "OK" if a grant covers (caller, table); otherwise "MISSING".
# Grant matches when:
#   - resource_type is Database (or the grant's type is empty/wildcard)
#   - grantee == caller OR grantee == "*"
#   - resource == table OR (resource ends with "*" AND table starts with the prefix)
check_coverage() {
  local caller="$1" table="$2"
  local owner
  owner="$(table_to_owner "$table")"
  if [ -z "$owner" ]; then
    echo "NON_CONVENTIONAL"
    return
  fi
  if [ "$caller" = "$owner" ]; then
    echo "OWN"
    return
  fi
  for g in "${GRANTS[@]}"; do
    IFS='|' read -r g_owner g_grantee g_resource g_type _g_kind <<< "$g"
    [ "$g_owner" != "$owner" ] && continue
    [ "$g_type" != "Database" ] && continue
    if [ "$g_grantee" != "*" ] && [ "$g_grantee" != "$caller" ]; then
      continue
    fi
    if [ "$g_resource" = "$table" ] || [ "$g_resource" = "*" ]; then
      echo "OK"; return
    fi
    # Prefix match: grant resource ends with `*`
    if [[ "$g_resource" == *\* ]]; then
      local prefix="${g_resource%\*}"
      if [[ "$table" == ${prefix}* ]]; then
        echo "OK"; return
      fi
    fi
  done
  echo "MISSING"
}

# Returns "OK" if a typed-Storage grant covers (caller, resource);
# otherwise "MISSING" or "NON_CONVENTIONAL_STORAGE" or "OWN_STORAGE".
# Grant matches when:
#   - g_type == "Storage"
#   - grantee == caller OR grantee == "*"
#   - g_resource == resource OR (g_resource ends with "*" AND resource starts with the prefix)
storage_check_coverage() {
  local caller="$1" resource="$2"
  local owner
  owner="$(storage_path_to_owner "$resource")"
  if [ -z "$owner" ]; then
    echo "NON_CONVENTIONAL_STORAGE"
    return
  fi
  if [ "$caller" = "$owner" ]; then
    echo "OWN_STORAGE"
    return
  fi
  for g in "${GRANTS[@]}"; do
    IFS='|' read -r g_owner g_grantee g_resource g_type _g_kind <<< "$g"
    [ "$g_owner" != "$owner" ] && continue
    [ "$g_type" != "Storage" ] && continue
    if [ "$g_grantee" != "*" ] && [ "$g_grantee" != "$caller" ]; then
      continue
    fi
    if [ "$g_resource" = "$resource" ] || [ "$g_resource" = "*" ]; then
      echo "OK"; return
    fi
    # Prefix match: grant resource ends with `*`
    if [[ "$g_resource" == *\* ]]; then
      local prefix="${g_resource%\*}"
      if [[ "$resource" == ${prefix}* ]]; then
        echo "OK"; return
      fi
    fi
  done
  echo "MISSING"
}

declare -i total=0 missing=0 unresolved=0 nonconv=0 allowed=0
declare -A SEEN_PAIRS
MISSING_LINES=()
UNRESOLVED_LINES=()
NONCONV_LINES=()
ALLOWED_LINES=()

# True if the file has a top-of-file `// audit-allow-file: <reason>` pragma
# in its first 30 lines. Used for pure pass-through helper files (e.g.
# `crud.rs` whose db::* calls all take the table name as a parameter — the
# real audit happens at the callers).
declare -A FILE_ALLOW_CACHE
file_allows_audit_skip() {
  local file="$1"
  if [ -n "${FILE_ALLOW_CACHE[$file]:-}" ]; then
    [ "${FILE_ALLOW_CACHE[$file]}" = "yes" ]
    return $?
  fi
  if head -n 30 "$file" 2>/dev/null | grep -qE "//[[:space:]]*audit-allow-file:[[:space:]]*[^[:space:]]"; then
    FILE_ALLOW_CACHE["$file"]="yes"
    return 0
  fi
  FILE_ALLOW_CACHE["$file"]="no"
  return 1
}

# Returns "yes" if the previous source line in $file (relative to $lineno)
# carries a `// audit-allow:` pragma. Reason after the colon is required.
has_allow_pragma() {
  local file="$1" lineno="$2"
  if [ "$lineno" -lt 2 ]; then
    return 1
  fi
  local prev
  prev="$(sed -n "$((lineno - 1))p" "$file" 2>/dev/null)"
  if [[ "$prev" =~ //[[:space:]]*audit-allow:[[:space:]]*[^[:space:]] ]]; then
    return 0
  fi
  return 1
}

while IFS= read -r line; do
  file="${line%%:*}"
  rest="${line#*:}"
  lineno="${rest%%:*}"
  rest="${rest#*:}"
  # Match db::list(ctx, COLLECTION, ...) — second arg is the table.
  # Permit an optional `&` prefix on the arg. Pattern in a variable for bash regex.
  re_dbcall='db::(list|create|update|delete|count|get|find_one)[[:space:]]*\([[:space:]]*ctx[[:space:]]*,[[:space:]]*&?([A-Za-z_:]+|"[^"]+")[[:space:]]*[,)]'
  if [[ "$rest" =~ $re_dbcall ]]; then
    arg="${BASH_REMATCH[2]}"
    table="$(resolve_token "$arg" "$file")"
    caller="$(file_to_block_id "$file")"
    pair_key="${caller}|${table}"
    [ -n "${SEEN_PAIRS[$pair_key]:-}" ] && continue
    SEEN_PAIRS["$pair_key"]=1
    total=$((total + 1))
    # Honor `// audit-allow: <reason>` (per-line) and `// audit-allow-file: <reason>`
    # (top-of-file) pragmas. Used for legitimate exceptions: legacy migrations
    # probing renamed-block tables, generic helpers whose tables are passed in
    # by callers, runtime-built table names (e.g. vector's per-index `_meta`).
    if file_allows_audit_skip "$file" || has_allow_pragma "$file" "$lineno"; then
      allowed=$((allowed + 1))
      ALLOWED_LINES+=("${file}:${lineno}: ${caller} → ${table}")
      continue
    fi
    if [[ "$table" == "<unresolved:"* ]]; then
      unresolved=$((unresolved + 1))
      UNRESOLVED_LINES+=("${file}:${lineno}: ${caller} → ${table}")
      continue
    fi
    result="$(check_coverage "$caller" "$table")"
    case "$result" in
      OK|OWN) ;;
      MISSING)
        missing=$((missing + 1))
        owner="$(table_to_owner "$table")"
        MISSING_LINES+=("${file}:${lineno}: ${caller} → ${table} (owned by ${owner})")
        ;;
      NON_CONVENTIONAL)
        nonconv=$((nonconv + 1))
        NONCONV_LINES+=("${file}:${lineno}: ${caller} → ${table}")
        ;;
    esac
  fi
done < <(grep -rEn "db::(list|create|update|delete|count|get|find_one)\(" "$BLOCKS_DIR" 2>/dev/null || true)

# ---------- Phase 3.5: walk storage callsites and check coverage ----------
# Mirrors Phase 3 but for typed Storage grants.

declare -i storage_total=0 storage_missing=0 storage_unresolved=0 storage_nonconv=0 storage_allowed=0
declare -A SEEN_PAIRS_STORAGE
STORAGE_MISSING_LINES=()
STORAGE_UNRESOLVED_LINES=()
STORAGE_NONCONV_LINES=()
STORAGE_ALLOWED_LINES=()

while IFS= read -r line; do
  file="${line%%:*}"
  rest="${line#*:}"
  lineno="${rest%%:*}"
  rest="${rest#*:}"
  # Match: clients::storage::<op>(ctx, ARG, …) — the 8 wrapper fns.
  re_storage_call='clients::storage::(get|put|delete|list|create_folder|delete_folder|list_folders|get_stream)[[:space:]]*\([[:space:]]*ctx[[:space:]]*,[[:space:]]*&?([A-Za-z_:][A-Za-z0-9_:.]*|"[^"]+")[[:space:]]*[,)]'
  if [[ "$rest" =~ $re_storage_call ]]; then
    arg="${BASH_REMATCH[2]}"
    resource="$(resolve_token "$arg" "$file")"
    caller="$(file_to_block_id "$file")"
    pair_key="${caller}|${resource}"
    [ -n "${SEEN_PAIRS_STORAGE[$pair_key]:-}" ] && continue
    SEEN_PAIRS_STORAGE["$pair_key"]=1
    storage_total=$((storage_total + 1))
    # Honor `// audit-allow:` and `// audit-allow-file:` pragmas (shared with Phase 3).
    if file_allows_audit_skip "$file" || has_allow_pragma "$file" "$lineno"; then
      storage_allowed=$((storage_allowed + 1))
      STORAGE_ALLOWED_LINES+=("${file}:${lineno}: ${caller} → ${resource}")
      continue
    fi
    if [[ "$resource" == "<unresolved:"* ]]; then
      storage_unresolved=$((storage_unresolved + 1))
      STORAGE_UNRESOLVED_LINES+=("${file}:${lineno}: ${caller} → ${resource}")
      continue
    fi
    result="$(storage_check_coverage "$caller" "$resource")"
    case "$result" in
      OK|OWN_STORAGE) ;;
      MISSING)
        storage_missing=$((storage_missing + 1))
        owner="$(storage_path_to_owner "$resource")"
        STORAGE_MISSING_LINES+=("${file}:${lineno}: ${caller} → ${resource} (owned by ${owner})")
        ;;
      NON_CONVENTIONAL_STORAGE)
        storage_nonconv=$((storage_nonconv + 1))
        STORAGE_NONCONV_LINES+=("${file}:${lineno}: ${caller} → ${resource}")
        ;;
    esac
  fi
done < <(grep -rEn "clients::storage::(get|put|delete|list|create_folder|delete_folder|list_folders|get_stream)\(" "$BLOCKS_DIR" 2>/dev/null || true)

# ---------- Phase 4: report ----------

echo
echo "WRAP grant audit — $(date)"
echo
echo "Indexed: ${#CONST_VALUE[@]} constants, ${#GRANTS[@]} grant decls."
echo "Database: ${total} unique (caller, table) pairs; ${allowed} pragma-allowed."
echo "Storage:  ${storage_total} unique (caller, resource) pairs; ${storage_allowed} pragma-allowed."
echo

if [ "${#MISSING_LINES[@]}" -gt 0 ]; then
  echo "MISSING grants (${missing}):"
  printf '  %s\n' "${MISSING_LINES[@]}"
  echo
fi

if [ "${#UNRESOLVED_LINES[@]}" -gt 0 ]; then
  echo "UNRESOLVED constants (${unresolved}) — needs human review:"
  printf '  %s\n' "${UNRESOLVED_LINES[@]}"
  echo
fi

if [ "${#NONCONV_LINES[@]}" -gt 0 ]; then
  echo "NON-CONVENTIONAL tables (${nonconv}) — owner cannot be derived from name:"
  printf '  %s\n' "${NONCONV_LINES[@]}"
  echo
fi

if [ "${#STORAGE_MISSING_LINES[@]}" -gt 0 ]; then
  echo "MISSING storage grants (${storage_missing}):"
  printf '  %s\n' "${STORAGE_MISSING_LINES[@]}"
  echo
fi

if [ "${#STORAGE_UNRESOLVED_LINES[@]}" -gt 0 ]; then
  echo "UNRESOLVED storage resources (${storage_unresolved}) — needs human review:"
  printf '  %s\n' "${STORAGE_UNRESOLVED_LINES[@]}"
  echo
fi

if [ "${#STORAGE_NONCONV_LINES[@]}" -gt 0 ]; then
  echo "NON-CONVENTIONAL storage resources (${storage_nonconv}) — owner cannot be derived from path:"
  printf '  %s\n' "${STORAGE_NONCONV_LINES[@]}"
  echo
fi

if [ "$missing" -gt 0 ] || [ "$storage_missing" -gt 0 ]; then
  total_missing=$((missing + storage_missing))
  echo "::error::WRAP grant audit found ${total_missing} missing grant(s) (${missing} db + ${storage_missing} storage)."
  exit 1
fi

echo "OK — no missing WRAP grants."
