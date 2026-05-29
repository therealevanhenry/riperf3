#!/usr/bin/env bash
# Build a pinned iperf3 from source — per the project's build-from-source policy
# (never distro packages), so the interop gate tests an exact, known iperf3.
#
# Clones the given tag, verifies the checkout against an expected commit SHA when
# one is supplied (a tag is mutable; pinning the SHA keeps the "known iperf3"
# from silently changing under the gate), bootstraps autotools, and compiles a
# static-libiperf binary (no shared-lib wrapper, so it runs from the build tree).
# All build chatter and the resolved SHA go to stderr; the absolute path to the
# built `iperf3` is printed to stdout so callers can `BIN=$(build-iperf3.sh ...)`.
#
# Usage:   build-iperf3.sh <version-tag> <workdir> [expected-commit-sha]
# Example: build-iperf3.sh 3.12 /tmp/iperf3 e61aaf8c95df956cefbc54fab7b3d78914664180
set -euo pipefail

version="${1:?usage: build-iperf3.sh <version-tag> <workdir> [expected-sha]}"
workdir="${2:?usage: build-iperf3.sh <version-tag> <workdir> [expected-sha]}"
expected_sha="${3:-}"
src="$workdir/iperf-$version"
bin="$src/src/iperf3"

if [ ! -x "$bin" ]; then
    rm -rf "$src"
    mkdir -p "$workdir"
    git clone --depth 1 --branch "$version" https://github.com/esnet/iperf.git "$src" >&2
fi

# Verify the checkout BEFORE the (expensive) build, so a moved tag fails fast.
sha="$(git -C "$src" rev-parse HEAD)"
echo "iperf3 $version checked out at $sha" >&2
if [ -n "$expected_sha" ] && [ "$sha" != "$expected_sha" ]; then
    echo "ERROR: iperf3 $version resolved to $sha, expected $expected_sha (tag moved?)" >&2
    exit 1
fi

if [ ! -x "$bin" ]; then
    (
        cd "$src"
        ./bootstrap.sh
        ./configure --disable-shared
        make -j"$(nproc 2>/dev/null || echo 2)"
    ) >&2
fi

# Sanity: the binary must actually run (catches a partial/corrupt build that
# still left an executable behind).
"$bin" --version >&2
echo "$bin"
