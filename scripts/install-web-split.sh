#!/usr/bin/env bash
# Put the front-end split's six files into a deployment directory, and nothing
# else. Read docs/WEB_DEPLOY.md first.
#
#   install-web-split.sh preflight <stage-tree> <target-dir>   # refuses, or says OK
#   install-web-split.sh install   <stage-tree> <target-dir>
#   install-web-split.sh status                 <target-dir>   # journal vs disk
#   install-web-split.sh verify                 <target-dir>
#   install-web-split.sh recover                <target-dir>   # undo exactly this install
#
# This runs on a server whose other container is the parent of every FFmpeg
# process on the machine, with a paying customer live in each one. So it copies
# files and it does nothing else: no container command, no signal, no database.
# `scripts/deploy-web.test.mjs` asserts that absence rather than trusting this
# paragraph.
#
# WHAT IT DOES NOT CLAIM
#
# Six files cannot be published atomically as a set on a POSIX filesystem.
# Each individual file is: it is copied to a temporary name in the SAME
# directory and renamed over the target, so no reader ever sees a half-written
# file. The set is not — a failure can leave between 0 and 5 of them in place.
# The journal is what makes that range exact instead of a guess.
#
# THE JOURNAL IS WRITE-AHEAD
#
#   PLAN <path> <sha-it-should-get>    before the copy
#   FILE <path> <sha-it-got>           after the rename
#
# Two lines, not one, because a crash has to land somewhere and both landings
# must be recoverable:
#
#   PLAN written, killed before the rename   -> the file is absent. Nothing to undo.
#   renamed, killed before FILE is written   -> the file is there, and the PLAN
#       line says what its contents must be. If they match, it is provably ours
#       and comes off. If they do not, someone else wrote it and it STAYS.
#
# Each line is flushed with `sync` before the next step, so the journal cannot
# be behind the filesystem.
#
# WHAT RECOVERY WILL NOT DO
#
# It removes a file only when the bytes on disk are still the bytes this install
# wrote. Anything else is kept, named, and reported, and the command exits
# non-zero so that a person looks. It removes directories with `rmdir`, which
# refuses a directory that is not empty, so a kept file keeps its parent. It
# never touches a file it did not create. A journal it cannot parse stops it
# entirely rather than guessing. Every path it removes is spelled "${var:?}", so
# an unset variable halts the shell instead of handing `rm` a top-level path.
set -uo pipefail

CMD="${1:-}"; shift || true

# Sorted, so the order is the same on every run and in every report.
FILES="deploy/web/Caddyfile
docker-compose.web.yml
docs/WEB_DEPLOY.md
scripts/deploy-web.sh
scripts/web-split-preflight.sh
scripts/web-split-verify.sh"

# The content this script is willing to install: commit 9fc6cc1 of PR #11.
# A staging tree whose bytes differ is refused, so a corrupted transfer or the
# wrong commit cannot reach the deployment directory.
SHAS="3978c73effcdd035c30948b595a8496e621629b1ec070281bcfff04d5e7820ee  deploy/web/Caddyfile
85400efaa4d5435b893a4c379861e564cc615d20bf58d56ab4e8b66dcc60b60f  docker-compose.web.yml
3a3b6f182c71308e1f5955894d22a31df6f913779c8b554a2d0d939e5c1b0da8  docs/WEB_DEPLOY.md
86969201d3f6f8083bf43700220e1fec5fc59e8dc2c6468482069c92868a3bc2  scripts/deploy-web.sh
b2e0480761d0d984b655ce7d51e234e78ff5a684cd8c9d4bb4a3e9ddaab7e721  scripts/web-split-preflight.sh
5015c9c96eaa4386e22789b3de0dd467e838bcca929fcbad1be48923fc3c79f2  scripts/web-split-verify.sh"

# Directories that may have to be created. Parents first.
NEWDIRS="deploy
deploy/web"

# The six files need ~50KB. This floor is a sanity margin, not a service
# threshold: the service's own floor is 5GiB (crates/louver-cloud/src/ingest.rs),
# warned about separately because a server under it is already refusing uploads.
MIN_FREE_KB=4096
SERVICE_FLOOR_KB=$((5 * 1024 * 1024))

JOURNAL="${LOUVER_INSTALL_JOURNAL:-$HOME/louver-web-split-install.journal}"
LOCK="$JOURNAL.lock"

die()  { printf 'install-web-split: %s\n' "$*" >&2; exit 1; }
ok()   { printf '  ok      %s\n' "$1"; }
bad()  { printf '  FAIL    %s\n' "$1" >&2; BAD=$((BAD + 1)); }
note() { printf '  note    %s\n' "$1"; }

sha() { sha256sum "$1" | cut -d' ' -f1; }
mode_for() { case "$1" in *.sh) echo 755 ;; *) echo 644 ;; esac; }
want_sha() { printf '%s\n' "$SHAS" | awk -v f="$1" '$2 == f { print $1 }'; }

# Appended and flushed, so the journal is never behind the filesystem.
journal() { printf '%s\n' "$*" >> "$JOURNAL"; sync "$JOURNAL" 2>/dev/null || sync; }

check_target() {
  [ -n "${1:-}" ] || die "no target directory given"
  [ -d "$1" ] || die "no such directory: $1"
  [ -f "$1/docker-compose.yml" ] || die "$1 has no docker-compose.yml — is that the deployment directory?"
}

# One install or recovery at a time. Two of them racing would each see the other
# half-done work as someone else's and refuse to clean it up.
take_lock() {
  exec 9> "$LOCK" || die "cannot create the lock file $LOCK"
  flock -n 9 || die "another install or recovery is holding $LOCK — wait for it, or check with: $0 status <target>"
}

# ---------------------------------------------------------------- preflight
cmd_preflight() {
  local stage="$1" target="$2"
  BAD=0
  check_target "$target"
  [ -d "$stage" ] || die "no such staging tree: $stage"

  printf 'preflight  stage=%s  target=%s\n\n' "$stage" "$target"

  # 1. the staging tree is exactly the six files.
  local have
  have="$(cd "$stage" && find . -type f | sed 's|^\./||' | sort)"
  if [ "$have" = "$FILES" ]; then ok "staging holds exactly the six expected files"
  else bad "staging tree is not the expected six files:
$(diff <(echo "$FILES") <(echo "$have") | sed 's/^/            /')"; fi

  # ...all of them regular files. A symlink in the archive would otherwise be
  # copied as whatever it points at.
  local odd
  odd="$(find "$stage" \( -type l -o -type b -o -type c -o -type p -o -type s \) -printf '%P\n' 2>/dev/null)"
  if [ -z "$odd" ]; then ok "nothing in staging is a symlink or a special file"
  else bad "staging contains non-regular entries: $(echo "$odd" | tr '\n' ' ')"; fi

  # ...and its bytes are the ones from the commit, not merely files with the
  # right names.
  local want got nbad=0
  while read -r want got; do
    [ -n "$got" ] || continue
    if [ ! -f "$stage/$got" ]; then nbad=$((nbad + 1)); continue; fi
    [ "$(sha "$stage/$got")" = "$want" ] || { printf '            %s does not match the manifest\n' "$got"; nbad=$((nbad + 1)); }
  done <<< "$SHAS"
  if [ "$nbad" -eq 0 ]; then ok "every staged file matches the manifest"
  else bad "$nbad staged file(s) do not match the manifest — refusing"; fi

  # 2. no component of any target path is a symlink, and no target path escapes
  #    the deployment directory. A `scripts` symlinked to /etc would otherwise
  #    make this write into /etc.
  local root comp acc esc=0
  root="$(realpath "$target")"
  while IFS= read -r f; do
    [ -n "$f" ] || continue
    acc="$target"
    while IFS= read -r comp; do
      [ -n "$comp" ] || continue
      acc="$acc/$comp"
      if [ -L "$acc" ]; then bad "$acc is a symlink — refusing to write through it"; esc=1; fi
    done <<< "$(printf '%s\n' "$f" | tr '/' '\n')"
    case "$(realpath -m "$target/$f")" in
      "$root"/*) ;;
      *) bad "$f resolves outside $root — refusing"; esc=1 ;;
    esac
  done <<< "$FILES"
  [ "$esc" -eq 0 ] && ok "no target path is a symlink or escapes $root"

  # 3. nothing in the target conflicts. Three outcomes per file, and only one of
  #    them is allowed to be a surprise.
  NEW=""; SAME=""; DIFF=""
  while IFS= read -r f; do
    [ -n "$f" ] || continue
    if [ -e "$target/$f" ] || [ -L "$target/$f" ]; then
      if [ -L "$target/$f" ]; then DIFF="$DIFF $f(symlink)"
      elif [ -d "$target/$f" ]; then DIFF="$DIFF $f(directory)"
      elif [ ! -f "$target/$f" ]; then DIFF="$DIFF $f(not-a-regular-file)"
      elif cmp -s "$stage/$f" "$target/$f"; then SAME="$SAME $f"
      else DIFF="$DIFF $f"; fi
    else
      NEW="$NEW $f"
    fi
  done <<< "$FILES"
  printf '  new:    %s\n  same:   %s\n  differs:%s\n' "${NEW:- (none)}" "${SAME:- (none)}" "${DIFF:- (none)}"
  if [ -z "$DIFF" ]; then ok "no conflicting file in the target"
  else bad "a target path exists with different content or the wrong type — nothing will be installed"; fi
  [ -n "$NEW" ] || note "every file is already in place; install would do nothing"

  # 4. writability of every directory that would be written, including ones that
  #    do not exist yet (their nearest existing ancestor must be writable).
  #
  #    By creating a file and removing it, not by reading the mode: `[ -w ]` is
  #    true for root on a 0555 directory, and so is the write, so a mode-based
  #    check would report a problem that is not there — or miss one that is.
  #    Note that for a root install directory modes cannot fail it at all; what
  #    can is a read-only filesystem, an immutable attribute, or a full disk.
  local probe dir t
  for f in $NEW; do
    dir="$target/$(dirname "$f")"
    [ "$(basename "$f")" = "$f" ] && dir="$target"
    probe="$dir"
    while [ ! -d "$probe" ]; do probe="$(dirname "$probe")"; done
    t="$probe/.install-probe.$$"
    if ( : > "$t" ) 2>/dev/null; then
      rm -f -- "${t:?}"
      ok "writable: $probe (for $f)"
    else
      bad "cannot write into $probe (needed for $f)"
    fi
  done

  # 5. disk.
  local free_kb
  free_kb="$(df -Pk "$target" | awk 'NR==2{print $4}')"
  if [ "${free_kb:-0}" -ge "$MIN_FREE_KB" ]; then ok "disk free ${free_kb}KB (needs ~50KB)"
  else bad "only ${free_kb}KB free"; fi
  if [ "${free_kb:-0}" -lt "$SERVICE_FLOOR_KB" ]; then
    note "WARNING: under the service's own 5GiB floor — uploads are already refused and /health is degraded. A separate problem; this install neither causes nor fixes it."
  fi

  # 6. an earlier run's journal, and whether it agrees with the disk.
  if [ -f "$JOURNAL" ]; then
    note "a journal already exists at $JOURNAL — this run appends to it"
    journal_shape || bad "that journal has lines this script cannot parse — run '$0 status $target' and sort it out by hand before installing"
  fi

  printf '\n'
  [ "$BAD" -eq 0 ] || { printf 'PREFLIGHT FAILED (%d). Nothing has been touched.\n' "$BAD" >&2; return 1; }
  printf 'PREFLIGHT OK — %d file(s) to install\n' "$(echo $NEW | wc -w)"
}

# Every line must be one of four shapes. A journal that has drifted is a reason
# to stop, not to improvise.
journal_shape() {
  [ -f "$JOURNAL" ] || return 0
  local n=0
  while IFS= read -r line; do
    n=$((n + 1))
    case "$line" in
      '#'*) ;;
      'DIR '*)  [ "$(echo "$line" | wc -w)" -eq 2 ] || return 1 ;;
      'PLAN '*) [ "$(echo "$line" | wc -w)" -eq 3 ] || return 1 ;;
      'FILE '*) [ "$(echo "$line" | wc -w)" -eq 3 ] || return 1 ;;
      '') ;;
      *) return 1 ;;
    esac
  done < "$JOURNAL"
  return 0
}

# ---------------------------------------------------------------- install
cmd_install() {
  local stage="$1" target="$2"
  cmd_preflight "$stage" "$target" > /dev/null 2>&1 || die "preflight does not pass; run '$0 preflight $stage $target' and read it"
  check_target "$target"
  take_lock

  printf 'install  journal=%s\n\n' "$JOURNAL"
  journal "# run $(date -u +%Y-%m-%dT%H:%M:%SZ) stage=$stage target=$target"

  # Directories first, journaled as they are made so recovery knows which are
  # ours. A directory that already existed is NOT journaled.
  local d
  while IFS= read -r d; do
    [ -n "$d" ] || continue
    if [ ! -d "$target/$d" ]; then
      mkdir "$target/$d" || die "could not create $target/$d"
      journal "DIR $d"
      printf '  made dir   %s\n' "$d"
    fi
  done <<< "$NEWDIRS"

  local f tmp m installed=0 already=0
  while IFS= read -r f; do
    [ -n "$f" ] || continue
    if [ -f "$target/$f" ] && cmp -s "$stage/$f" "$target/$f"; then
      printf '  already    %s\n' "$f"
      already=$((already + 1))
      continue
    fi
    m="$(mode_for "$f")"
    # Written before the copy. If the kill lands between here and the rename,
    # the file is absent and there is nothing to undo; if it lands after the
    # rename, this line is what proves the file is ours.
    journal "PLAN $f $(want_sha "$f")"
    tmp="$target/$(dirname "$f")/.install-tmp.$$.$(basename "$f")"
    [ "$(basename "$f")" = "$f" ] && tmp="$target/.install-tmp.$$.$f"
    if ! cp "$stage/$f" "$tmp" 2>/dev/null; then
      rm -f -- "${tmp:?}"
      printf '  FAILED     %s (could not write into %s)\n' "$f" "$(dirname "$f")" >&2
      printf '\n%d file(s) installed before the failure. Journal: %s\n' "$installed" "$JOURNAL" >&2
      printf 'Undo exactly those:  %s recover %s\n' "$0" "$target" >&2
      return 1
    fi
    chmod "$m" "$tmp"
    # Same directory, so this is a rename: the target is either absent or the
    # whole new file, never half of one.
    if ! mv -T "$tmp" "$target/$f" 2>/dev/null; then
      rm -f -- "${tmp:?}"
      printf '  FAILED     %s (could not publish — something is in the way)\n' "$f" >&2
      printf '\n%d file(s) installed before the failure. Journal: %s\n' "$installed" "$JOURNAL" >&2
      printf 'Undo exactly those:  %s recover %s\n' "$0" "$target" >&2
      return 1
    fi
    journal "FILE $f $(sha "$target/$f")"
    installed=$((installed + 1))
    printf '  installed  %s  (%s)\n' "$f" "$m"
  done <<< "$FILES"

  printf '\nINSTALL OK — %d file(s) written, %d already in place\n' "$installed" "$already"
}

# ---------------------------------------------------------------- status
# Does the journal agree with the disk? Run this before trusting `recover`,
# and after any interruption.
cmd_status() {
  local target="$1"
  check_target "$target"
  printf 'status  journal=%s  target=%s\n\n' "$JOURNAL" "$target"
  if [ ! -f "$JOURNAL" ]; then
    printf 'no journal. Nothing is recorded as installed by this tool.\n'
    local n=0 f
    while IFS= read -r f; do [ -f "$target/$f" ] && n=$((n + 1)); done <<< "$FILES"
    printf '%d of 6 target files are present on disk.\n' "$n"
    [ "$n" -eq 0 ] && return 0
    printf 'They are present without a journal, so this tool will not remove them.\n' >&2
    return 1
  fi
  journal_shape || { printf 'JOURNAL UNREADABLE — a line does not parse. Fix or move it aside; recover will refuse.\n' >&2; return 2; }

  local disagree=0 kind path recorded now plan
  while read -r kind path recorded; do
    case "$kind" in
      DIR) [ -d "$target/$path" ] && printf '  dir     %s/  present\n' "$path" || printf '  dir     %s/  MISSING (made by us, gone now)\n' "$path" ;;
      PLAN)
        plan="$(grep -c "^FILE $path " "$JOURNAL" 2>/dev/null || true)"
        if [ "${plan:-0}" -gt 0 ]; then continue; fi
        # A PLAN with no FILE: the crash window. Decide it from the bytes.
        if [ ! -e "$target/$path" ]; then
          printf '  window  %s  absent — killed before the rename, nothing to undo\n' "$path"
        else
          now="$(sha "$target/$path")"
          if [ "$now" = "$recorded" ]; then
            printf '  window  %s  present and matches the plan — killed after the rename; recover will remove it\n' "$path"
          else
            printf '  WINDOW  %s  present but does NOT match the plan — someone else wrote it; recover will KEEP it\n' "$path" >&2
            disagree=$((disagree + 1))
          fi
        fi ;;
      FILE)
        if [ ! -e "$target/$path" ]; then
          printf '  file    %s  recorded but ABSENT\n' "$path"; disagree=$((disagree + 1))
        else
          now="$(sha "$target/$path")"
          if [ "$now" = "$recorded" ]; then printf '  file    %s  as installed\n' "$path"
          else printf '  file    %s  CHANGED since the install — recover will keep it\n' "$path" >&2; disagree=$((disagree + 1)); fi
        fi ;;
    esac
  done < "$JOURNAL"

  printf '\n'
  if [ "$disagree" -eq 0 ]; then printf 'JOURNAL AND DISK AGREE\n'; return 0; fi
  printf 'JOURNAL AND DISK DISAGREE in %d place(s). Look at the lines above before recovering.\n' "$disagree" >&2
  return 1
}

# ---------------------------------------------------------------- verify
cmd_verify() {
  local target="$1"
  BAD=0
  check_target "$target"
  printf 'verify  target=%s\n\n' "$target"
  local f m want
  while IFS= read -r f; do
    [ -n "$f" ] || continue
    if [ ! -f "$target/$f" ]; then bad "missing: $f"; continue; fi
    m="$(stat -c %a "$target/$f")"; want="$(mode_for "$f")"
    if [ "$m" != "$want" ]; then bad "$f has mode $m, expected $want"; continue; fi
    if [ "$(sha "$target/$f")" = "$(want_sha "$f")" ]; then ok "$f  $m  $(sha "$target/$f")"
    else bad "$f does not match the manifest"; fi
  done <<< "$FILES"
  printf '\n'
  [ "$BAD" -eq 0 ] || { printf 'VERIFY FAILED (%d)\n' "$BAD" >&2; return 1; }
  printf 'VERIFY OK\n'
}

# ---------------------------------------------------------------- recover
cmd_recover() {
  local target="$1"
  check_target "$target"
  [ -f "$JOURNAL" ] || die "no journal at $JOURNAL — nothing is known to have been installed, so nothing will be removed"
  journal_shape || die "the journal at $JOURNAL has a line this script cannot parse. It will not guess: move the journal aside and check the six paths by hand against docs/WEB_DEPLOY.md"
  take_lock
  printf 'recover  journal=%s  target=%s\n\n' "$JOURNAL" "$target"

  local removed=0 kept=0 gone=0 kind path recorded now expect d
  # Files in reverse order, so a directory is empty by the time it is reached.
  # A PLAN without a FILE is the crash window: judged on the bytes, same rule.
  while read -r kind path recorded; do
    case "$kind" in FILE) expect="$recorded" ;; PLAN) expect="$recorded" ;; *) continue ;; esac
    [ -n "$path" ] && [ -n "$expect" ] || { printf '  SKIPPED    a journal line with no path or hash\n' >&2; kept=$((kept + 1)); continue; }
    if [ "$kind" = "PLAN" ] && grep -q "^FILE $path " "$JOURNAL"; then continue; fi   # the FILE line handles it
    if [ ! -e "$target/$path" ]; then
      printf '  gone       %s (already absent)\n' "$path"; gone=$((gone + 1)); continue
    fi
    if [ -L "$target/$path" ] || [ ! -f "$target/$path" ]; then
      printf '  KEPT       %s — no longer a regular file; not touching it\n' "$path" >&2; kept=$((kept + 1)); continue
    fi
    now="$(sha "$target/$path")"
    if [ "$now" = "$expect" ]; then
      rm -f -- "${target:?}/${path:?}" && {
        [ "$kind" = "PLAN" ] && printf '  removed    %s (from the crash window; bytes matched the plan)\n' "$path" \
                             || printf '  removed    %s\n' "$path"
        removed=$((removed + 1)); }
    else
      printf '  KEPT       %s — changed since this install (wrote %.12s…, now %.12s…)\n' "$path" "$expect" "$now" >&2
      kept=$((kept + 1))
    fi
  done < <(grep -E '^(FILE|PLAN) ' "$JOURNAL" | tac)

  # Directories, deepest first. `rmdir` refuses a non-empty directory, which is
  # exactly what is wanted: a file that was kept keeps its parent.
  while read -r kind d; do
    [ "$kind" = "DIR" ] || continue
    [ -n "$d" ] || continue
    if [ ! -d "$target/$d" ]; then printf '  gone       %s/\n' "$d"; continue; fi
    if rmdir -- "${target:?}/${d:?}" 2>/dev/null; then printf '  removed    %s/\n' "$d"
    else printf '  KEPT       %s/ — not empty\n' "$d"; fi
  done < <(grep '^DIR ' "$JOURNAL" | tac)

  printf '\nremoved %d, kept %d, already absent %d\n' "$removed" "$kept" "$gone"
  mv "$JOURNAL" "$JOURNAL.$(date -u +%Y%m%dT%H%M%SZ).done"
  printf 'journal retired to %s.*.done\n' "$JOURNAL"
  if [ "$kept" -gt 0 ]; then
    printf '\n%d path(s) were KEPT because they are no longer what this install wrote.\nNothing was deleted that had been changed. Check them by hand.\n' "$kept" >&2
    return 1
  fi
}

case "$CMD" in
  preflight) [ $# -eq 2 ] || die "preflight <stage-tree> <target-dir>"; cmd_preflight "$1" "$2" ;;
  install)   [ $# -eq 2 ] || die "install <stage-tree> <target-dir>";   cmd_install   "$1" "$2" ;;
  status)    [ $# -eq 1 ] || die "status <target-dir>";                 cmd_status    "$1" ;;
  verify)    [ $# -eq 1 ] || die "verify <target-dir>";                 cmd_verify    "$1" ;;
  recover)   [ $# -eq 1 ] || die "recover <target-dir>";                cmd_recover   "$1" ;;
  *) die "usage: $0 {preflight|install|status|verify|recover} ..." ;;
esac
