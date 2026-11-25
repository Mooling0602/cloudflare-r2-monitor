#!/bin/sh
# Install the repository's git hooks, once per clone.
#
# The hook itself lives in `.githooks/` so it is versioned with the code it
# protects; git only reads hooks from `.git/hooks` (or `core.hooksPath`), so this
# script points git at the tracked copy.
set -eu

git config core.hooksPath .githooks
chmod +x .githooks/pre-commit

echo "core.hooksPath = $(git config core.hooksPath)"
echo "The pre-commit hook now refuses staged Cloudflare tokens, account ids and"
echo "himalaya SMTP passwords. Run it by hand with: sh .githooks/pre-commit"
