#!/usr/bin/env bash
# Bootstrap a full Herdr test environment on a fresh ephemeral machine.
#
# Containers used by Claude Code on the web are reclaimed after inactivity, so
# every session otherwise starts with no Zig, no nextest, no `just`, and no
# graphics stack — and a binary then gets shipped having only been checked
# against unit assertions. This script installs the exact toolchain the test
# suites need so a real-terminal verification pass is actually possible.
#
#   ./scripts/bootstrap_test_env.sh          # install everything
#   ./scripts/bootstrap_test_env.sh --check  # report only, install nothing
#
# What each piece is for:
#   zig 0.15.2      build.rs compiles vendored libghostty-vt with it (pinned).
#   cargo-nextest   `just test` uses it; `cargo test` is NOT a substitute
#                   (see CLAUDE.md - shared-process XDG env tests).
#   just            the project's recipe runner.
#   kitty + Xvfb    real-terminal graphics verification harnesses under data/.
#   imagemagick     screenshot capture/analysis in those harnesses.
set -uo pipefail

CHECK_ONLY=0
[ "${1:-}" = "--check" ] && CHECK_ONLY=1

ZIG_VERSION="0.15.2"
missing=0

have() { command -v "$1" >/dev/null 2>&1; }

report() {
  printf '%-16s %s\n' "$1" "$2"
}

need() {
  # need <name> <version-probe-cmd>
  if have "$1"; then
    report "$1" "ok  $($2 2>&1 | head -1)"
  else
    report "$1" "MISSING"
    missing=$((missing + 1))
  fi
}

status() {
  echo "=== herdr test environment ==="
  need cargo "cargo --version"
  need zig "zig version"
  need just "just --version"
  need cargo-nextest "cargo-nextest --version"
  need python3 "python3 --version"
  need kitty "kitty --version"
  need Xvfb "echo present"
  need convert "convert --version"
  echo "=============================="
}

if [ "$CHECK_ONLY" = 1 ]; then
  status
  exit $(( missing > 0 ))
fi

# --- zig -------------------------------------------------------------------
# ziglang.org is commonly blocked by sandbox egress policy; the official builds
# are also published to PyPI, which is usually reachable.
if ! have zig || [ "$(zig version 2>/dev/null)" != "$ZIG_VERSION" ]; then
  echo "installing zig $ZIG_VERSION"
  pip install --quiet "ziglang==$ZIG_VERSION" || echo "WARN: pip install ziglang failed"
  zbin="$(python3 -c "import ziglang,os;print(os.path.join(os.path.dirname(ziglang.__file__),'zig'))" 2>/dev/null)"
  if [ -n "$zbin" ] && [ -f "$zbin" ]; then
    chmod +x "$zbin"
    printf '#!/bin/sh\nexec %s "$@"\n' "$zbin" > /usr/local/bin/zig
    chmod +x /usr/local/bin/zig
  fi
fi

# --- cargo tools -----------------------------------------------------------
have cargo-nextest || { echo "installing cargo-nextest"; cargo install cargo-nextest --locked; }
have just || { echo "installing just"; cargo install just --locked; }

# --- graphics verification stack -------------------------------------------
if ! have kitty || ! have Xvfb || ! have convert; then
  echo "installing graphics stack"
  apt-get update -qq 2>/dev/null
  DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
    kitty imagemagick xvfb x11-utils xauth fonts-dejavu-core 2>&1 | tail -3
fi

status

cat <<'NOTE'

NOTE: `cargo build` additionally needs the vendored libghostty-vt Zig
dependencies. The required one is fetched from deps.files.ghostty.org, which
is blocked by egress policy in some sandboxes. If the build fails with
"bad HTTP response code: '403'" or "'405 Method Not Allowed'", that host must
be allowlisted for the session -- it cannot be worked around locally.
NOTE
