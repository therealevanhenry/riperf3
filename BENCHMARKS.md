# Benchmarks

Throughput comparison of `riperf3` against the reference `iperf3`, run on a
two-VM sandbox over a virtio-net bridge. These numbers measure protocol and
CPU efficiency of the implementations, **not** physical link speed — there is
no physical NIC in the path, so the ceiling is set by the guests' CPU and the
host's virtio bridge.

## Test environment

| | |
|---|---|
| Date | 2026-05-28 |
| Host | Intel i9-13900K, Linux 7.0.10-arch1-1 (Arch), KVM |
| Guests | 2× Debian 13 (Trixie), Linux 6.12.74-cloud, 8 vCPU, 8 GB RAM each |
| NIC | virtio-net (vhost=on), bridged, MTU 9000; IPv4 `172.20.0.0/24` + IPv6 `fd00:20::/64` |
| riperf3 | 0.3.0 + #6 fix (`perf/udp-sender-throughput`; ships as 0.4.0) |
| iperf3 | 3.20+ (cJSON 1.7.15), built from source |
| Per-run | `-t 10` (10 s), client→server unless noted |

Methodology: each cell is a single 10 s run, server started fresh (`-s -1`) per
run on a unique port, with cleanup between rows. Reverse (`-R`) numbers are the
client's *receive* rate; forward numbers are the client's *send* rate. UDP runs
target `-b 100G` for riperf3 / `-b 0` for iperf3 (both "send as fast as
possible"). With no `-l`, the UDP datagram size is now derived from the
control-connection MSS (~8928 B on this jumbo-frame path) for both tools — see
the datagram-size sweep below.

## TCP (Gbps)

### Forward (client → server)

| -P | riperf3 IPv4 | iperf3 IPv4 | riperf3 IPv6 | iperf3 IPv6 |
|----|-------------:|------------:|-------------:|------------:|
| 1  | 74.3 | 75.2 | 77.3 | 73.9 |
| 4  | 67.3 | 66.2 | 67.1 | 64.9 |
| 8  | 61.1 | 58.0 | 61.9 | 57.8 |

### Reverse (server → client, `-R`)

| -P | riperf3 IPv4 | iperf3 IPv4 | riperf3 IPv6 | iperf3 IPv6 |
|----|-------------:|------------:|-------------:|------------:|
| 1  | 75.6 | 77.3 | 77.3 | 76.9 |
| 4  | 70.3 | 66.8 | 70.0 | 63.9 |
| 8  | 62.5 | 59.4 | 62.0 | 58.5 |

**Takeaway:** TCP is unchanged by the UDP work ([#6](https://github.com/therealevanhenry/riperf3/issues/6))
and remains at parity with iperf3 — within run-to-run noise single-stream, and a
few percent ahead at `-P 4`/`-P 8` in both directions.

## UDP (Gbps)

### Forward (client → server)

| -P | riperf3 IPv4 | iperf3 IPv4 | riperf3 IPv6 | iperf3 IPv6 |
|----|-------------:|------------:|-------------:|------------:|
| 1  | 34.5 | 31.7 | 38.8 | 34.0 |
| 4  | 33.4 | 32.1 | 34.3 | 31.5 |
| 8  | 35.4 | 30.8 | 35.4 | 30.7 |

### Reverse (server → client, `-R`)

| -P | riperf3 IPv4 | iperf3 IPv4 | riperf3 IPv6 | iperf3 IPv6 |
|----|-------------:|------------:|-------------:|------------:|
| 1  | 31.8 | 28.5 | 33.6 | 31.6 |
| 4  | 32.0 | 31.0 | 36.4 | 32.2 |
| 8  | 33.0 | 29.9 | 33.6 | 30.0 |

**Takeaway:** riperf3's UDP path now meets or beats iperf3 in every cell, and
total throughput holds steady across `-P` instead of collapsing. At `-P 8` it
sustains ~35 Gbps forward (vs ~30.7 for iperf3) **at 0.00% loss**, where iperf3
shows ~0.5–0.6% loss. This closes
[#6](https://github.com/therealevanhenry/riperf3/issues/6).

For reference, 0.3.0 (before the fix) on this same fleet, UDP forward IPv6:
`-P 1` 15.9 Gbps, `-P 4` 16.2 Gbps, `-P 8` 8.2 Gbps — i.e. roughly half of
iperf3 single-stream and *falling* with parallelism. Two changes fixed it:
the default datagram size now tracks the control-socket MSS (it was pinned at
1460 B), and the UDP sender/receiver sockets are now blocking, so the sender
backpressures in-kernel instead of busy-spinning on `EAGAIN` (which had
starved the runtime and contended for CPU as `-P` rose).

### Datagram-size sweep (IPv6 forward, `-P 1`)

Throughput scales with datagram size for both tools; the default (no `-l`)
lands at the MSS (~8928 B). riperf3's `sendmmsg` batching pulls ahead of
iperf3's per-datagram `send()` once datagrams are large; at the 1460 B floor
the two are within noise.

| `-l` (bytes) | riperf3 | iperf3 |
|-------------:|--------:|-------:|
| 1460 | 15.9 | 16.3 |
| 4096 | 25.1 | 23.3 |
| 8192 | 37.8 | 32.5 |
| 8928 (≈MSS) | 39.0 | 33.3 |

### Cross-compatibility (IPv6 forward, `-P 1`, default `-l`)

Mixed client/server pairs confirm wire compatibility is preserved — the
MSS-derived datagram size is carried in the param-exchange `len` field exactly
as before:

| client → server | sender rate |
|---|---:|
| riperf3 → iperf3 | 39.0 Gbps |
| iperf3 → riperf3 | 33.6 Gbps |

## Not measured

- **Bidirectional (`--bidir`)** is omitted: the two tools label per-direction
  summary lines differently, so a like-for-like aggregate needs a
  direction-aware parser this harness doesn't yet have. Spot-checked TCP
  `--bidir -P 1` shows ~43 Gbps *per direction* for both tools (≈86 Gbps
  aggregate) — parity. (The earlier `riperf3 -u --bidir -P 8 -b 100G` hang was
  fixed in 0.3.0, [#5](https://github.com/therealevanhenry/riperf3/issues/5).)

## Reproducing

Run from a host with the sandbox VM fleet up and both binaries deployed
(`riperf3` and `iperf3` built from source on each guest). Start a fresh
`-s -1` server per run on a unique port and clean up stray processes between
rows. For forward/reverse, parse the client's `sender`/`receiver` summary line
respectively; for `-P > 1` take the final `[SUM]` line (both tools emit one).
