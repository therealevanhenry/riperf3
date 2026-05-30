#!/usr/bin/env bash
# riperf3 <-> iperf3 wire-interop matrix over loopback.
#
# Runs every client->server pairing of {riperf3, iperf3} across protocol x
# direction x parallel x feature spot-checks, asserting each test COMPLETES and
# moves data in both aggregates (sum_sent and sum_received > 0). This catches the
# protocol-interop regressions the in-repo tests cannot: those are riperf3<->
# riperf3 only, so a wire constant wrong against real iperf3 still passes them.
# Scope: it proves the handshake / param-exchange / transfer path interoperates;
# it does NOT assert JSON field-level parity (counter/schema fidelity is #36).
# Interop pairings are r->i and i->r; r->r and i->i are controls. Throughput is
# meaningless here (loopback).
#
# Exit status is non-zero if any case fails.
#
# Usage:   interop.sh <riperf3-bin> <iperf3-bin>
# Env:     INTEROP_DURATION  seconds per test (default 2)
set -uo pipefail

RIPERF3="${1:?usage: interop.sh <riperf3-bin> <iperf3-bin>}"
IPERF3="${2:?usage: interop.sh <riperf3-bin> <iperf3-bin>}"
# The two binaries must differ — tag() maps each path to its r/i column, so the
# same path twice would silently render every pairing as a control.
if [ "$RIPERF3" = "$IPERF3" ]; then
    echo "error: riperf3 and iperf3 binaries must differ (got '$RIPERF3' twice)" >&2
    exit 2
fi
DUR="${INTEROP_DURATION:-2}"
# Space-separated list of "name|col" keys (e.g. "udp_rev|[r->i]") for cells with a
# tracked, KNOWN-broken interop bug, tolerated either way (pass OR fail never reds
# the gate). Empty by default — no cell is currently xfailed (#48 is fixed). Reserved
# for a future racy/known-broken interop bug: set it per iperf3 version in the CI
# workflow, then remove the entry once fixed so the cell is required again.
XFAIL="${XFAIL:-}"
XFAIL_REASON="${XFAIL_REASON:-tracked}"
HOST=127.0.0.1
port=5202
workdir="$(mktemp -d)"
trap 'rm -rf "$workdir"' EXIT

# Per-attempt I/O sinks (fixed paths, reused per case). -J JSON (stdout) and
# diagnostics (stderr) stay SEPARATE: a stray stderr line merged into the JSON
# sink would make the parser choke and score a good transfer as a false FAIL.
# Both sides emit JSON now (the riperf3 server gained a -J path in #50), so the
# server's output (sj) is validated too — catching server-side-only regressions
# (e.g. the #23/#48 teardown class) that leave the client JSON looking fine.
cj="$workdir/c.json"; cerr="$workdir/c.err"
sj="$workdir/s.json"; serr="$workdir/s.err"

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

# Succeeds (exit 0) iff the client JSON shows no error AND both end-of-test
# aggregates moved data (sum_sent.bytes > 0 and sum_received.bytes > 0). Requiring
# both — not "some bytes somewhere" — catches a one-sided transfer (a real interop
# break) instead of scoring it green. The per-direction bidir split
# (sum_*_bidir_reverse) is intentionally NOT required: riperf3 doesn't emit it yet
# (#36), while the sender/receiver aggregates are populated by both tools in every
# direction. Dependency-free (python3, no jq) so it runs the same locally and in CI.
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
def aggregate_bytes(key):
    v = end.get(key)
    return v["bytes"] if isinstance(v, dict) and isinstance(v.get("bytes"), (int, float)) else 0
sent = aggregate_bytes("sum_sent")
recv = aggregate_bytes("sum_received")
if sent > 0 and recv > 0:
    sys.exit(0)
print(f"  no bidirectional transfer: sum_sent={sent} sum_received={recv}", file=sys.stderr)
sys.exit(2)
PY
}

# Succeeds iff the server's -J output is valid JSON, reports no error, and shows
# the server moved data on the side it measured. Unlike the client check, the
# server only ever populates ONE direction's aggregate (forward → sum_received,
# reverse → sum_sent; bidir populates both — see #50), so require sum_sent>0 OR
# sum_received>0, not both. This catches a server that crashed, emitted broken
# JSON, or tore the connection down with no data (the #23/#48 class) — failures
# invisible in the client JSON alone.
valid_server_result() {
    python3 - "$1" <<'PY'
import json, sys
try:
    d = json.load(open(sys.argv[1]))
except Exception as e:
    print("  server json parse failed:", e, file=sys.stderr)
    sys.exit(1)
if isinstance(d, dict) and d.get("error"):
    print("  server reported error:", d["error"], file=sys.stderr)
    sys.exit(1)
end = d.get("end", {}) if isinstance(d, dict) else {}
def aggregate_bytes(key):
    v = end.get(key)
    return v["bytes"] if isinstance(v, dict) and isinstance(v.get("bytes"), (int, float)) else 0
sent = aggregate_bytes("sum_sent")
recv = aggregate_bytes("sum_received")
if sent > 0 or recv > 0:
    sys.exit(0)
print(f"  server moved no data: sum_sent={sent} sum_received={recv}", file=sys.stderr)
sys.exit(2)
PY
}

# attempt <client-bin> <server-bin> <client-opts...>
# One server/client round on a fresh port. Returns 0 iff the test completed with
# bidirectional transfer. Writes cj/cerr (client) and sj/serr (server).
attempt() {
    local client="$1" server="$2"
    shift 2
    local p=$((port++))
    "$server" -s -1 -p "$p" -J >"$sj" 2>"$serr" &
    local spid=$!
    if ! wait_port "$p"; then
        kill "$spid" 2>/dev/null
        wait "$spid" 2>/dev/null
        echo "server never listened on port $p" >"$cerr"
        return 1
    fi
    # -k escalates to SIGKILL if the client ignores SIGTERM; the budget scales
    # with the test duration so a hang costs seconds rather than a fixed ceiling.
    timeout -k 5 "$((DUR + 15))" "$client" -c "$HOST" -p "$p" -J "$@" >"$cj" 2>"$cerr"
    local rc=$?
    # The one-off server prints its JSON and exits on its own once the test ends.
    # Let it finish so $sj is complete before we validate it (a premature kill
    # would truncate the JSON and score a false FAIL); only force-kill if it
    # overruns, i.e. hung.
    local w
    for w in $(seq 1 50); do
        kill -0 "$spid" 2>/dev/null || break
        sleep 0.1
    done
    kill "$spid" 2>/dev/null
    wait "$spid" 2>/dev/null
    [ "$rc" -eq 0 ] && valid_result "$cj" && valid_server_result "$sj"
}

# run_case <name> <client-bin> <server-bin> <client-opts...>
run_case() {
    local name="$1" client="$2" server="$3"
    shift 3
    local col="[$(tag "$client")->$(tag "$server")]"

    local ok=1
    if attempt "$client" "$server" "$@"; then ok=0; fi

    # Retry a single transient failure once: UDP -b0 floods on loopback are
    # inherently lossy/racy under back-to-back load, so an isolated failure may
    # not reproduce. A deterministic failure — a real interop break, or a tracked
    # xfail — fails both attempts. Don't spend the retry on a known xfail.
    if [ "$ok" -ne 0 ] && ! is_xfail "$name|$col"; then
        printf 'RETRY %-26s %s  (transient failure — re-running once)\n' "$name" "$col"
        if attempt "$client" "$server" "$@"; then ok=0; fi
    fi

    if is_xfail "$name|$col"; then
        # Tolerated known-broken cell: report the outcome but never red the gate.
        # The bug is racy (the #48 teardown reset is timing-dependent), so a pass
        # doesn't mean "fixed", just that the race went the other way. NOTE this
        # also means a NEW, unrelated breakage in the cell stays hidden — keep the
        # XFAIL list minimal, tied to a tracked issue, and removed once fixed (so
        # a regression reds the gate again).
        if [ "$ok" -eq 0 ]; then
            printf 'XPASS %-26s %s  (known-flaky %s; passed this run)\n' "$name" "$col" "$XFAIL_REASON"
        else
            printf 'XFAIL %-26s %s  (known: %s)\n' "$name" "$col" "$XFAIL_REASON"
        fi
        xfail=$((xfail + 1))
        return
    fi

    if [ "$ok" -eq 0 ]; then
        printf 'PASS  %-26s %s\n' "$name" "$col"
        pass=$((pass + 1))
    else
        printf 'FAIL  %-26s %s\n' "$name" "$col"
        # The validators already print the reason (parse error / no transfer) to
        # stderr above. These show the raw output for context. The -J files are
        # pretty JSON whose end-of-test aggregates sit near the bottom, so show
        # enough of the tail to include the `end` block, not just closing braces.
        sed 's/^/    client    | /' "$cj" | tail -25
        sed 's/^/    client-err| /' "$cerr" | tail -4
        sed 's/^/    server    | /' "$sj" | tail -25
        sed 's/^/    server-err| /' "$serr" | tail -4
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
