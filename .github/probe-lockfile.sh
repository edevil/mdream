#!/usr/bin/env bash
# TEMPORARY DIAGNOSTIC - do not merge.
#
# Determines why `git checkout --detach "$BASE_SHA"` in bundle-size.yml aborts
# with "Your local changes to the following files would be overwritten" for any
# PR that changes pnpm-lock.yaml.
#
# Every command here is read-only. In particular `git status` and `git diff`
# normally refresh and rewrite the index, which would repair the very stat
# staleness we are trying to observe, so all such calls go through
# `git --no-optional-locks`.
#
# The decisive signal is the three-way hash comparison:
#   HEAD == index == worktree  -> content is identical; any reported "M" is
#                                 pure index stat staleness (hypothesis H1)
#   worktree differs           -> something genuinely rewrote the file
#                                 (hypothesis H2/H3)
set -uo pipefail

label="${1:-unlabelled}"

hash_of_head() { git rev-parse "HEAD:$1" 2>/dev/null || echo "<none>"; }
hash_of_index() { git --no-optional-locks ls-files -s -- "$1" 2>/dev/null | awk '{print $2}' || echo "<none>"; }
hash_of_worktree() { [ -f "$1" ] && git hash-object "$1" 2>/dev/null || echo "<none>"; }

echo "======== PROBE: ${label} ========"

for f in pnpm-lock.yaml crates/Cargo.lock; do
  h_head=$(hash_of_head "$f")
  h_index=$(hash_of_index "$f")
  h_work=$(hash_of_worktree "$f")
  echo "-- ${f}"
  echo "   HEAD blob     : ${h_head}"
  echo "   index blob    : ${h_index}"
  echo "   worktree hash : ${h_work}"
  if [ "$h_head" = "$h_index" ] && [ "$h_index" = "$h_work" ]; then
    echo "   VERDICT       : content identical (any 'M' below is stat staleness only)"
  else
    echo "   VERDICT       : CONTENT DIVERGES"
    echo "   -- diff stat:"
    git --no-optional-locks diff --stat -- "$f" | sed 's/^/      /' || true
    echo "   -- added lines: $(git --no-optional-locks diff -U0 -- "$f" | grep -c "^+" || true)"
    echo "   -- removed lines: $(git --no-optional-locks diff -U0 -- "$f" | grep -c "^-" || true)"
    echo "   -- added-line shapes, digits collapsed to N (top 25):"
    git --no-optional-locks diff -U0 -- "$f" | sed -n "s/^+//p" \
      | sed "s/[0-9][0-9.]*/N/g" | sort | uniq -c | sort -rn | head -25 | sed "s/^/      /" || true
    echo "   -- first 60 diff lines:"
    git --no-optional-locks diff -U1 -- "$f" | head -60 | sed 's/^/      /' || true
  fi
  if [ -f "$f" ]; then
    stat -c "   stat          : mtime=%y size=%s inode=%i" "$f" 2>/dev/null \
      || stat -f "   stat          : mtime=%Sm size=%z inode=%i" "$f" 2>/dev/null \
      || true
  fi
done

echo "-- diff-index vs HEAD, all paths (read-only, index not refreshed):"
git --no-optional-locks diff-index --name-status HEAD | sed 's/^/   /' || true
echo "-- count of paths reported modified: $(git --no-optional-locks diff-index --name-status HEAD | wc -l)"

echo "-- index file stat:"
idx=$(git rev-parse --git-path index 2>/dev/null || echo "")
if [ -n "$idx" ] && [ -f "$idx" ]; then
  stat -c "   index mtime=%y size=%s" "$idx" 2>/dev/null \
    || stat -f "   index mtime=%Sm size=%z" "$idx" 2>/dev/null \
    || true
else
  echo "   (index not found: '$idx')"
fi

echo "======== END PROBE: ${label} ========"
