#!/usr/bin/env bash
#
# xcheck.sh — cross-platform compile-check matrix for riperf3.
#
# Runs `cargo check` for every OS riperf3 claims to support, so cfg-gate / per-OS
# compile divergence (e.g. #78: a code path gated for the wrong set of platforms)
# is caught locally in seconds, with no VM and no CI round-trip.
#
# SCOPE — read this before trusting a green run:
#   `cargo check --target X` type-checks the cfg-gated code FOR that target, so
#   it catches anything that fails to COMPILE on a platform (wrong cfg, missing
#   arm, type mismatch in an OS-specific block). It does NOT link or run, so it
#   CANNOT catch runtime-semantic divergence (e.g. #79 WSAEWOULDBLOCK vs
#   EINPROGRESS, #80 winsock UDP demux) — those compile fine and only fail on a
#   real host. For that class, deploy to a native VM (sandbox-* / mithrandir).
#
# Usage:
#   scripts/xcheck.sh                  # check every target, lib+bins+tests
#   scripts/xcheck.sh --no-tests       # lib+bins only (skip --all-targets)
#   XCHECK_TARGETS="x86_64-unknown-freebsd" scripts/xcheck.sh   # subset
#   scripts/xcheck.sh -- --features foo   # pass extra args through to cargo
#
# Missing rustup targets are auto-added. Host target is checked too (catches
# the embarrassing case where the thing doesn't even build where you are).

set -uo pipefail

# Default: the OSes riperf3 cfg-gates for. Override with XCHECK_TARGETS (space-sep).
DEFAULT_TARGETS=(
    x86_64-unknown-linux-gnu     # host / primary
    x86_64-unknown-linux-musl    # static-linked Linux (CI "musl check")
    x86_64-pc-windows-msvc       # Windows (native winsock)
    x86_64-apple-darwin          # macOS Intel
    aarch64-apple-darwin         # macOS Apple silicon
    x86_64-unknown-freebsd       # FreeBSD
    x86_64-unknown-netbsd        # the "other-Unix" canary (#78 lived here)
)

# shellcheck disable=SC2206
TARGETS=(${XCHECK_TARGETS:-${DEFAULT_TARGETS[*]}})

# --all-targets also checks tests/ (where macOS/Windows cfg gating lives, e.g.
# #72 LOOPBACK_DEV). Drop it with --no-tests for a faster lib+bins-only pass.
CHECK_SCOPE=(--all-targets)
PASSTHRU=()
while [ $# -gt 0 ]; do
    case "$1" in
        --no-tests) CHECK_SCOPE=() ;;
        --) shift; PASSTHRU=("$@"); break ;;
        *) PASSTHRU+=("$1") ;;
    esac
    shift
done

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

declare -A RESULT
overall=0
log_dir="$(mktemp -d)"

for t in "${TARGETS[@]}"; do
    if ! rustup target list --installed 2>/dev/null | grep -qx "$t"; then
        printf '  + rustup target add %s\n' "$t"
        if ! rustup target add "$t" >/dev/null 2>&1; then
            RESULT[$t]="NO-STD (rustup target add failed)"
            overall=1
            continue
        fi
    fi
    printf '==> cargo check --target %-26s ... ' "$t"
    if cargo check --workspace --target "$t" "${CHECK_SCOPE[@]+"${CHECK_SCOPE[@]}"}" \
            "${PASSTHRU[@]+"${PASSTHRU[@]}"}" >"$log_dir/$t.log" 2>&1; then
        RESULT[$t]="ok"
        printf 'ok\n'
    else
        RESULT[$t]="FAIL"
        overall=1
        printf 'FAIL\n'
    fi
done

echo
echo "=== xcheck summary ==="
for t in "${TARGETS[@]}"; do
    printf '  %-26s %s\n' "$t" "${RESULT[$t]:-?}"
done

if [ "$overall" -ne 0 ]; then
    echo
    echo "=== failure logs (last 30 lines each) ==="
    for t in "${TARGETS[@]}"; do
        if [ "${RESULT[$t]:-}" != "ok" ]; then
            echo "--- $t ---"
            tail -n 30 "$log_dir/$t.log" 2>/dev/null
            echo
        fi
    done
    echo "full logs: $log_dir"
else
    rm -rf "$log_dir"
fi

exit "$overall"
