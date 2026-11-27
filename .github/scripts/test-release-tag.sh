#!/bin/sh
# Offline tests for release-tag.sh.
#
# Every case builds a throwaway bare repository as `origin` plus a working clone,
# so nothing here touches the network or this repository. Run it directly:
#
#   .github/scripts/test-release-tag.sh
set -eu

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
script="$here/release-tag.sh"

# The cases must not inherit a developer configuration: a signing key, an
# unusual tag format or a pre-set identity would change what the script sees.
export GIT_CONFIG_GLOBAL=/dev/null
export GIT_CONFIG_SYSTEM=/dev/null

root=$(mktemp -d "${TMPDIR:-/tmp}/release-tag-test.XXXXXX")
trap 'rm -rf "$root"' 0 1 2 15

failures=0
ok() { echo "ok - $*"; }
not_ok() {
  echo "not ok - $*"
  failures=$((failures + 1))
}

expect_pass() {
  desc=$1
  shift
  if "$@" >/dev/null 2>&1; then ok "$desc"; else not_ok "$desc (the script failed)"; fi
}

expect_fail() {
  desc=$1
  shift
  if "$@" >/dev/null 2>&1; then not_ok "$desc (the script succeeded)"; else ok "$desc"; fi
}

expect_equal() {
  desc=$1
  actual=$2
  expected=$3
  if [ "$actual" = "$expected" ]; then
    ok "$desc"
  else
    not_ok "$desc (got '$actual', expected '$expected')"
  fi
}

# new_repo <case> creates $root/<case>/{origin.git,work} with one commit on main.
new_repo() {
  case_dir="$root/$1"
  origin="$case_dir/origin.git"
  work="$case_dir/work"
  mkdir -p "$case_dir"
  git init --quiet --bare "$origin"
  git init --quiet "$work"
  git -C "$work" symbolic-ref HEAD refs/heads/main
  git -C "$work" config user.name "Test"
  git -C "$work" config user.email "test@example.org"
  printf 'test\n' > "$work/CHANGELOG.md"
  git -C "$work" add CHANGELOG.md
  git -C "$work" commit --quiet -m "chore: initial commit"
  git -C "$work" remote add origin "$origin"
  git -C "$work" push --quiet -u origin main
  # The bare repository starts with HEAD on master; point it at the branch the
  # cases clone, so a clone checks something out.
  git -C "$origin" symbolic-ref HEAD refs/heads/main
}

# add_release_commit <version> appends the release commit and prints its hash.
add_release_commit() {
  git -C "$work" commit --quiet --allow-empty -m "release: v$1"
  git -C "$work" push --quiet origin main
  git -C "$work" rev-parse HEAD
}

# run_tag <version> <sha> runs the script inside the working clone.
run_tag() {
  (cd "$work" && VERSION=$1 GITHUB_SHA=$2 sh "$script")
}

tag_type() { git -C "$origin" cat-file -t "refs/tags/$1" 2>/dev/null || echo missing; }
tag_object() { git -C "$origin" rev-parse --verify --quiet "refs/tags/$1" || echo missing; }
tag_commit() { git -C "$origin" rev-parse --verify --quiet "refs/tags/$1^{commit}" || echo missing; }
tag_message() { git -C "$origin" for-each-ref --format='%(contents)' "refs/tags/$1"; }

# A release commit gets an annotated tag with the message derived from it.
new_repo first-release
sha=$(add_release_commit 0.2.0)
expect_pass "creates the tag for a release commit" run_tag 0.2.0 "$sha"
expect_equal "tag is annotated" "$(tag_type 0.2.0)" tag
expect_equal "tag points at the release commit" "$(tag_commit 0.2.0)" "$sha"
expect_equal "tag message names the commit" "$(tag_message 0.2.0)" "commit $(printf '%s' "$sha" | cut -c1-7)"

# A rerun must reuse the published tag, not rewrite it.
before=$(tag_object 0.2.0)
expect_pass "reuses the tag on a rerun" run_tag 0.2.0 "$sha"
expect_equal "rerun leaves the tag object untouched" "$(tag_object 0.2.0)" "$before"

# The reuse path also works in a checkout that has no local tag at all, which is
# what a fresh CI checkout looks like.
fresh="$root/first-release/fresh"
git clone --quiet --no-tags --branch main "$origin" "$fresh"
# This case needs a checkout without the tag. --no-tags usually delivers one,
# but local clones have kept tags anyway (Git 2.56 on the CI runner did), so
# drop whatever the clone carried and say so in the log.
carried=$(git -C "$fresh" tag --list)
[ -z "$carried" ] || echo "note - the clone carried tags: $(printf '%s' "$carried" | tr '\n' ' ')"
git -C "$fresh" tag --list | while read -r t; do git -C "$fresh" tag -d "$t" >/dev/null; done
work=$fresh
expect_pass "reuses the remote tag without a local one" run_tag 0.2.0 "$sha"
expect_equal "reuse adds no tag object" "$(tag_object 0.2.0)" "$before"
expect_equal "reuse does not create a local tag" "$(git -C "$fresh" tag --list 0.2.0)" ""

# A lightweight tag is not a release tag.
new_repo lightweight
sha=$(add_release_commit 0.3.0)
git -C "$work" tag 0.3.0
git -C "$work" push --quiet origin refs/tags/0.3.0
before=$(tag_object 0.3.0)
expect_fail "refuses a lightweight tag" run_tag 0.3.0 "$sha"
expect_equal "leaves the lightweight tag alone" "$(tag_object 0.3.0)" "$before"
expect_equal "does not convert the lightweight tag" "$(tag_type 0.3.0)" commit

# A hand-written annotation that names the wrong thing is refused.
new_repo wrong-message
sha=$(add_release_commit 0.4.0)
git -C "$work" tag -a 0.4.0 -m "0.4.0"
git -C "$work" push --quiet origin refs/tags/0.4.0
before=$(tag_object 0.4.0)
expect_fail "refuses a tag with the wrong message" run_tag 0.4.0 "$sha"
expect_equal "does not rewrite the annotated tag" "$(tag_object 0.4.0)" "$before"

# A tag that points somewhere else is never moved onto the release commit.
new_repo wrong-commit
first=$(git -C "$work" rev-parse HEAD)
sha=$(add_release_commit 0.5.0)
git -C "$work" tag -a 0.5.0 -m "commit $(printf '%s' "$first" | cut -c1-7)" "$first"
git -C "$work" push --quiet origin refs/tags/0.5.0
before=$(tag_object 0.5.0)
expect_fail "refuses a tag that points at another commit" run_tag 0.5.0 "$sha"
expect_equal "does not move the misplaced tag" "$(tag_object 0.5.0)" "$before"
expect_equal "keeps the misplaced tag on its commit" "$(tag_commit 0.5.0)" "$first"

# Only a release commit may be tagged.
new_repo not-a-release
git -C "$work" commit --quiet --allow-empty -m "feat: something"
git -C "$work" push --quiet origin main
sha=$(git -C "$work" rev-parse HEAD)
expect_fail "refuses a commit that is not a release commit" run_tag 0.6.0 "$sha"
expect_equal "creates no tag for a feature commit" "$(tag_type 0.6.0)" missing

# The checkout must be the commit that triggered the run.
new_repo wrong-checkout
sha=$(add_release_commit 0.7.0)
parent=$(git -C "$work" rev-parse HEAD^)
expect_fail "refuses a checkout that is not the triggering commit" run_tag 0.7.0 "$parent"
expect_equal "creates no tag when the checkout is wrong" "$(tag_type 0.7.0)" missing

# A malformed version never reaches git tag.
new_repo malformed-version
sha=$(add_release_commit 0.8.0)
expect_fail "refuses a malformed version" run_tag 0.8 "$sha"
expect_equal "creates no tag for a malformed version" "$(tag_type 0.8)" missing

# A stale local tag is not published when it is the wrong kind.
new_repo local-tag
sha=$(add_release_commit 0.9.0)
git -C "$work" tag 0.9.0
expect_fail "refuses a local lightweight tag" run_tag 0.9.0 "$sha"
expect_equal "does not publish the local lightweight tag" "$(tag_type 0.9.0)" missing

if [ "$failures" -ne 0 ]; then
  echo "$failures check(s) failed" >&2
  exit 1
fi

echo "all checks passed"
