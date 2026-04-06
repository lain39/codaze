#!/usr/bin/env bash

set -euo pipefail

root_dir="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root_dir"

tag="${1:-}"
if [[ -z "$tag" ]]; then
  echo "usage: $0 vX.Y.Z" >&2
  exit 2
fi

if [[ "$tag" != v* ]]; then
  echo "release tag must start with 'v': $tag" >&2
  exit 1
fi

manifest_version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)"
tag_version="${tag#v}"
release_version="$tag_version"

if [[ -z "$manifest_version" ]]; then
  echo "failed to read version from Cargo.toml" >&2
  exit 1
fi

if [[ "$manifest_version" != "$tag_version" ]]; then
  base_tag_version="${tag_version%%[-+]*}"
  if [[ "$manifest_version" == "$base_tag_version" ]]; then
    release_version="$base_tag_version"
  else
    echo "tag version ($tag_version) does not match Cargo.toml version ($manifest_version)" >&2
    exit 1
  fi
fi

if ! grep -Eq '^## Unreleased$' CHANGELOG.md; then
  echo "CHANGELOG.md must contain an '## Unreleased' section" >&2
  exit 1
fi

release_heading="## ${release_version} - "
if ! grep -Eq "^## ${release_version} - [0-9]{4}-[0-9]{2}-[0-9]{2}$" CHANGELOG.md; then
  echo "CHANGELOG.md must contain a dated section for ${release_version}" >&2
  exit 1
fi

release_body="$(
  awk -v prefix="$release_heading" '
    index($0, prefix) == 1 { in_section = 1; next }
    in_section && /^## / { exit }
    in_section { print }
  ' CHANGELOG.md
)"

if ! printf '%s\n' "$release_body" | grep -Eq '[[:alnum:]]'; then
  echo "CHANGELOG.md section for ${release_version} is empty" >&2
  exit 1
fi

echo "release metadata OK for ${tag} (release section ${release_version})"
