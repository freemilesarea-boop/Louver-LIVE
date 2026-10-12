#!/usr/bin/env bash
# Release the web dashboard without touching the server that is broadcasting.
#
#   scripts/deploy-web.sh release apps/web/dist      # publish + activate
#   scripts/deploy-web.sh list
#   scripts/deploy-web.sh rollback
#   scripts/deploy-web.sh prune --keep 5
#
# The layout it owns, under --root (default /srv/louver/web):
#
#   releases/20261012T062500Z-22dddc6/   an unpacked apps/web/dist
#   releases/20261012T054100Z-e692dd8/   the one before it, kept for rollback
#   current  -> releases/20261012T062500Z-22dddc6     RELATIVE symlink
#   previous -> releases/20261012T054100Z-e692dd8     what rollback goes back to
#
# Activation is one `rename` of a symlink. There is no moment at which `current`
# points at nothing, no file is ever edited in place, and Caddy resolves the
# link per request — so no reload, no signal, no restart.
#
# WHAT THIS SCRIPT DELIBERATELY CANNOT DO
#
# It runs no container command, sends no signal and opens no database. The whole
# point is that a dashboard release cannot reach the `louver` container, because
# that container is the parent of every FFmpeg process on the machine and a
# paying customer is live in each of them. `scripts/deploy-web.test.mjs` asserts
# the absence rather than trusting this paragraph.
set -euo pipefail

ROOT="${LOUVER_WEB_RELEASES:-/srv/louver/web}"
VERSION=""
KEEP=5
FORCE=0
CMD="${1:-}"
[ $# -gt 0 ] && shift

usage() {
  sed -n '2,28p' "$0" | sed 's/^# \{0,1\}//'
  exit "${1:-1}"
}

die() { echo "deploy-web: $*" >&2; exit 1; }
# Progress goes to stderr so that `publish` can print the version it created on
# stdout and `release` can read it back without parsing prose.
say() { echo "  $*" >&2; }

ARGS=()
while [ $# -gt 0 ]; do
  case "$1" in
    --root) ROOT="${2:?--root needs a directory}"; shift 2 ;;
    --version) VERSION="${2:?--version needs a name}"; shift 2 ;;
    --keep) KEEP="${2:?--keep needs a number}"; shift 2 ;;
    --force) FORCE=1; shift ;;
    -h|--help) usage 0 ;;
    -*) die "unknown option $1" ;;
    *) ARGS+=("$1"); shift ;;
  esac
done

# `mv -T` is the only way to replace a symlink that points at a directory in one
# step; without it `mv` helpfully moves the new link *inside* the old target and
# the site starts serving `current/current/index.html`. Checked once, up front,
# rather than discovered halfway through an activation.
mv --help 2>&1 | grep -q -- '-T' || die "needs GNU mv (coreutils) for an atomic symlink swap"

releases_dir() { printf '%s/releases' "$ROOT"; }

# Newest first. The version prefix is a UTC timestamp, so a plain reverse sort is
# chronological without stat-ing anything.
all_releases() {
  [ -d "$(releases_dir)" ] || return 0
  find "$(releases_dir)" -mindepth 1 -maxdepth 1 -type d -printf '%f\n' 2>/dev/null \
    | grep -v '^\.' | sort -r
}

link_target() {
  # The stored target is relative (`releases/<v>`); callers want the bare name.
  [ -L "$ROOT/$1" ] || return 0
  basename "$(readlink "$ROOT/$1")"
}

default_version() {
  local stamp sha
  stamp="$(date -u +%Y%m%dT%H%M%SZ)"
  sha="$(git -C "$(dirname "$0")/.." rev-parse --short HEAD 2>/dev/null || true)"
  if [ -n "$sha" ]; then printf '%s-%s\n' "$stamp" "$sha"; else printf '%s\n' "$stamp"; fi
}

# A bundle that is missing its entry document would answer the whole site with
# 404 and look exactly like a Caddy misconfiguration. Refuse it here, where the
# message can say which directory was wrong.
check_bundle() {
  local dir="$1"
  [ -d "$dir" ] || die "no such directory: $dir"
  [ -f "$dir/index.html" ] || die "$dir has no index.html — is that a built apps/web/dist?"
  [ -d "$dir/assets" ] || die "$dir has no assets/ — is that a built apps/web/dist?"
}

cmd_publish() {
  local dist="${ARGS[0]:-}"
  [ -n "$dist" ] || die "publish needs a dist directory"
  check_bundle "$dist"
  local v; v="${VERSION:-$(default_version)}"
  local dest; dest="$(releases_dir)/$v"
  if [ -e "$dest" ]; then
    [ "$FORCE" = 1 ] || die "release $v already exists (--force to replace it)"
    rm -rf "$dest"
  fi
  mkdir -p "$(releases_dir)" "$ROOT/meta"
  # Copy to a hidden sibling and rename, so a release directory never exists in
  # a half-copied state that `current` could be pointed at.
  local staging; staging="$(releases_dir)/.incoming-$$"
  rm -rf "$staging"
  mkdir -p "$staging"
  cp -a "$dist/." "$staging/"
  mv -T "$staging" "$dest"
  # Metadata lives outside the release so that nothing the web root serves is
  # written by this script.
  {
    printf 'version=%s\n' "$v"
    printf 'published=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    printf 'source=%s\n' "$(cd "$dist" && pwd)"
  } > "$ROOT/meta/$v"
  say "published $v"
  printf '%s\n' "$v"
}

cmd_activate() {
  local v="${ARGS[0]:-$VERSION}"
  [ -n "$v" ] || die "activate needs a version (see: $0 list)"
  [ -d "$(releases_dir)/$v" ] || die "no such release: $v"
  check_bundle "$(releases_dir)/$v"
  local now; now="$(link_target current)"
  if [ "$now" = "$v" ]; then say "$v is already current"; return 0; fi
  if [ -n "$now" ]; then
    ln -sfn "releases/$now" "$ROOT/.previous.new"
    mv -T "$ROOT/.previous.new" "$ROOT/previous"
  fi
  # Created inside $ROOT so the link text stays relative and resolves the same
  # way on the host and under the container's /srv/web.
  ln -sfn "releases/$v" "$ROOT/.current.new"
  mv -T "$ROOT/.current.new" "$ROOT/current"
  say "current -> releases/$v${now:+  (previous: $now)}"
}

cmd_release() {
  local v; v="$(cmd_publish)"
  ARGS=("$v")
  cmd_activate
}

cmd_rollback() {
  local prev; prev="$(link_target previous)"
  [ -n "$prev" ] || die "no previous release recorded — nothing to roll back to"
  [ -d "$(releases_dir)/$prev" ] || die "previous release $prev is gone from disk"
  ARGS=("$prev")
  cmd_activate
}

cmd_list() {
  local cur prev
  cur="$(link_target current)"; prev="$(link_target previous)"
  local any=0
  while read -r v; do
    [ -n "$v" ] || continue
    any=1
    local mark="  "
    [ "$v" = "$cur" ] && mark="* "
    [ "$v" = "$prev" ] && mark="- "
    printf '%s%s\n' "$mark" "$v"
  done <<< "$(all_releases)"
  [ "$any" = 1 ] || say "no releases under $ROOT"
  printf '\n  * current   - previous   root: %s\n' "$ROOT"
}

cmd_prune() {
  case "$KEEP" in ''|*[!0-9]*) die "--keep needs a number" ;; esac
  [ "$KEEP" -ge 2 ] || die "--keep must be at least 2, so a rollback target survives"
  local cur prev n=0
  cur="$(link_target current)"; prev="$(link_target previous)"
  while read -r v; do
    [ -n "$v" ] || continue
    n=$((n + 1))
    [ "$n" -le "$KEEP" ] && continue
    # Belt and braces: the newest-N rule already keeps these in every ordinary
    # case, but deleting what the site is serving is not a mistake worth
    # leaving to an ordering assumption.
    if [ "$v" = "$cur" ] || [ "$v" = "$prev" ]; then say "kept $v (in use)"; continue; fi
    rm -rf "$(releases_dir)/$v" "$ROOT/meta/$v"
    say "removed $v"
  done <<< "$(all_releases)"
}

case "$CMD" in
  publish) cmd_publish ;;
  activate) cmd_activate ;;
  release) cmd_release ;;
  rollback) cmd_rollback ;;
  list) cmd_list ;;
  prune) cmd_prune ;;
  ''|-h|--help|help) usage 0 ;;
  *) die "unknown command: $CMD (try --help)" ;;
esac
