#!/usr/bin/env bash
# riperf3-vs-iperf3 throughput campaign over the two-VM sandbox bridge.
#
# Reconstructs the statistical campaign documented in BENCHMARKS.md: a replicated,
# randomized head-to-head so the comparison is defensible rather than anecdotal.
# Throughput here measures protocol + CPU efficiency of the implementations, NOT
# physical link speed — the path is a virtio-net bridge between two guests, no NIC.
#
# Design: 32 cells = {TCP,UDP} x {forward,reverse} x {P1,P8} x {IPv4,IPv6} x
# {riperf3,iperf3}, each tool head-to-head against itself. N measured runs per
# cell (default 30), preceded by WARMUP discarded runs per cell. All measured
# (cell, iter) tuples are executed in RANDOMIZED order so host/thermal drift
# can't systematically favor either tool. Each run gets a fresh `-s -1` one-shot
# server on a unique port; a hard `timeout` wraps every client. Rows stream to a
# CSV consumed by analyze.py.
#
# Orchestrated from the host: the server runs on the server VM, the client on the
# client VM, data crosses the bridge VM<->VM. JSON (-J) is captured from the
# client over ssh and parsed here, so the guests need no jq/python.
#
# Usage:   bench.sh <riperf3-remote-path> <iperf3-remote-path> <out.csv>
# Env:
#   N=30                 measured runs per cell
#   WARMUP=2             discarded warmup runs per cell
#   DURATION=5           seconds per run (-t)
#   SERVER_SSH=sandbox-server-1   ssh alias for the server VM
#   CLIENT_SSH=sandbox-client-1   ssh alias for the client VM
#   SERVER_V4=172.20.0.20         server address the client dials (IPv4)
#   SERVER_V6=fd00:20::20         server address the client dials (IPv6)
#   PORT_BASE=5301       first ephemeral test port (incremented per run)
#   PROTOS="TCP UDP"     restrict protocols (smoke: PROTOS=TCP)
#   SEED=                optional integer seed for reproducible shuffle order
set -uo pipefail

RIPERF3="${1:?usage: bench.sh <riperf3-remote-path> <iperf3-remote-path> <out.csv>}"
IPERF3="${2:?usage: bench.sh <riperf3-remote-path> <iperf3-remote-path> <out.csv>}"
OUT="${3:?usage: bench.sh <riperf3-remote-path> <iperf3-remote-path> <out.csv>}"

N="${N:-30}"
WARMUP="${WARMUP:-2}"
DURATION="${DURATION:-5}"
SERVER_SSH="${SERVER_SSH:-sandbox-server-1}"
CLIENT_SSH="${CLIENT_SSH:-sandbox-client-1}"
SERVER_V4="${SERVER_V4:-172.20.0.20}"
SERVER_V6="${SERVER_V6:-fd00:20::20}"
PORT_BASE="${PORT_BASE:-5301}"
PROTOS="${PROTOS:-TCP UDP}"
SEED="${SEED:-}"

# Per-run client wall-clock ceiling: the test itself plus connect/teardown slack.
RUN_TIMEOUT=$((DURATION + 20))

# Reuse one ssh connection per VM for the whole campaign (960+ invocations) so we
# pay the TCP/auth handshake once, not per run. Sockets live under a temp dir.
SSH_CTL="$(mktemp -d)"
trap 'rm -rf "$SSH_CTL"' EXIT
SSH_OPTS=(-o ControlMaster=auto -o "ControlPath=$SSH_CTL/%r@%h:%p" -o ControlPersist=300 \
          -o BatchMode=yes -o ConnectTimeout=10)

ssh_server() { ssh "${SSH_OPTS[@]}" "$SERVER_SSH" "$@"; }
ssh_client() { ssh "${SSH_OPTS[@]}" "$CLIENT_SSH" "$@"; }

# Pull the direction-correct aggregate throughput (Gbps) plus retransmits, UDP
# loss, and host/remote CPU out of one iperf3/riperf3 -J document on stdin.
#   forward (client sends)  -> end.sum_sent      (TCP) / end.sum (UDP)
#   reverse (-R, client rx) -> end.sum_received  (TCP) / end.sum (UDP)
# Emits: "<gbps> <retransmits> <lost_percent> <host_cpu> <remote_cpu>" or "ERR".
# The program is passed via -c (not stdin) so the piped JSON is free to be read
# by json.load(sys.stdin); proto/direction arrive as argv.
extract() {
    python3 -c '
import sys, json
proto, direction = sys.argv[1], sys.argv[2]
try:
    d = json.load(sys.stdin)
    end = d["end"]
    if proto == "UDP":
        s = end["sum"]
        bps = s["bits_per_second"]
        retr = 0
        loss = s.get("lost_percent", 0.0)
    else:
        s = end["sum_sent"] if direction == "forward" else end["sum_received"]
        bps = s["bits_per_second"]
        retr = end.get("sum_sent", {}).get("retransmits", 0)
        loss = 0.0
    cpu = end.get("cpu_utilization_percent", {})
    host = cpu.get("host_total", 0.0)
    rem = cpu.get("remote_total", 0.0)
    print(f"{bps/1e9:.4f} {retr} {loss:.4f} {host:.2f} {rem:.2f}")
except Exception:
    print("ERR")
' "$1" "$2"
}

# Wait until <port> is in LISTEN on the server VM (TCP control socket exists for
# both TCP and UDP tests). Connect-probing would consume the one-shot server's
# single accept, so we poll `ss` instead. Returns non-zero if it never appears.
wait_listen() {
    local port="$1" i
    for i in $(seq 1 50); do
        if ssh_server "ss -Hltn 'sport = :$port' | grep -q LISTEN" 2>/dev/null; then
            return 0
        fi
        sleep 0.1
    done
    return 1
}

bin_for() { [ "$1" = riperf3 ] && echo "$RIPERF3" || echo "$IPERF3"; }
addr_for() { [ "$1" = v4 ] && echo "$SERVER_V4" || echo "$SERVER_V6"; }

# One measured (or warmup) run. Echoes a CSV line on success, nothing on failure
# (caller counts the gap). Args: tool proto dir parallel family iter port.
do_run() {
    local tool="$1" proto="$2" dir="$3" par="$4" fam="$5" iter="$6" port="$7"
    local bin addr pflag dirflag uflag
    bin="$(bin_for "$tool")"
    addr="$(addr_for "$fam")"
    [ "$par" = P8 ] && pflag="-P 8" || pflag=""
    [ "$dir" = reverse ] && dirflag="-R" || dirflag=""
    [ "$proto" = UDP ] && uflag="-u -b 0" || uflag=""

    # Fresh one-shot server on a unique port; -1 makes it exit after this single
    # client, so no lingering listener collides with the next run. Started via
    # shell backgrounding (nohup, fds detached) rather than the tool's own -D:
    # riperf3 0.6.0's -D daemon mode hangs the client (filed separately), and
    # nohup-backgrounding works identically for both tools.
    ssh_server "nohup $bin -s -1 -p $port >/dev/null 2>&1 </dev/null &" >/dev/null 2>&1
    if ! wait_listen "$port"; then
        ssh_server "pkill -f \"$bin -s -1 -p $port\"" >/dev/null 2>&1
        return 1
    fi

    local json metric
    json="$(ssh_client "timeout $RUN_TIMEOUT $bin -c $addr -p $port -t $DURATION $dirflag $pflag $uflag -J" 2>/dev/null)"
    metric="$(printf '%s' "$json" | extract "$proto" "$dir")"
    if [ "$metric" = ERR ] || [ -z "$metric" ]; then
        ssh_server "pkill -f \"$bin -s -1 -p $port\"" >/dev/null 2>&1
        return 1
    fi
    # cell,tool,proto,dir,parallel,family,iter,gbps,retransmits,lost_percent,host_cpu,remote_cpu
    local cell="${proto}_${dir}_${par}_${fam}"
    echo "$cell,$tool,$proto,$dir,$par,$fam,$iter,$metric" | tr ' ' ','
}

echo "campaign: N=$N warmup=$WARMUP duration=${DURATION}s protos='$PROTOS'" >&2
echo "  server=$SERVER_SSH ($SERVER_V4 / $SERVER_V6)  client=$CLIENT_SSH" >&2
echo "  riperf3=$RIPERF3" >&2
echo "  iperf3 =$IPERF3" >&2

echo "cell,tool,proto,dir,parallel,family,iter,gbps,retransmits,lost_percent,host_cpu,remote_cpu" > "$OUT"

# Enumerate the cell space, then the full measured job list (cell x iter).
cells=()
for proto in $PROTOS; do
    for dir in forward reverse; do
        for par in P1 P8; do
            for fam in v4 v6; do
                for tool in riperf3 iperf3; do
                    cells+=("$tool $proto $dir $par $fam")
                done
            done
        done
    done
done

# Warmups: WARMUP runs of every cell, discarded. Caches/CC state settle before
# any measured sample is taken.
echo "warmup: ${#cells[@]} cells x $WARMUP ..." >&2
for cell in "${cells[@]}"; do
    for ((w=0; w<WARMUP; w++)); do
        do_run $cell 0 $((PORT_BASE)) >/dev/null
    done
done

# Build measured job list, then shuffle so (cell, iter) execution order is random
# across the whole campaign — drift can't track a single tool/cell.
jobs=()
for cell in "${cells[@]}"; do
    for ((i=1; i<=N; i++)); do
        jobs+=("$cell $i")
    done
done
if [ -n "$SEED" ]; then
    mapfile -t jobs < <(printf '%s\n' "${jobs[@]}" | shuf --random-source=<(yes "$SEED"))
else
    mapfile -t jobs < <(printf '%s\n' "${jobs[@]}" | shuf)
fi

total="${#jobs[@]}"
echo "measured: $total runs (randomized)" >&2
done_n=0; ok=0; fail=0; retried=0; port=$PORT_BASE
for job in "${jobs[@]}"; do
    # Up to 3 attempts per measured run (fresh port each), so a transient blip
    # (rare listen race / UDP setup hiccup) doesn't drop a cell's sample. The
    # campaign target is 0 unrecovered failures.
    got=0
    for attempt in 1 2 3; do
        port=$((port + 1)); [ "$port" -gt 65000 ] && port=$PORT_BASE
        if line="$(do_run $job "$port")"; then
            echo "$line" >> "$OUT"
            ok=$((ok + 1)); got=1
            [ "$attempt" -gt 1 ] && retried=$((retried + 1))
            break
        fi
    done
    if [ "$got" -eq 0 ]; then
        fail=$((fail + 1))
        echo "  FAIL run (3 attempts): $job" >&2
    fi
    done_n=$((done_n + 1))
    if (( done_n % 32 == 0 )); then
        echo "  progress: $done_n/$total (ok=$ok fail=$fail retried=$retried)" >&2
    fi
done

echo "done: $ok ok, $fail failed, $retried recovered-on-retry -> $OUT" >&2
[ "$fail" -eq 0 ]
