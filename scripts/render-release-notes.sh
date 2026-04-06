#!/usr/bin/env bash

set -euo pipefail

root_dir="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root_dir"

tag="${1:-}"
if [[ -z "$tag" ]]; then
  echo "usage: $0 vX.Y.Z" >&2
  exit 2
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

release_heading="## ${release_version} - "

section="$(
  awk -v prefix="$release_heading" '
    index($0, prefix) == 1 { in_section = 1; next }
    in_section && /^## / { exit }
    in_section { print }
  ' CHANGELOG.md
)"

if [[ -z "${section//$'\n'/}" ]]; then
  echo "no changelog section found for ${release_version}" >&2
  exit 1
fi

printf '# codaze %s\n\n' "$tag_version"
printf '%s\n' "$section"
