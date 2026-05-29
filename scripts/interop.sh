#!/usr/bin/env bash
# riperf3 <-> iperf3 wire-interop matrix over loopback.
#
# Runs every client->server pairing of {riperf3, iperf3} across protocol x
# direction x parallel x feature spot-checks, asserting each test COMPLETES with
# a valid (non-zero-byte) result. This is the correctness gate the in-repo
# loopback tests cannot provide: those are riperf3<->riperf3 only, so a wire
# constant that is wrong against real iperf3 still passes them. The interop-
# relevant pairings are r->i and i->r; r->r and i->i are controls. Throughput is
# meaningless here (loopback) — only completion + data transfer is asserted.
#
# Exit status is non-zero if any case fails.
#
# Usage:   interop.sh <riperf3-bin> <iperf3-bin>
# Env:     INTEROP_DURATION  seconds per test (default 2)
set -uo pipefail

RIPERF3="${1:?usage: interop.sh <riperf3-bin> <iperf3-bin>}"
IPERF3="${2:?usage: interop.sh <riperf3-bin> <iperf3-bin>}"
DUR="${INTEROP_DURATION:-2}"
# Space-separated list of "name|col" keys (e.g. "udp_rev|[r->i]") that are known
# to fail against this peer and should be tolerated. An xfail that unexpectedly
# PASSES is reported as UPASS and fails the run, forcing promotion to required
# once the underlying bug is fixed. Set per iperf3 version by the CI workflow.
XFAIL="${XFAIL:-}"
XFAIL_REASON="${XFAIL_REASON:-tracked}"
HOST=127.0.0.1
port=5202
workdir="$(mktemp -d)"
trap 'rm -rf "$workdir"' EXIT

pass=0
fail=0
xfail=0
failed_cases=()

echo "riperf3: $("$RIPERF3" -v 2>&1 | head -1)"
echo "iperf3:  $("$IPERF3" --version 2>&1 | head -1)"
echo

# Single-letter tag for the matrix column.
tag() {
    case "$1" in
        "$RIPERF3") echo r ;;
        "$IPERF3") echo i ;;
        *) echo "?" ;;
    esac
}

# True if "name|col" is in the known-xfail list for this peer.
is_xfail() {
    case " $XFAIL " in
        *" $1 "*) return 0 ;;
        *) return 1 ;;
    esac
}

# Wait until the server's TCP control port is in LISTEN state. Uses `ss`, not a
# connect probe: a probe would consume the single connection a `-s -1` one-off
# server accepts, leaving the real client to see "connection refused".
wait_port() {
    local p="$1" i
    for i in $(seq 1 100); do
        if ss -ltn 2>/dev/null | grep -qE ":${p} "; then
            return 0
        fi
        sleep 0.1
    done
    return 1
}

# Succeeds (exit 0) iff the client JSON shows no error and a positive byte count
# in any end-of-test aggregate. Kept dependency-free (python3, no jq) so it runs
# the same locally and in CI.
valid_result() {
    python3 - "$1" <<'PY'
import json, sys
try:
    d = json.load(open(sys.argv[1]))
except Exception as e:
    print("  json parse failed:", e, file=sys.stderr)
    sys.exit(1)
if isinstance(d, dict) and d.get("error"):
    print("  reported error:", d["error"], file=sys.stderr)
    sys.exit(1)
end = d.get("end", {}) if isinstance(d, dict) else {}
best = 0
for k, v in end.items():
    if isinstance(v, dict) and isinstance(v.get("bytes"), (int, float)):
        best = max(best, v["bytes"])
sys.exit(0 if best > 0 else 2)
PY
}

# run_case <name> <client-bin> <server-bin> <client-opts...>
run_case() {
    local name="$1" client="$2" server="$3"
    shift 3
    local copts=("$@")
    local p=$((port++))
    local sj="$workdir/s.json" cj="$workdir/c.json"
    local col="[$(tag "$client")->$(tag "$server")]"

    "$server" -s -1 -p "$p" -J >"$sj" 2>&1 &
    local spid=$!

    if ! wait_port "$p"; then
        kill "$spid" 2>/dev/null
        wait "$spid" 2>/dev/null
        printf 'FAIL  %-26s %s  (server never listened)\n' "$name" "$col"
        fail=$((fail + 1))
        failed_cases+=("$name $col")
        return
    fi

    timeout 40 "$client" -c "$HOST" -p "$p" -J "${copts[@]}" >"$cj" 2>&1
    local rc=$?
    kill "$spid" 2>/dev/null
    wait "$spid" 2>/dev/null

    local ok=1
    if [ "$rc" -eq 0 ] && valid_result "$cj"; then ok=0; fi

    if is_xfail "$name|$col"; then
        if [ "$ok" -eq 0 ]; then
            printf 'UPASS %-26s %s  (xfail but PASSED — bug fixed? promote to required)\n' "$name" "$col"
            fail=$((fail + 1))
            failed_cases+=("UPASS $name $col")
        else
            printf 'XFAIL %-26s %s  (known: %s)\n' "$name" "$col" "$XFAIL_REASON"
            xfail=$((xfail + 1))
        fi
        return
    fi

    if [ "$ok" -eq 0 ]; then
        printf 'PASS  %-26s %s\n' "$name" "$col"
        pass=$((pass + 1))
    else
        printf 'FAIL  %-26s %s  (rc=%s)\n' "$name" "$col" "$rc"
        sed 's/^/    | /' "$cj" | tail -4
        fail=$((fail + 1))
        failed_cases+=("$name $col")
    fi
}

# Client option-sets. The server is always plain `-s` — iperf3 negotiates
# protocol/direction/parallel/features from the client's param exchange.
order=(
    tcp_fwd tcp_rev tcp_bidir tcp_p4 tcp_len128k tcp_win256k tcp_mss1400
    tcp_omit tcp_zerocopy tcp_get_output
    udp_fwd udp_rev udp_bidir udp_p4 udp_len8192 udp_64bit
)
declare -A opts=(
    [tcp_fwd]="-t $DUR"
    [tcp_rev]="-R -t $DUR"
    [tcp_bidir]="--bidir -t $DUR"
    [tcp_p4]="-P 4 -t $DUR"
    [tcp_len128k]="-l 128K -t $DUR"
    [tcp_win256k]="-w 256K -t $DUR"
    [tcp_mss1400]="-M 1400 -t $DUR"
    [tcp_omit]="-O 1 -t $DUR"
    [tcp_zerocopy]="-Z -t $DUR"
    [tcp_get_output]="--get-server-output -t $DUR"
    [udp_fwd]="-u -b 0 -t $DUR"
    [udp_rev]="-u -R -b 0 -t $DUR"
    [udp_bidir]="-u --bidir -b 0 -t $DUR"
    [udp_p4]="-u -P 4 -b 0 -t $DUR"
    [udp_len8192]="-u -l 8192 -b 0 -t $DUR"
    [udp_64bit]="-u --udp-counters-64bit -b 0 -t $DUR"
)

for name in "${order[@]}"; do
    read -ra o <<<"${opts[$name]}"
    run_case "$name" "$RIPERF3" "$IPERF3" "${o[@]}"  # r->i  (interop)
    run_case "$name" "$IPERF3" "$RIPERF3" "${o[@]}"  # i->r  (interop)
    run_case "$name" "$RIPERF3" "$RIPERF3" "${o[@]}" # r->r  (control)
    run_case "$name" "$IPERF3" "$IPERF3" "${o[@]}"   # i->i  (control)
done

echo
echo "==== interop summary: $pass passed, $xfail xfail, $fail failed ===="
if [ "$fail" -gt 0 ]; then
    printf '  FAIL: %s\n' "${failed_cases[@]}"
    exit 1
fi
