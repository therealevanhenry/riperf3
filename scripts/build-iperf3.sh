#!/usr/bin/env bash
# Build a pinned iperf3 from source — per the project's build-from-source policy
# (never distro packages), so the interop gate tests an exact, known iperf3.
#
# Clones the given tag, bootstraps autotools, and compiles a static-libiperf
# binary (no shared-lib wrapper, so it runs from the build tree). All build
# chatter goes to stderr; the absolute path to the built `iperf3` is printed to
# stdout so callers can `BIN=$(build-iperf3.sh ...)`.
#
# Usage:   build-iperf3.sh <version-tag> <workdir>
# Example: build-iperf3.sh 3.12 /tmp/iperf3
set -euo pipefail

version="${1:?usage: build-iperf3.sh <version-tag> <workdir>}"
workdir="${2:?usage: build-iperf3.sh <version-tag> <workdir>}"
src="$workdir/iperf-$version"
bin="$src/src/iperf3"

if [ ! -x "$bin" ]; then
    rm -rf "$src"
    mkdir -p "$workdir"
    git clone --depth 1 --branch "$version" https://github.com/esnet/iperf.git "$src" >&2
    (
        cd "$src"
        ./bootstrap.sh
        ./configure --disable-shared
        make -j"$(nproc 2>/dev/null || echo 2)"
    ) >&2
fi

echo "$bin"
