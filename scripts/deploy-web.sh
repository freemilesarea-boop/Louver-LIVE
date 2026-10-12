#!/usr/bin/env bash
# Release the web dashboard without touching the server that is broadcasting.
#
#   scripts/deploy-web.sh release apps/web/dist      # publish + activate
#   scripts/deploy-web.sh check                      # is a release ready to serve?
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
# THE OTHER BUILD'S FILES STAY REACHABLE
#
# Before the swap, the outgoing release's `assets/` is hard-linked into the
# incoming one. A browser still holding the other build's `index.html` names a
# hashed chunk that this release never built; without the carry-forward that is
# a 404 and a blank screen. Hashed filenames cannot collide, so the union is
# always well defined, and a hard link means the bytes exist once on disk
# however many releases reference them. It runs on a rollback too, in the same
# direction — whichever release is being replaced hands its files over — so the
# union grows in both directions and no browser is left without its chunk.
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
CARRY=1
CMD="${1:-}"
[ $# -gt 0 ] && shift

usage() {
  cat <<'USAGE'
Release the web dashboard without touching the server that is broadcasting.

  deploy-web.sh release <dist>    publish a bundle and make it live
  deploy-web.sh publish <dist>    stage a bundle without serving it
  deploy-web.sh activate <ver>    make a staged release live
  deploy-web.sh rollback          go back to the release before this one
  deploy-web.sh check             refuse-to-proceed check: is a release ready?
  deploy-web.sh list              what is on disk, and what is live
  deploy-web.sh prune --keep N    delete all but the newest N releases

  --root DIR     the releases directory   (default: $LOUVER_WEB_RELEASES or /srv/louver/web)
  --version V    name this release        (default: a UTC timestamp and the git sha)
  --force        replace an existing release of the same name
  --no-carry     do not hand the outgoing release's assets to the incoming one
  --keep N       how many releases prune leaves behind (minimum 2)

Activation is one rename of a symlink: no reload, no signal, no restart, and
nothing here can reach the container that holds the FFmpeg processes.
USAGE
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
    --no-carry) CARRY=0; shift ;;
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

# Hand the outgoing release's hashed files to the incoming one, so a browser
# that is still holding the other build's `index.html` can still fetch the chunk
# it names. `-l` hard-links (the bytes exist once, whatever references them),
# `-n` never overwrites, and a hashed name cannot mean two different files — so
# this cannot change what the incoming release serves under its own names. On a
# filesystem that refuses the link (a different device), it copies instead.
carry_assets() { # carry_assets <from-version> <to-version>
  local from="$(releases_dir)/$1/assets" to="$(releases_dir)/$2/assets"
  [ -d "$from" ] && [ -d "$to" ] || return 0
  local before after
  before="$(find "$to" -type f | wc -l)"
  cp -aln "$from/." "$to/" 2>/dev/null || cp -an "$from/." "$to/" 2>/dev/null || {
    say "WARNING: could not carry $1's assets into $2 — a browser on the old build may 404"
    return 0
  }
  after="$(find "$to" -type f | wc -l)"
  say "carried $((after - before)) file(s) from $1 so its pages keep working"
}

cmd_activate() {
  local v="${ARGS[0]:-$VERSION}"
  [ -n "$v" ] || die "activate needs a version (see: $0 list)"
  [ -d "$(releases_dir)/$v" ] || die "no such release: $v"
  check_bundle "$(releases_dir)/$v"
  local now; now="$(link_target current)"
  if [ "$now" = "$v" ]; then say "$v is already current"; return 0; fi
  # Before the swap, not after: the release has to be complete at the instant it
  # becomes reachable.
  if [ -n "$now" ] && [ "$CARRY" = 1 ]; then carry_assets "$now" "$v"; fi
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

# The gate in front of the Caddy switch.
#
# Pointing Caddy at this directory when no release is live does not fail loudly:
# Caddy starts, `/api/*` and `/health` keep answering, and every single static
# path returns 404 — a blank dashboard that looks like a Caddy fault. Measured,
# not assumed. So the switch is only ever made after this exits 0.
cmd_check() {
  local bad=0
  ok() { printf '  ok    %s\n' "$1"; }
  # Failures go to stderr, so that a caller piping this into a log still sees
  # the reason it refused — and so an operator who pipes stdout away cannot miss
  # the one line that matters.
  nope() { printf '  FAIL  %s\n' "$1" >&2; bad=1; }

  if [ -d "$(releases_dir)" ]; then ok "releases/ exists under $ROOT"
  else nope "no releases/ under $ROOT — nothing has been published"; fi

  if [ -L "$ROOT/current" ]; then
    local target; target="$(readlink "$ROOT/current")"
    ok "current is a symlink -> $target"
    case "$target" in
      /*) nope "current is an ABSOLUTE link; it will not resolve inside the container's /srv/web" ;;
      *)  ok "the link is relative, so it resolves on the host and under /srv/web" ;;
    esac
    if [ -d "$ROOT/current/" ]; then
      ok "it resolves to a directory that exists"
      if [ -f "$ROOT/current/index.html" ]; then ok "index.html is there"
      else nope "no index.html — every page would 404"; fi
      local n; n="$(find "$ROOT/current/assets" -type f 2>/dev/null | wc -l)"
      if [ "$n" -gt 0 ]; then ok "assets/ holds $n file(s)"
      else nope "assets/ is empty or missing — the dashboard would load no code"; fi
    else
      nope "the link is dangling — its release is gone from disk"
    fi
  elif [ -e "$ROOT/current" ]; then
    nope "current exists but is not a symlink; activate/rollback cannot swap it"
  else
    nope "no current link — publish and activate a release BEFORE pointing Caddy here"
  fi

  if [ -L "$ROOT/previous" ]; then
    if [ -d "$ROOT/previous/" ]; then ok "previous resolves, so rollback has somewhere to go"
    else nope "previous is dangling — rollback would fail"; fi
  else
    printf '  note  no previous release yet; rollback has nowhere to go until the next release\n'
  fi

  [ "$bad" = 0 ] || { printf '\n  NOT READY — do not switch Caddy to this directory.\n' >&2; return 1; }
  printf '\n  ready: %s\n' "$(readlink "$ROOT/current")"
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
  check) cmd_check ;;
  list) cmd_list ;;
  prune) cmd_prune ;;
  ''|-h|--help|help) usage 0 ;;
  *) die "unknown command: $CMD (try --help)" ;;
esac
