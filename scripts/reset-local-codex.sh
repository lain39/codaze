#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

backup_dir=".codaze-local/local-codex"
lock_backup_path="$backup_dir/Cargo.lock.backup"
active_config=".cargo/config.toml"
template_config=".cargo/config.toml.example"

expected_rev="$(
  sed -n 's/^codex-client = .*rev = "\([^"]*\)".*/\1/p' Cargo.toml | head -n 1
)"

if [[ -z "$expected_rev" ]]; then
  echo "failed to determine pinned Codex revision from Cargo.toml" >&2
  exit 1
fi

expected_source="git+https://github.com/openai/codex.git?rev=${expected_rev}#${expected_rev}"

has_local_codex_lock_contamination() {
  awk -v expected="$expected_source" '
  function check_pkg() {
    if (name ~ /^codex-/ && source != expected) {
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
    exit bad ? 0 : 1;
  }
  ' Cargo.lock
}

if [[ -f "$active_config" ]]; then
  if cmp -s "$active_config" "$template_config"; then
    rm "$active_config"
    echo "removed managed .cargo/config.toml"
  else
    echo "leaving existing .cargo/config.toml untouched"
  fi
fi

if [[ -f "$lock_backup_path" ]]; then
  if cmp -s Cargo.lock "$lock_backup_path"; then
    rm -f "$lock_backup_path"
  elif has_local_codex_lock_contamination; then
    cp "$lock_backup_path" Cargo.lock
    rm -f "$lock_backup_path"
    echo "restored Cargo.lock from local Codex backup"
  else
    echo "Cargo.lock has local changes, but they do not look like local Codex override contamination." >&2
    echo "leaving Cargo.lock untouched and preserving $lock_backup_path for manual recovery" >&2
  fi
fi

if [[ -d "$backup_dir" ]] && [[ -z "$(find "$backup_dir" -mindepth 1 -print -quit)" ]]; then
  rmdir "$backup_dir"
fi
if [[ -d .codaze-local ]] && [[ -z "$(find .codaze-local -mindepth 1 -print -quit)" ]]; then
  rmdir .codaze-local
fi
