#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

backup_dir=".codaze-local/local-codex"
lock_backup_path="$backup_dir/Cargo.lock.backup"
active_config=".cargo/config.toml"
template_config=".cargo/config.toml.example"

if [[ -f "$active_config" ]] && cmp -s "$active_config" "$template_config"; then
  echo ".cargo/config.toml contains the managed local Codex override and must not remain active for verify/CI." >&2
  echo "Remove it or run scripts/reset-local-codex.sh before validating the repository." >&2
  exit 1
fi

if [[ -f "$lock_backup_path" ]]; then
  echo "found managed local Codex backup state in $backup_dir" >&2
  echo "run scripts/reset-local-codex.sh before validating the repository." >&2
  exit 1
fi

expected_rev="$(
  sed -n 's/^codex-client = .*rev = "\([^"]*\)".*/\1/p' Cargo.toml | head -n 1
)"

if [[ -z "$expected_rev" ]]; then
  echo "failed to determine pinned Codex revision from Cargo.toml" >&2
  exit 1
fi

expected_source="git+https://github.com/openai/codex.git?rev=${expected_rev}#${expected_rev}"

awk -v expected="$expected_source" '
function check_pkg() {
  if (name ~ /^codex-/ && source != expected) {
    printf "Cargo.lock package %s has source %s, expected %s\n", name, source, expected > "/dev/stderr";
    bad = 1;
  }
}

$0 == "[[package]]" {
  if (in_pkg) {
    check_pkg();
  }
  in_pkg = 1;
  name = "";
  source = "";
  next;
}

in_pkg && /^name = "/ {
  name = $0;
  sub(/^name = "/, "", name);
  sub(/"$/, "", name);
  next;
}

in_pkg && /^source = "/ {
  source = $0;
  sub(/^source = "/, "", source);
  sub(/"$/, "", source);
  next;
}

END {
  if (in_pkg) {
    check_pkg();
  }
  exit bad;
}
' Cargo.lock
