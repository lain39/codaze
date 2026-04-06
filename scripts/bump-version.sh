#!/usr/bin/env bash

set -euo pipefail

root_dir="${CODAZE_ROOT:-$(cd "$(dirname "$0")/.." && pwd)}"
cd "$root_dir"

new_version="${1:-}"
release_date="${2:-$(date +%F)}"

if [[ -z "$new_version" ]]; then
  echo "usage: $0 X.Y.Z [YYYY-MM-DD]" >&2
  exit 2
fi

if [[ ! "$new_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+([.-][0-9A-Za-z.-]+)?$ ]]; then
  echo "invalid version: $new_version" >&2
  exit 1
fi

if [[ ! "$release_date" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}$ ]]; then
  echo "invalid release date: $release_date" >&2
  exit 1
fi

current_version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)"
if [[ -z "$current_version" ]]; then
  echo "failed to read version from Cargo.toml" >&2
  exit 1
fi

if [[ "$current_version" == "$new_version" ]]; then
  echo "Cargo.toml already at version $new_version" >&2
  exit 1
fi

if ! grep -Eq '^## Unreleased$' CHANGELOG.md; then
  echo "CHANGELOG.md must contain an '## Unreleased' section" >&2
  exit 1
fi

if grep -Eq "^## ${new_version} - " CHANGELOG.md; then
  echo "CHANGELOG.md already contains a section for $new_version" >&2
  exit 1
fi

tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT

prelude_file="$tmp_dir/prelude"
unreleased_file="$tmp_dir/unreleased"
post_file="$tmp_dir/post"
trimmed_prelude_file="$tmp_dir/prelude-trimmed"
trimmed_post_file="$tmp_dir/post-trimmed"

awk -v prelude="$prelude_file" -v unreleased="$unreleased_file" -v post="$post_file" '
BEGIN {
  mode = "prelude"
}
/^## Unreleased$/ && mode == "prelude" {
  mode = "unreleased"
  next
}
mode == "unreleased" && /^## / {
  mode = "post"
}
{
  if (mode == "prelude") {
    print >> prelude
  } else if (mode == "unreleased") {
    print >> unreleased
  } else {
    print >> post
  }
}
' CHANGELOG.md

awk '
{
  lines[++count] = $0
}
END {
  while (count > 0 && lines[count] == "") {
    count--
  }
  for (i = 1; i <= count; i++) {
    print lines[i]
  }
}
' "$prelude_file" > "$trimmed_prelude_file"

awk '
{
  lines[++count] = $0
}
END {
  start = 1
  while (start <= count && lines[start] == "") {
    start++
  }
  for (i = start; i <= count; i++) {
    print lines[i]
  }
}
' "$post_file" > "$trimmed_post_file"

release_body_file="$tmp_dir/release-body"
awk '
BEGIN {
  section = ""
  kept = 0
}
/^### / {
  section = $0
  next
}
$0 == "" {
  next
}
$0 == "- Nothing yet" {
  next
}
{
  if (section != "") {
    if (kept > 0) {
      print ""
    }
    print section
    print ""
    section = ""
  }
  print
  kept = 1
}
END {
  if (kept == 0) {
    print "### Added"
    print ""
    print "- Nothing yet"
  }
}
' "$unreleased_file" > "$release_body_file"

updated_changelog="$tmp_dir/CHANGELOG.md"
{
  cat "$trimmed_prelude_file"
  printf '\n\n'
  printf '## Unreleased\n\n'
  printf '### Added\n\n- Nothing yet\n\n'
  printf '### Changed\n\n- Nothing yet\n\n'
  printf '### Fixed\n\n- Nothing yet\n\n'
  printf '### Removed\n\n- Nothing yet\n\n'
  printf '## %s - %s\n\n' "$new_version" "$release_date"
  cat "$release_body_file"
  if [[ -s "$trimmed_post_file" ]]; then
    printf '\n\n'
    cat "$trimmed_post_file"
  fi
  printf '\n'
} > "$updated_changelog"

awk -v new_version="$new_version" '
BEGIN {
  replaced = 0
}
!replaced && /^version = "/ {
  print "version = \"" new_version "\""
  replaced = 1
  next
}
{
  print
}
END {
  if (!replaced) {
    exit 1
  }
}
' Cargo.toml > "$tmp_dir/Cargo.toml"

mv "$tmp_dir/Cargo.toml" Cargo.toml
mv "$updated_changelog" CHANGELOG.md

echo "bumped version: $current_version -> $new_version"
echo "updated CHANGELOG.md with release date $release_date"
