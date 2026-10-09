#!/bin/bash
# Smoke test against the real `fm` on this Mac. Run by hand before a release.
#   scripts/real_fm_smoke.sh                  facts and tools (about 1 min)
#   scripts/real_fm_smoke.sh sessions         several sessions and a long summarise (about 4 min)
#   scripts/real_fm_smoke.sh facts tools sessions
# Needs Apple Silicon, macOS 27 with Apple Intelligence on, and python3.
set -euo pipefail
cd "$(dirname "$0")/.."

if [ "$(uname -m)" != arm64 ]; then echo "needs Apple Silicon" >&2; exit 1; fi
command -v python3 >/dev/null || { echo "needs python3 (xcode-select --install)" >&2; exit 1; }

if [ -z "${FM_MCP_BINARY:-}" ]; then
  cargo build --release --quiet
fi
exec python3 -I scripts/smoke.py "$@"
