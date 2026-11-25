#!/bin/sh
# Build the release note for one version out of CHANGELOG.md.
#
# CHANGELOG.md holds one H2 section per version plus two shared H2 sections,
# `Header` and `Endfix`. A version's release note is:
#
#   <Header content>  (a version-level H3 `Header` overrides the shared one)
#   <the version's body, minus any H3 Header / Endfix subsections>
#   <Endfix content>  (a version-level H3 `Endfix` overrides the shared one)
#
# The `**full_changelog!**` marker in Endfix is replaced with a real compare
# link. For the first release there is no previous tag, so the link compares
# the tag against nothing, matching the preview in the skill.
#
# Usage: release-note.sh <version> <changelog> <repository> <previous-tag>
#   version       tag name, e.g. 0.1.0
#   changelog     path to CHANGELOG.md
#   repository    owner/name, e.g. Mooling0602/cloudflare-r2-monitor
#   previous-tag  the tag before this one, or "-" for the first release
#
# Prints the release note on stdout. Exits non-zero when the version section or
# a mandatory shared section is missing, so a malformed CHANGELOG fails the
# release instead of publishing an empty note.

set -eu

version=${1:?usage: release-note.sh <version> <changelog> <repository> <previous-tag>}
changelog=${2:?usage: release-note.sh <version> <changelog> <repository> <previous-tag>}
repository=${3:?usage: release-note.sh <version> <changelog> <repository> <previous-tag>}
previous=${4:--}

if [ ! -f "$changelog" ]; then
  echo "release-note: $changelog does not exist" >&2
  exit 1
fi

# section <heading> <level> prints the body under `## <heading>` or
# `### <heading>`, with surrounding blank lines trimmed. The section ends at the
# next heading of the same or a higher level, so an H3 subsection stays inside
# its H2 parent.
section() {
  heading=$1
  level=$2
  awk -v want="$heading" -v lvl="$level" '
    BEGIN { prefix = (lvl == 2) ? "## " : "### "; in_section = 0; done = 0 }
    $0 == prefix want { in_section = 1; done = 0; next }
    in_section && lvl == 2 && /^## / { in_section = 0; done = 1 }
    in_section && lvl == 3 && /^## / { in_section = 0; done = 1 }
    in_section && lvl == 3 && /^### / { in_section = 0; done = 1 }
    in_section { print }
    END { if (!done && !in_section) exit 0 }
  ' "$changelog" | trim_blanks
}

# trim_blanks removes leading and trailing blank lines.
trim_blanks() {
  awk '
    { lines[NR] = $0 }
    END {
      start = 1
      while (start <= NR && lines[start] ~ /^[[:space:]]*$/) start++
      end = NR
      while (end >= start && lines[end] ~ /^[[:space:]]*$/) end--
      for (i = start; i <= end; i++) print lines[i]
    }
  '
}

# A version body must drop its own Header / Endfix subsections: they are
# overrides, not body text, and the caller emits them separately.
body_without_overrides() {
  section "$version" 2 | awk '
    BEGIN { skip = 0 }
    /^### Header[[:space:]]*$/ { skip = 1; next }
    /^### Endfix[[:space:]]*$/ { skip = 1; next }
    /^### / { skip = 0 }
    /^## / { skip = 0 }
    !skip { print }
  ' | trim_blanks
}

if ! grep -qx "## $version" "$changelog"; then
  echo "release-note: CHANGELOG.md has no '## $version' section" >&2
  exit 1
fi

# Version-level overrides win; otherwise fall back to the shared sections.
header=$(section "$version" 3 >/dev/null 2>&1 || true)
version_header=$(awk -v want="$version" '
  BEGIN { in_version = 0; capture = 0 }
  $0 == "## " want { in_version = 1; next }
  in_version && /^## / { exit }
  in_version && $0 == "### Header" { capture = 1; next }
  capture && /^### / { exit }
  capture && /^## / { exit }
  capture { print }
' "$changelog" | trim_blanks)

version_endfix=$(awk -v want="$version" '
  BEGIN { in_version = 0; capture = 0 }
  $0 == "## " want { in_version = 1; next }
  in_version && /^## / { exit }
  in_version && $0 == "### Endfix" { capture = 1; next }
  capture && /^### / { exit }
  capture && /^## / { exit }
  capture { print }
' "$changelog" | trim_blanks)

shared_header=$(awk '
  BEGIN { capture = 0 }
  /^## Header[[:space:]]*$/ { capture = 1; next }
  capture && /^## / { exit }
  capture { print }
' "$changelog" | trim_blanks)

shared_endfix=$(awk '
  BEGIN { capture = 0 }
  /^## Endfix[[:space:]]*$/ { capture = 1; next }
  capture && /^## / { exit }
  capture { print }
' "$changelog" | trim_blanks)

header=${version_header:-$shared_header}
endfix=${version_endfix:-$shared_endfix}

if [ -z "$header" ] && [ -z "$endfix" ]; then
  echo "release-note: CHANGELOG.md has neither a version override nor the shared Header/Endfix" >&2
  exit 1
fi

if [ "$previous" = "-" ]; then
  compare="https://github.com/$repository/compare/$version"
else
  compare="https://github.com/$repository/compare/$previous...$version"
fi

# The marker is a tag, not literal content: replace it wherever it appears in
# the endfix, and keep the endfix otherwise verbatim.
endfix_rendered=$(printf '%s\n' "$endfix" | awk -v link="**Full Changelog**: $compare" '
  /^\*\*full_changelog!\*\*[[:space:]]*$/ { print link; next }
  { print }
')

body=$(body_without_overrides)

# Assemble with exactly one blank line between the three parts. Empty parts
# collapse instead of leaving stray blank lines at the top and bottom.
emit() { printf '%s\n' "$1" | awk 'NF || seen { seen = 1; print }'; }

{
  [ -n "$header" ] && printf '%s\n\n' "$header"
  if [ -n "$body" ]; then
    printf '%s\n' "$body"
  fi
  [ -n "$endfix_rendered" ] && printf '\n%s\n' "$endfix_rendered"
} | trim_blanks | sed '/./,$!d'
