#!/bin/sh
# Create or verify the release tag for the commit that triggered the release
# workflow.
#
# Inputs come from the environment:
#
#   VERSION     version taken from the release commit subject, e.g. 0.2.0
#   GITHUB_SHA  commit that triggered the run
#   REMOTE      git remote to publish to, defaults to origin
#
# The tag is named after the version without a leading v, is annotated, and
# carries exactly `commit <7 character short hash>` as its message. The script
# never moves, deletes or recreates a tag: an existing one is reused only when
# it is annotated, points at this commit and carries that message.
set -eu

fail() {
  echo "release-tag: $*" >&2
  exit 1
}

version=${VERSION:-}
sha=${GITHUB_SHA:-}
remote=${REMOTE:-origin}

[ -n "$version" ] || fail "VERSION is not set"
[ -n "$sha" ] || fail "GITHUB_SHA is not set"

printf '%s\n' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$' ||
  fail "'$version' is not a release version"

printf '%s\n' "$sha" | grep -Eq '^[0-9a-f]{40}$' ||
  fail "'$sha' is not a commit hash"

head=$(git rev-parse HEAD)
[ "$head" = "$sha" ] ||
  fail "the checkout is $head, not the triggering commit $sha"

subject=$(git log -1 --format=%s "$head")
expected_subject="release: v$version"
[ "$subject" = "$expected_subject" ] ||
  fail "the commit subject is '$subject', not '$expected_subject'; only a release commit may be tagged"

short=$(printf '%s' "$head" | cut -c1-7)
expected_message="commit $short"
tag_ref="refs/tags/$version"
scratch_ref="refs/release-tag-check/$version"

# tag_message prints the annotation body of the tag object at <ref>, with
# trailing blank lines removed. The headers end at the first blank line.
tag_message() {
  git cat-file -p "$1" | awk '
    { line[NR] = $0 }
    END {
      i = 1
      while (i <= NR && line[i] != "") i++
      start = i + 1
      end = NR
      while (end >= start && line[end] ~ /^[[:space:]]*$/) end--
      text = ""
      for (j = start; j <= end; j++) text = text (j > start ? "\n" : "") line[j]
      printf "%s\n", text
    }'
}

# check_tag <ref> <where> validates one tag against the release commit.
check_tag() {
  ref=$1
  where=$2
  type=$(git cat-file -t "$ref" 2>/dev/null || true)
  [ "$type" = tag ] ||
    fail "the $where tag $version is not an annotated tag; a release tag must be annotated"
  target=$(git rev-parse --verify --quiet "$ref^{commit}" || true)
  [ "$target" = "$sha" ] ||
    fail "the $where tag $version points at ${target:-an unknown object}, not at the release commit $sha; it will not be moved"
  message=$(tag_message "$ref")
  [ "$message" = "$expected_message" ] ||
    fail "the $where tag $version carries the message '$message', not '$expected_message'; it will not be rewritten"
}

remote_has_tag() {
  git ls-remote --tags --exit-code --refs "$remote" "$tag_ref" >/dev/null 2>&1
}

# read_remote_tag fetches the remote tag object into a scratch ref, so its type,
# target and message can be inspected without touching refs/tags.
read_remote_tag() {
  git fetch --quiet --force --no-tags "$remote" "+$tag_ref:$scratch_ref" ||
    fail "cannot fetch the remote tag $version"
  check_tag "$scratch_ref" "remote"
  git update-ref -d "$scratch_ref"
}

if remote_has_tag; then
  read_remote_tag
  echo "release-tag: reusing the annotated tag $version at $short"
  exit 0
fi

if git show-ref --verify --quiet "$tag_ref"; then
  check_tag "$tag_ref" "local"
  echo "release-tag: the local tag $version already matches, pushing it"
else
  git config user.name "github-actions[bot]"
  git config user.email "41898282+github-actions[bot]@users.noreply.github.com"
  git tag -a "$version" -m "$expected_message" "$head"
  echo "release-tag: created the annotated tag $version with message '$expected_message'"
fi

git push --quiet "$remote" "$tag_ref" || fail "cannot push the tag $version"

# Confirm what actually landed instead of trusting a silent push.
remote_has_tag || fail "the remote tag $version is missing after the push"
read_remote_tag

echo "release-tag: published $version at $head"
