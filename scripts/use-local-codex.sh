#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

backup_dir=".codaze-local/local-codex"
lock_backup_path="$backup_dir/Cargo.lock.backup"
active_config=".cargo/config.toml"
template_config=".cargo/config.toml.example"

if [[ ! -d local/codex/codex-rs ]]; then
  echo "expected local Codex checkout at local/codex/codex-rs" >&2
  echo "clone it first, for example: git clone https://github.com/openai/codex.git local/codex" >&2
  exit 1
fi

if [[ -e "$lock_backup_path" ]]; then
  echo "found existing local Codex backup state in $backup_dir" >&2
  echo "run scripts/reset-local-codex.sh first, or inspect and remove $backup_dir manually" >&2
  exit 1
fi

if [[ -f "$active_config" ]] && cmp -s "$active_config" "$template_config"; then
  echo "local Codex override is already active via $active_config"
  exit 0
fi

if [[ -f "$active_config" ]]; then
  echo ".cargo/config.toml already exists and is not the managed local Codex override." >&2
  echo "strict managed mode does not merge with custom cargo config; move it aside or use a clean worktree first." >&2
  exit 1
fi

if [[ -n "$(git status --porcelain -- Cargo.lock)" ]]; then
  echo "Cargo.lock has local changes; refusing to enable local Codex override." >&2
  echo "Commit/stash them first, or reset Cargo.lock before running this script." >&2
  exit 1
fi

mkdir -p .cargo
mkdir -p "$backup_dir"

cp Cargo.lock "$lock_backup_path"
cp "$template_config" "$active_config"

echo "enabled local Codex override via .cargo/config.toml"
echo "note: local override can rewrite Cargo.lock while active"
echo "run scripts/reset-local-codex.sh before committing or opening a PR"
