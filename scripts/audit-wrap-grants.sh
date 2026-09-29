#!/bin/bash
#
# audit-wrap-grants.sh — static-analysis WRAP-grant coverage for impresspress-core
#
# Walks every `db::{list,create,update,delete,count,get,find_one}` callsite
# in `crates/impresspress-core/src/blocks/`, derives the calling block from the
# file path and the table-owning block from the table's `{org}__{block}__`
# prefix, and verifies the owning block declares a `ResourceGrant` covering
# the call. A reference to `crate::platform_state::<module>` from a block is
# walked the same way: that module's functions run under the calling block's
# WRAP identity, so the reference is a database access on the module's
# `TABLE` (Phase 3.7). So are the two references that reach the admin audit
# table through its shared writer — `logs::audit_log` and
# `ui::settings_form::save_settings` (Phase 3.8).
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
#     locked down. Auditing Network only becomes useful once that
#     default-allow grant is narrowed.
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

# The tree this audit walks. Overridable ONLY so the self-check below can
# point a real run at a synthetic fixture; nothing else sets it.
BLOCKS_DIR="${AUDIT_WRAP_GRANTS_FIXTURE_DIR:-crates/impresspress-core/src/blocks}"
# The dev block's guest TEMPLATES are not host code: they are the sources of
# sandbox blocks (`site/<name>`) that Rubrc compiles for wasm32-wasip1 in the
# browser. Their `db::*` calls run inside the guest, as the guest block, and
# reach the database through WRAP under that block's own capabilities (the
# `site__<name>__*` namespace rule in `blocks/dev/validation.rs`) — there is
# no grant for `impresspress/dev` to declare, because `impresspress/dev` is
# not the caller. Attributing them to the dev block by path would flag every
# template as a missing grant, so the walk skips that directory.
GUEST_TEMPLATES_DIR="$BLOCKS_DIR/dev/templates"
# `grep -r` and `find` spell the exclusion differently; both forms below
# name the same directory.
GREP_EXCLUDE=(--exclude-dir=templates)

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
done < <(grep -rEn "${GREP_EXCLUDE[@]}" "^(pub(\([^)]+\))?[[:space:]]+)?const [A-Z_]+: &str = \"[^\"]+\"" "$BLOCKS_DIR" 2>/dev/null || true)

# ---------- Phase 1.7: platform_state tables ----------
# `crates/impresspress-core/src/platform_state/<module>.rs` owns one platform
# table each, declared as `pub const TABLE`. Indexed by module name so Phase
# 3.7 can resolve `platform_state::<module>` to the table it reaches.
PLATFORM_STATE_DIR="crates/impresspress-core/src/platform_state"
declare -A PLATFORM_TABLE       # module -> table name
re_platform_table='const[[:space:]]+TABLE[[:space:]]*:[[:space:]]*&str[[:space:]]*=[[:space:]]*"([^"]+)"'
while IFS= read -r line; do
  file="${line%%:*}"
  rest="${line#*:}"
  rest="${rest#*:}"
  if [[ "$rest" =~ $re_platform_table ]]; then
    module="$(basename "$file" .rs)"
    PLATFORM_TABLE["$module"]="${BASH_REMATCH[1]}"
  fi
done < <(grep -rEn "^pub const TABLE: &str = \"[^\"]+\"" "$PLATFORM_STATE_DIR" 2>/dev/null || true)

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
done < <(grep -rEn "${GREP_EXCLUDE[@]}" "^use[[:space:]]" "$BLOCKS_DIR" 2>/dev/null || true)

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
done < <(find "$BLOCKS_DIR" -path "$GUEST_TEMPLATES_DIR" -prune -o -name '*.rs' -print 2>/dev/null)

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
# whitespace collapsed. Handles multi-line brace forms. Line comments inside
# a brace group (an `// audit-allow:` pragma on the platform_state entry of
# a `use crate::{..}` group, say) are stripped before the `;` test, so a
# `;` in comment text cannot end the statement early.
read_use_statements() {
  awk '
    /^[[:space:]]*(pub(\([^)]+\))?[[:space:]]+)?use[[:space:]]/ {
      buf = $0
      sub(/\/\/.*$/, "", buf)
      while (buf !~ /;/) {
        if ((getline next_line) <= 0) break
        sub(/\/\/.*$/, "", next_line)
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

      # `use crate::platform_state::<module>::TABLE [as ALIAS]` binds the
      # platform table directly; the module lives outside `src/blocks/`, so
      # the file walk below cannot reach it.
      if [ "$src_const" = "TABLE" ] && [[ "$module_part" =~ ^crate::platform_state::([a-z_]+)$ ]]; then
        pmodule="${BASH_REMATCH[1]}"
        if [ -n "${PLATFORM_TABLE[$pmodule]:-}" ]; then
          FILE_USE_VALUE["${file}::${alias}"]="${PLATFORM_TABLE[$pmodule]}"
          FILE_USE_ALIAS["${file}::${alias}"]="TABLE"
          continue
        fi
      fi

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
done < <(find "$BLOCKS_DIR" -path "$GUEST_TEMPLATES_DIR" -prune -o -name '*.rs' -print 2>/dev/null)

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
  done < <(find "$BLOCKS_DIR" -path "$GUEST_TEMPLATES_DIR" -prune -o -name '*.rs' -print 2>/dev/null)
  [ "$changed" -eq 0 ] && break
done

# ---------- Phase 1.8: per-file platform_state references ----------
# Which `platform_state` modules each block file reaches: every `use
# crate::platform_state::<module>…` leaf (a whole-module import, an item
# import, `self`, or an alias) plus every inline `platform_state::<module>::`
# path. Read once here; the resolver consults it for `<module>::TABLE`
# tokens and Phase 3.7 walks it.
declare -A FILE_PLATFORM_MODULES   # file -> space-separated module names
while IFS= read -r file; do
  modules=""
  while IFS= read -r stmt; do
    [ -z "$stmt" ] && continue
    body="${stmt#*use }"
    body="${body%;*}"
    while IFS= read -r entry; do
      [ -z "$entry" ] && continue
      src_path="${entry%%|*}"
      if [[ "$src_path" =~ ^crate::platform_state::([a-z_]+)(::|$) ]]; then
        modules="$modules ${BASH_REMATCH[1]}"
      fi
    done < <(explode_use_body "$body")
  done < <(read_use_statements "$file")
  while IFS= read -r match; do
    [ -z "$match" ] && continue
    match="${match#platform_state::}"
    modules="$modules ${match%::}"
  done < <(grep -oE "platform_state::[a-z_]+::" "$file" 2>/dev/null || true)
  [ -z "${modules// /}" ] && continue
  FILE_PLATFORM_MODULES["$file"]="$(echo "$modules" | tr ' ' '\n' | sort -u | tr '\n' ' ')"
done < <(find "$BLOCKS_DIR" -path "$GUEST_TEMPLATES_DIR" -prune -o -name '*.rs' -print 2>/dev/null)

# ---------- Phase 2: collect grants per-owning-block ----------
# Pattern:  ResourceGrant::{read,read_write,append}(GRANTEE, RESOURCE)[.typed(TYPE)]
# Grants live in a block's `BlockInfo::grants(vec![...])` — we attribute the
# grant to the file's owning block (the directory or .rs filename under
# blocks/).
#
# Each grant entry is encoded as:
#   "${owner_block_id}|${grantee}|${resource}|${type}|${kind}"
# where KIND is the constructor (`read`, `read_write`, `append`). An `append`
# grant is typed `Db` by construction and covers only inserts — see
# `check_coverage`.
# where TYPE is "Db" by default or the `ResourceType` variant named in
# `.typed()`. The default and the `.typed()` spelling must be the SAME
# string, and both must be a name upstream defines: a grant indexed under
# any other type matches nothing in `check_coverage` and is dropped from
# the audit without saying so.

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
  # 0. `<module>::TABLE` naming a `platform_state` module the file imports
  #    (`use crate::platform_state::{variables, ..}` then `variables::TABLE`),
  #    or the fully qualified `crate::platform_state::<module>::TABLE`. The
  #    modules live outside `src/blocks/`, so the file walk in 0c cannot
  #    reach them; a block's own `repo::variables::TABLE` (products) does
  #    not import `platform_state` and falls through to 0c.
  if [[ "$tok" =~ (^|::)([a-z_]+)::TABLE$ ]]; then
    local pmodule="${BASH_REMATCH[2]}"
    if [ -n "${PLATFORM_TABLE[$pmodule]:-}" ]; then
      if [[ "$tok" == *platform_state::* ]] || [[ " ${FILE_PLATFORM_MODULES[$file]:-} " == *" $pmodule "* ]]; then
        echo "${PLATFORM_TABLE[$pmodule]}"
        return
      fi
    fi
  fi
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

# Drop a `//` line comment from one line of Rust, ignoring a `//` that sits
# inside a double-quoted string — a grant resource is routinely a URL
# (`read("*", "https://api.example.com/*")`), and cutting at the scheme's
# slashes would corrupt the argument this script then resolves. Escaped
# quotes do not occur in these declarations and are not modelled.
strip_rust_line_comment() {
  awk '''{
    inq = 0
    for (i = 1; i <= length($0); i++) {
      c = substr($0, i, 1)
      if (c == "\"") { inq = !inq }
      else if (!inq && c == "/" && substr($0, i + 1, 1) == "/") {
        print substr($0, 1, i - 1); next
      }
    }
    print
  }''' <<< "$1"
}

# The `ResourceType` variants upstream defines
# (wafer-block/src/types/grants.rs). Kept as the Rust variant spelling
# because that is what `.typed(..)` writes and what this script indexes on;
# anything else captured from a `.typed(..)` is a name one side made up.
KNOWN_RESOURCE_TYPES="Db Config Storage Crypto Network Vector"

is_known_resource_type() {
  local candidate="$1" known
  for known in $KNOWN_RESOURCE_TYPES; do
    [ "$candidate" = "$known" ] && return 0
  done
  return 1
}

declare -i unparsed_grants=0
declare -i bad_type_grants=0
UNPARSED_GRANT_LINES=()
BAD_TYPE_GRANT_LINES=()

while IFS= read -r line; do
  file="${line%%:*}"
  rest="${line#*:}"
  lineno="${rest%%:*}"
  rest="${rest#*:}"
  # Match: ResourceGrant::read("a", "b"), ResourceGrant::read_write(IDENT, IDENT)
  # or ResourceGrant::append(IDENT, IDENT)
  # The args may be string literals or constant identifiers (with optional `super::module::` qualifier).
  # Bash requires the regex stored in a variable when it contains parens.
  re_grant='ResourceGrant::(read|read_write|append)\(([^,]+),[[:space:]]*([^)]+)\)'
  # rustfmt breaks a declaration whose arguments do not fit onto continuation
  # lines, leaving only `ResourceGrant::read_write(` on the matched line. Read
  # the rest of the statement before testing: a grant the audit cannot see is
  # reported as a MISSING grant for a call that IS granted, and the obvious
  # way to silence that is a duplicate grant or a pragma.
  stmt="$rest"
  if ! [[ "$stmt" =~ $re_grant ]]; then
    probe=$((lineno + 1))
    until [[ "$stmt" == *')'* ]] || [ "$probe" -gt $((lineno + 8)) ]; do
      # Comment-stripped: a `//` note on an argument's own line would
      # otherwise be joined INTO that argument, and the garbage grantee that
      # produces reports every call the grant covers as MISSING.
      stmt="$stmt $(strip_rust_line_comment "$(sed -n "${probe}p" "$file" 2>/dev/null)")"
      probe=$((probe + 1))
    done
  fi
  if [[ "$stmt" =~ $re_grant ]]; then
    kind="${BASH_REMATCH[1]}"
    grantee_raw="${BASH_REMATCH[2]// /}"
    resource_raw="${BASH_REMATCH[3]// /}"
    # A joined statement keeps rustfmt's trailing comma inside the captured
    # final argument.
    grantee_raw="${grantee_raw%,}"
    resource_raw="${resource_raw%,}"
    grantee="$(resolve_token "$grantee_raw" "$file")"
    resource="$(resolve_token "$resource_raw" "$file")"
    # Grant type: default Db; `.typed(...)` on the same line picks
    # another. rustfmt puts a long `.typed(...)` on its own continuation
    # line, so that line is read too — without it admin's
    # `read("*", "*").typed(Network)` and `read_write("*", "*").typed(Crypto)`
    # index as Db wildcards that cover every admin-owned table for
    # every caller, and no missing grant on an admin table can ever be
    # reported.
    #
    # The default is spelled the way `ResourceType::Db` is, so a grant that
    # declares its type explicitly indexes the same as one that leaves it
    # open.
    type="Db"
    re_typed='\.typed\(([^)]*ResourceType::)?([A-Za-z]+)\)'
    if [[ "$stmt" =~ $re_typed ]]; then
      type="${BASH_REMATCH[2]}"
    else
      next_line="$(sed -n "$((lineno + 1))p" "$file" 2>/dev/null)"
      if [[ "$next_line" =~ ^[[:space:]]*\.typed ]] && [[ "$next_line" =~ $re_typed ]]; then
        type="${BASH_REMATCH[2]}"
      fi
    fi
    # The variant is captured as free text, so a name this script does not
    # know is a grant indexed under a type nothing ever matches — invisible
    # to `check_coverage`, and visible only as a false MISSING somewhere
    # else. A typo and a genuine upstream addition look identical here, and
    # both need a human, so the captured name is checked against the set
    # upstream defines rather than trusted.
    if ! is_known_resource_type "$type"; then
      bad_type_grants=$((bad_type_grants + 1))
      BAD_TYPE_GRANT_LINES+=("${file}:${lineno}: .typed(${type})")
      echo "::warning file=${file},line=${lineno}::WRAP grant audit does not know ResourceType variant '${type}' (expected one of: ${KNOWN_RESOURCE_TYPES}); this grant is indexed under a type nothing matches"
    fi
    # Owning block = the block this file lives in
    owner="$(file_to_block_id "$file")"
    GRANTS+=("${owner}|${grantee}|${resource}|${type}|${kind}")
  else
    # A declaration this parser could not read is a grant the audit does not
    # know exists — which is the exact silence this walk is for. It surfaces
    # downstream as a MISSING finding for a call that IS granted, or as no
    # finding at all when another grant happens to cover the same call, so
    # it is announced here rather than left to be inferred.
    unparsed_grants=$((unparsed_grants + 1))
    UNPARSED_GRANT_LINES+=("${file}:${lineno}")
    echo "::warning file=${file},line=${lineno}::WRAP grant audit could not parse this ResourceGrant declaration; it is absent from the grant index"
  fi
done < <(grep -rEn "${GREP_EXCLUDE[@]}" "ResourceGrant::(read|read_write|append)\(" "$BLOCKS_DIR" 2>/dev/null || true)

# ---------- Phase 3: walk db::* callsites and check coverage ----------

# Returns "OK" if a grant covers (caller, table, access); otherwise "MISSING".
# ACCESS is `append` for an insert (`db::create`, the audit writer) and
# `other` for everything else. Grant matches when:
#   - resource_type is Db (or the grant's type is empty/wildcard)
#   - grantee == caller OR grantee == "*"
#   - resource == table OR (resource ends with "*" AND table starts with the prefix)
#   - the grant is not `append`, or ACCESS is `append`: an append-only grant
#     admits inserts and nothing else, not even a read
check_coverage() {
  local caller="$1" table="$2" access="$3"
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
    IFS='|' read -r g_owner g_grantee g_resource g_type g_kind <<< "$g"
    [ "$g_owner" != "$owner" ] && continue
    [ "$g_type" != "Db" ] && continue
    [ "$g_kind" = "append" ] && [ "$access" != "append" ] && continue
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
    op="${BASH_REMATCH[1]}"
    arg="${BASH_REMATCH[2]}"
    table="$(resolve_token "$arg" "$file")"
    caller="$(file_to_block_id "$file")"
    if [ "$op" = "create" ]; then access="append"; else access="other"; fi
    # Keyed on the access too: a caller's insert and its update of the same
    # table need different grants, so the first must not hide the second.
    pair_key="${caller}|${table}|${access}"
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
    result="$(check_coverage "$caller" "$table" "$access")"
    case "$result" in
      OK|OWN) ;;
      MISSING)
        missing=$((missing + 1))
        owner="$(table_to_owner "$table")"
        MISSING_LINES+=("${file}:${lineno}: ${caller} → ${table} [db::${op}] (owned by ${owner})")
        ;;
      NON_CONVENTIONAL)
        nonconv=$((nonconv + 1))
        NONCONV_LINES+=("${file}:${lineno}: ${caller} → ${table}")
        ;;
    esac
  fi
done < <(grep -rEn "${GREP_EXCLUDE[@]}" "db::(list|create|update|delete|count|get|find_one)\(" "$BLOCKS_DIR" 2>/dev/null || true)

# ---------- Phase 3.7: walk platform_state references ----------
# A block reaches a platform table through
# `crates/impresspress-core/src/platform_state/<module>.rs`, whose functions
# run under the CALLING block's WRAP identity — the module is a codec plus
# query helpers, not a service that acts as admin. Every `use
# crate::platform_state::<module>…` leaf and every inline
# `platform_state::<module>::` path in a block file is therefore a database
# access on that module's table, checked exactly like a `db::*` callsite that
# names it (same pragmas, same report). Without this, moving those calls out
# of `src/blocks/` would silently un-audit every cross-block read of the
# platform tables. The reported line is the file's first mention of
# `platform_state`; an `// audit-allow:` pragma goes on the line above it.
declare -i ps_total=0
while IFS= read -r file; do
  modules="${FILE_PLATFORM_MODULES[$file]:-}"
  [ -z "${modules// /}" ] && continue
  lineno="$(grep -n "platform_state" "$file" | head -1 | cut -d: -f1)"
  [ -z "$lineno" ] && lineno=1
  caller="$(file_to_block_id "$file")"
  for module in $modules; do
    [ -z "$module" ] && continue
    table="${PLATFORM_TABLE[$module]:-<unresolved:platform_state::${module}>}"
    # A platform_state module's functions read as well as write, so the
    # reference is checked as a general access.
    pair_key="${caller}|${table}|other"
    [ -n "${SEEN_PAIRS[$pair_key]:-}" ] && continue
    SEEN_PAIRS["$pair_key"]=1
    ps_total=$((ps_total + 1))
    total=$((total + 1))
    if file_allows_audit_skip "$file" || has_allow_pragma "$file" "$lineno"; then
      allowed=$((allowed + 1))
      ALLOWED_LINES+=("${file}:${lineno}: ${caller} → ${table} (via platform_state::${module})")
      continue
    fi
    if [[ "$table" == "<unresolved:"* ]]; then
      unresolved=$((unresolved + 1))
      UNRESOLVED_LINES+=("${file}:${lineno}: ${caller} → ${table}")
      continue
    fi
    result="$(check_coverage "$caller" "$table" other)"
    case "$result" in
      OK|OWN) ;;
      MISSING)
        missing=$((missing + 1))
        owner="$(table_to_owner "$table")"
        MISSING_LINES+=("${file}:${lineno}: ${caller} → ${table} (owned by ${owner}, via platform_state::${module})")
        ;;
      NON_CONVENTIONAL)
        nonconv=$((nonconv + 1))
        NONCONV_LINES+=("${file}:${lineno}: ${caller} → ${table} (via platform_state::${module})")
        ;;
    esac
  done
done < <(find "$BLOCKS_DIR" -path "$GUEST_TEMPLATES_DIR" -prune -o -name '*.rs' -print 2>/dev/null)

# ---------- Phase 3.8: walk admin-audit writer references ----------
# The admin audit trail is one table with one writer,
# `blocks::admin::logs::audit_log`, and blocks other than admin reach it:
# directly (userportal's portal buttons) and through
# `ui::settings_form::save_settings`, which audits every settings page it
# serves. Like `platform_state`, that writer is a helper rather than a
# service — it runs under the CALLING block's WRAP identity, so the row lands
# only when the admin block grants that block the table.
#
# Neither reference is a `db::*` callsite in the caller's own file, and
# `save_settings` does not even live under `src/blocks/`, so Phase 3
# attributes both writes to the admin block and sees nothing to check. This
# phase attributes them to the block that made the call, checked exactly like
# a `db::*` callsite naming the table (same pragmas, same report).
#
# What it does NOT see, so this is not read as more than it is: the two
# references are named literally below, so a call spelled through an
# unqualified import (`use ...::save_settings;` then a bare
# `save_settings(..)`) and any future shared helper outside `src/blocks/`
# that wraps `audit_log` are both invisible until added to that pattern; and
# because this phase honours Phase 3's pragmas, an `// audit-allow-file:`
# added for a `db::*` callsite exempts that file's audit-writer calls too.
AUDIT_WRITER_TABLE="${CONST_VALUE[AUDIT_LOGS_TABLE]:-}"
if [ -z "$AUDIT_WRITER_TABLE" ]; then
  echo "::error::AUDIT_LOGS_TABLE not indexed — the admin audit writer cannot be walked." >&2
  exit 2
fi
declare -i audit_writer_total=0
while IFS= read -r line; do
  file="${line%%:*}"
  rest="${line#*:}"
  lineno="${rest%%:*}"
  caller="$(file_to_block_id "$file")"
  # The writer only inserts (`db::create`), so an append grant covers it.
  pair_key="${caller}|${AUDIT_WRITER_TABLE}|append"
  [ -n "${SEEN_PAIRS[$pair_key]:-}" ] && continue
  SEEN_PAIRS["$pair_key"]=1
  audit_writer_total=$((audit_writer_total + 1))
  total=$((total + 1))
  if file_allows_audit_skip "$file" || has_allow_pragma "$file" "$lineno"; then
    allowed=$((allowed + 1))
    ALLOWED_LINES+=("${file}:${lineno}: ${caller} → ${AUDIT_WRITER_TABLE} (via the audit writer)")
    continue
  fi
  result="$(check_coverage "$caller" "$AUDIT_WRITER_TABLE" append)"
  case "$result" in
    OK|OWN) ;;
    MISSING)
      missing=$((missing + 1))
      owner="$(table_to_owner "$AUDIT_WRITER_TABLE")"
      MISSING_LINES+=("${file}:${lineno}: ${caller} → ${AUDIT_WRITER_TABLE} (owned by ${owner}, via the audit writer)")
      ;;
    NON_CONVENTIONAL)
      nonconv=$((nonconv + 1))
      NONCONV_LINES+=("${file}:${lineno}: ${caller} → ${AUDIT_WRITER_TABLE} (via the audit writer)")
      ;;
  esac
done < <(grep -rEn "${GREP_EXCLUDE[@]}" "(audit_log|settings_form::save_settings)\(" "$BLOCKS_DIR" 2>/dev/null || true)

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
done < <(grep -rEn "${GREP_EXCLUDE[@]}" "clients::storage::(get|put|delete|list|create_folder|delete_folder|list_folders|get_stream)\(" "$BLOCKS_DIR" 2>/dev/null || true)

# ---------- Phase 3.9: self-check ----------
# Both ways this parser can fail are silent by construction: a declaration it
# cannot read, and one whose `.typed(..)` names a variant it does not know,
# each drop a grant out of the index and show up — if at all — as a MISSING
# finding somewhere unrelated. Nothing downstream can tell that apart from a
# grant that was never written, so the guards need their own proof.
#
# So the guards are exercised on every invocation, against synthetic
# fixtures, through this same script: the real grep, the real multi-line
# join, the real validation and the real exit code. `$AUDIT_WRAP_GRANTS_
# FIXTURE_DIR` is what points a child run at a fixture instead of the repo,
# and its presence is also what stops a child recursing.
run_self_check() {
  local tmp out status failures=0
  tmp="$(mktemp -d)"
  # shellcheck disable=SC2064
  trap "rm -rf '$tmp'" RETURN

  # Writes a two-block fixture: the admin block declaring the grant handed
  # on stdin, and a userportal file whose `db::create` (or `db::$1`) on the
  # admin table is covered by exactly that grant. The callsite is what makes
  # a misread grant observable — a dropped or corrupted one stops covering
  # the call, so the walk reports MISSING and the run fails.
  _fixture() {
    local op="${1:-create}"
    rm -rf "${tmp:?}/blocks"
    mkdir -p "$tmp/blocks/admin" "$tmp/blocks/userportal"
    {
      echo 'pub const FIXTURE_TABLE: &str = "impresspress__admin__fixture";'
      # Phase 3.8 refuses to run without this constant, so the fixture
      # carries it the way the admin block does.
      echo 'pub const AUDIT_LOGS_TABLE: &str = "impresspress__admin__audit_logs";'
      cat
    } > "$tmp/blocks/admin/mod.rs"
    cat > "$tmp/blocks/userportal/pages.rs" <<CALLER
use crate::blocks::admin::FIXTURE_TABLE;
async fn writes_the_admin_table(ctx: &dyn Context) {
    let _ = db::${op}(ctx, FIXTURE_TABLE, data).await;
}
CALLER
  }
  _run() {
    AUDIT_WRAP_GRANTS_FIXTURE_DIR="$tmp/blocks" bash "$0" 2>&1
  }
  _expect() {
    local what="$1" want_status="$2" want_text="$3"
    if [ "$status" != "$want_status" ]; then
      echo "::error::WRAP grant audit self-check [$what]: expected exit $want_status, got $status" >&2
      failures=$((failures + 1))
    fi
    if ! grep -qF "$want_text" <<< "$out"; then
      echo "::error::WRAP grant audit self-check [$what]: output did not contain: $want_text" >&2
      failures=$((failures + 1))
    fi
  }

  # A variant name neither side defines. Parses cleanly, so only an explicit
  # check against the upstream set can catch it by name (it also stops
  # covering the callsite, which is the mystery MISSING it used to be).
  _fixture <<'FIXTURE'
    wafer_run::ResourceGrant::read_write("impresspress/userportal", FIXTURE_TABLE)
        .typed(wafer_run::ResourceType::Database),
FIXTURE
  out="$(_run)" && status=0 || status=$?
  _expect "unknown variant" 1 "does not know ResourceType variant 'Database'"

  # A declaration whose closing paren never arrives.
  _fixture <<'FIXTURE'
    wafer_run::ResourceGrant::read_write(
        "impresspress/userportal",
FIXTURE
  out="$(_run)" && status=0 || status=$?
  _expect "unparsed declaration" 1 "could not parse this ResourceGrant declaration"

  # The same grant spelled correctly: no warning, and a verdict is given.
  _fixture <<'FIXTURE'
    wafer_run::ResourceGrant::read_write("impresspress/userportal", FIXTURE_TABLE)
        .typed(wafer_run::ResourceType::Db),
FIXTURE
  out="$(_run)" && status=0 || status=$?
  _expect "well-formed grant" 0 "OK — no missing WRAP grants."

  # A `//` note on an argument's own line must not become part of that
  # argument: the grantee it corrupts is what turns covered calls MISSING.
  _fixture <<'FIXTURE'
    wafer_run::ResourceGrant::read_write(
        // the portal writes the admin audit trail
        "impresspress/userportal",
        FIXTURE_TABLE,
    )
    .typed(wafer_run::ResourceType::Db),
FIXTURE
  out="$(_run)" && status=0 || status=$?
  _expect "comment inside a declaration" 0 "OK — no missing WRAP grants."

  # An append-only grant covers the insert...
  _fixture <<'FIXTURE'
    wafer_run::ResourceGrant::append("impresspress/userportal", FIXTURE_TABLE),
FIXTURE
  out="$(_run)" && status=0 || status=$?
  _expect "append grant, insert" 0 "OK — no missing WRAP grants."

  # ...and nothing else: an update under it is a missing grant.
  _fixture update <<'FIXTURE'
    wafer_run::ResourceGrant::append("impresspress/userportal", FIXTURE_TABLE),
FIXTURE
  out="$(_run)" && status=0 || status=$?
  _expect "append grant, update" 1 "impresspress/userportal → impresspress__admin__fixture [db::update]"

  if [ "$failures" -gt 0 ]; then
    echo "::error::WRAP grant audit self-check failed (${failures} assertion(s)); the grant walk cannot be trusted." >&2
    return 1
  fi
  return 0
}

if [ -z "${AUDIT_WRAP_GRANTS_FIXTURE_DIR:-}" ]; then
  run_self_check || exit 2
  self_check_note="self-check: parser guards verified against fixtures."
else
  self_check_note=""
fi

# ---------- Phase 4: report ----------

echo
echo "WRAP grant audit — $(date)"
echo
echo "Indexed: ${#CONST_VALUE[@]} constants, ${#GRANTS[@]} grant decls."
[ -n "$self_check_note" ] && echo "$self_check_note"
if [ "${#UNPARSED_GRANT_LINES[@]}" -gt 0 ]; then
  echo "UNPARSED ResourceGrant declarations (${unparsed_grants}) — absent from the index above:"
  printf '  %s\n' "${UNPARSED_GRANT_LINES[@]}"
fi
if [ "${#BAD_TYPE_GRANT_LINES[@]}" -gt 0 ]; then
  echo "UNKNOWN ResourceType variants (${bad_type_grants}) — indexed under a type nothing matches:"
  printf '  %s\n' "${BAD_TYPE_GRANT_LINES[@]}"
fi
echo "Database: ${total} unique (caller, table, access) triples; ${allowed} pragma-allowed (${ps_total} reached through platform_state, ${audit_writer_total} through the audit writer)."
echo "Storage:  ${storage_total} unique (caller, resource) pairs; ${storage_allowed} pragma-allowed."
echo

if [ "${#MISSING_LINES[@]}" -gt 0 ]; then
  echo "MISSING grants (${missing}):"
  printf '  %s\n' "${MISSING_LINES[@]}"
  echo
fi

if [ "${#ALLOWED_LINES[@]}" -gt 0 ]; then
  echo "ALLOWED by pragma (${allowed}) — each carries its reason at the site:"
  printf '  %s\n' "${ALLOWED_LINES[@]}"
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

# A grant the index could not read, or read under a name nothing matches, is
# a hole in the very thing this script asserts. Whether it also produced a
# MISSING finding is luck — another grant may happen to cover the same call
# — so "no missing grants" is not a verdict this run is entitled to give.
# Failing here is what keeps a knowingly incomplete index from riding into
# main green.
if [ "$unparsed_grants" -gt 0 ] || [ "$bad_type_grants" -gt 0 ]; then
  echo "::error::WRAP grant audit index is incomplete: ${unparsed_grants} unparsed declaration(s), ${bad_type_grants} unknown ResourceType variant(s). No verdict on missing grants is possible until they are resolved."
  exit 1
fi

echo "OK — no missing WRAP grants."
