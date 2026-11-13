#!/usr/bin/env bash
# T124: the whole gate must leave the real machine as it found it.
#
# Records an inventory, runs `cargo test --workspace` here and `cargo test`
# in ../tp_basic, records it again, and fails on any difference. Read-only
# on the real machine: it lists, it deletes nothing.
#
# The inventory:
#   - `find` listings (names and mtimes) of the real stage, cache and run
#     directory, where they exist;
#   - whether `.telepatch` exists at `/`, `/tmp`, `/home`, `/var`, `/var/tmp`
#     and every other mount point `findmnt -rno TARGET` lists.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
tmp=$(mktemp -d)
trap 'rm -r -f -- "$tmp"' EXIT

inventory() {
    local d m
    for d in /var/tmp/telepatch /var/cache/telepatch /run/telepatch; do
        if [ -e "$d" ]; then
            echo "== $d"
            # No -L: a link is listed, never followed.
            find "$d" -printf '%p %T@\n' 2>&1 | LC_ALL=C sort
        else
            echo "== $d (absent)"
        fi
    done
    {
        printf '%s\n' / /tmp /home /var /var/tmp
        findmnt -rno TARGET
    } | LC_ALL=C sort -u | while IFS= read -r m; do
        # findmnt -r escapes spaces as \x20.
        m=$(printf '%b' "$m")
        if [ -e "${m%/}/.telepatch" ] || [ -L "${m%/}/.telepatch" ]; then
            echo "${m%/}/.telepatch: exists"
        else
            echo "${m%/}/.telepatch: absent"
        fi
    done
}

inventory >"$tmp/before"
(cd "$here" && cargo test --workspace)
if [ -d "$here/../tp_basic" ]; then
    (cd "$here/../tp_basic" && cargo test)
else
    echo "check-sandbox: ../tp_basic is missing; its tests were not run" >&2
    exit 1
fi
inventory >"$tmp/after"

if diff -u "$tmp/before" "$tmp/after"; then
    echo "check-sandbox: the real stage, cache, run dir and filesystem roots are unchanged"
else
    echo "check-sandbox: FAILED: a test touched the real machine (diff above)" >&2
    exit 1
fi
