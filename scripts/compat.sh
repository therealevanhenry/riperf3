#!/usr/bin/env bash
# riperf3 <-> iperf3 compatibility matrix over the two-VM sandbox bridge.
#
# Complements interop.sh (which proves wire-interop over loopback in CI, where
# throughput is meaningless) by exercising every client->server tool pairing
# VM-to-VM, so each cell reports a real cross-bridge throughput AND a PASS/FAIL.
# This is the "compat" half of the benchmark; bench.sh is the statistical
# "campaign" half. See BENCHMARKS.md.
#
# For each pairing {r->r, r->i, i->r, i->i} it runs the base protocol x direction
# grid (TCP/UDP forward/reverse/bidir), then a set of param-exchange feature
# spot-checks on the cross pairings (r->i, i->r) that the same-tool controls
# can't catch. A cell PASSES if the transfer completes and moved data.
#
# Usage:   compat.sh <riperf3-remote-path> <iperf3-remote-path>
# Env:     DURATION (default 4), SERVER_SSH, CLIENT_SSH, SERVER_V4, PORT_BASE
set -uo pipefail

RIPERF3="${1:?usage: compat.sh <riperf3-remote-path> <iperf3-remote-path>}"
IPERF3="${2:?usage: compat.sh <riperf3-remote-path> <iperf3-remote-path>}"
DURATION="${DURATION:-4}"
SERVER_SSH="${SERVER_SSH:-sandbox-server-1}"
CLIENT_SSH="${CLIENT_SSH:-sandbox-client-1}"
SERVER_V4="${SERVER_V4:-172.20.0.20}"
PORT_BASE="${PORT_BASE:-5601}"
RUN_TIMEOUT=$((DURATION + 20))

SSH_CTL="$(mktemp -d)"
trap 'rm -rf "$SSH_CTL"' EXIT
SSH_OPTS=(-o ControlMaster=auto -o "ControlPath=$SSH_CTL/%r@%h:%p" -o ControlPersist=120 \
          -o BatchMode=yes -o ConnectTimeout=10)
ssh_server() { ssh "${SSH_OPTS[@]}" "$SERVER_SSH" "$@"; }
ssh_client() { ssh "${SSH_OPTS[@]}" "$CLIENT_SSH" "$@"; }
bin_for() { [ "$1" = r ] && echo "$RIPERF3" || echo "$IPERF3"; }

port=$PORT_BASE
next_port() { port=$((port + 1)); echo "$port"; }

wait_listen() {
    local p="$1" i
    for i in $(seq 1 50); do
        ssh_server "ss -Hltn 'sport = :$p' | grep -q LISTEN" 2>/dev/null && return 0
        sleep 0.1
    done
    return 1
}

# Representative throughput (Gbps) from a -J doc on stdin. $1=1 for bidir.
# A unidirectional flow reports the same bytes under both sum_sent and
# sum_received (and UDP also under sum), so summing them double/triple-counts;
# take a single perspective. For --bidir the two are distinct directions, so
# their sum is the real aggregate. Prints "ERR" if nothing positive parses.
extract_gbps() {
    python3 -c '
import sys, json
bidir = len(sys.argv) > 1 and sys.argv[1] == "1"
try:
    end = json.load(sys.stdin)["end"]
    def g(k): return end.get(k, {}).get("bits_per_second", 0.0)
    if bidir:
        bps = g("sum_sent") + g("sum_received") or g("sum")
    else:
        bps = g("sum_received") or g("sum_sent") or g("sum")
    print(f"{bps/1e9:.1f}" if bps > 0 else "ERR")
except Exception:
    print("ERR")
' "$1"
}

# Run one cell: <client_tool r|i> <server_tool r|i> <"flags for client"> -> echoes
# Gbps on success, "FAIL" otherwise. Server is the one-shot, shell-backgrounded
# pattern bench.sh uses (riperf3 -D is broken; nohup works for both tools).
run_cell() {
    local ctool="$1" stool="$2" cflags="$3" p
    p="$(next_port)"
    local sbin cbin; sbin="$(bin_for "$stool")"; cbin="$(bin_for "$ctool")"
    ssh_server "nohup $sbin -s -1 -p $p >/dev/null 2>&1 </dev/null &" >/dev/null 2>&1
    if ! wait_listen "$p"; then echo FAIL; return 1; fi
    local g bidir=0
    [[ "$cflags" == *"--bidir"* ]] && bidir=1
    g="$(ssh_client "timeout $RUN_TIMEOUT $cbin -c $SERVER_V4 -p $p -t $DURATION $cflags -J" 2>/dev/null | extract_gbps "$bidir")"
    if [ "$g" = ERR ] || [ -z "$g" ]; then
        ssh_server "pkill -f \"$sbin -s -1 -p $p\"" >/dev/null 2>&1
        echo FAIL; return 1
    fi
    echo "$g"
}

echo "compat matrix over the bridge ($SERVER_SSH <- $CLIENT_SSH), -t ${DURATION}s"
echo "  riperf3=$RIPERF3"
echo "  iperf3 =$IPERF3"
echo

# Base grid: rows = protocol x direction, cols = the four pairings.
declare -a CONFIGS=(
    "TCP forward|"
    "TCP reverse|-R"
    "TCP bidir|--bidir"
    "UDP forward|-u -b 0"
    "UDP reverse|-u -b 0 -R"
    "UDP bidir|-u -b 0 --bidir"
)
PAIRS=("r r" "r i" "i r" "i i")
PAIR_HDR=("r→r" "r→i" "i→r" "i→i")

printf "| %-18s | %6s | %6s | %6s | %6s |\n" "config (Gbps)" "${PAIR_HDR[@]}"
printf "|%s|%s|%s|%s|%s|\n" "--------------------" "-------:" "-------:" "-------:" "-------:"
fail=0
for cfg in "${CONFIGS[@]}"; do
    label="${cfg%%|*}"; flags="${cfg#*|}"
    row="| $(printf '%-18s' "$label") |"
    for pair in "${PAIRS[@]}"; do
        set -- $pair
        res="$(run_cell "$1" "$2" "$flags")"
        [ "$res" = FAIL ] && fail=$((fail + 1))
        row="$row $(printf '%6s' "$res") |"
    done
    echo "$row"
done

# Feature spot-checks on the cross pairings only (same-tool can't catch a wire
# constant that's wrong against real iperf3). Each must PASS in both directions.
echo
echo "Feature interop (cross pairs r→i / i→r, each must PASS):"
declare -a FEATURES=(
    "-P 4"
    "-l 128K"
    "-O 2"
    "-w 256K"
    "-M 1400"
    "--get-server-output"
    "-Z"
    "-u -b 0 -l 8192"
    "-u -b 0 --udp-counters-64bit"
    "-u -b 0 -P 4"
)
for feat in "${FEATURES[@]}"; do
    ri="$(run_cell r i "$feat")"; ir="$(run_cell i r "$feat")"
    rs=PASS; [ "$ri" = FAIL ] && { rs=FAIL; fail=$((fail + 1)); }
    is=PASS; [ "$ir" = FAIL ] && { is=FAIL; fail=$((fail + 1)); }
    printf "  %-32s r→i %-4s  i→r %-4s\n" "$feat" "$rs" "$is"
done

echo
if [ "$fail" -eq 0 ]; then
    echo "RESULT: all cells interoperate (0 failures)"
else
    echo "RESULT: $fail FAILED cell(s)"
fi
[ "$fail" -eq 0 ]
