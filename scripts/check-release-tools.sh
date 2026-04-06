#!/usr/bin/env bash

set -euo pipefail

missing=0

for tool in cargo-deny cargo-about; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "missing required release tool: $tool" >&2
    missing=1
  fi
done

if [[ "$missing" -ne 0 ]]; then
  echo "install them first, e.g.:" >&2
  echo "  cargo install cargo-deny" >&2
  echo "  cargo install cargo-about --version 0.8.4" >&2
  exit 1
fi
