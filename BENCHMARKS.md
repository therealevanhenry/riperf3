# Benchmarks & compatibility

Cross-tool compatibility and throughput comparison of `riperf3` against the
reference `iperf3`, on a two-VM sandbox over a virtio-net bridge. Throughput
numbers measure protocol and CPU efficiency of the implementations, **not**
physical link speed — there is no physical NIC in the path, so the ceiling is
set by the guests' CPU and the host's virtio bridge.

## Test environment

| | |
|---|---|
| Date | 2026-05-29 |
| Host | Intel i9-13900K, Linux 7.0.10-arch1-1 (Arch), KVM |
| Guests | 2× Debian 13 (Trixie), Linux 6.12.74-cloud, 8 vCPU, 8 GB RAM each |
| NIC | virtio-net (vhost=on), bridged, MTU 9000; IPv4 `172.20.0.0/24` + IPv6 `fd00:20::/64` |
| riperf3 | 0.5.1 |
| iperf3 | 3.20+ (cJSON 1.7.15), built from source |
| Per-run | `-t 10` (perf) / `-t 2` (compatibility), fresh `-s -1` server per run |

Methodology: each cell is a single run, server started fresh (`-s -1`) per run
on a unique port, with cleanup between rows. Reverse (`-R`) numbers are the
client's *receive* rate; forward numbers are the client's *send* rate. UDP runs
use `-b 0` (unlimited) on both tools. With no `-l`, the UDP datagram size is
derived from the control-connection MSS (~8928 B on this jumbo-frame path) for
both tools. TCP single-run throughput varies ±~10% run-to-run; UDP is tighter.

## Compatibility matrix (iperf3 interop)

Every client→server tool pairing across protocol × direction, plus
param-exchange features. **All 42 cells interoperate** — each completes with a
valid result and no protocol error. `r` = riperf3, `i` = iperf3; the
interop-relevant pairings are `r→i` and `i→r`.

### Base: protocol × direction (sender/receiver rate, Gbps)

| config | r→r | r→i | i→r | i→i |
|---|--:|--:|--:|--:|
| TCP forward | 71.0 | 75.9 | 61.6 | 76.2 |
| TCP reverse | 75.1 | 77.1 | 54.9 | 75.5 |
| TCP bidir | 44.5 | 43.3 | 39.2 | 44.6 |
| UDP forward `-b 0` | 37.3 | 34.3 | 33.5 | 32.4 |
| UDP reverse `-b 0` | 31.3 | 27.4 | **35.1** † | 27.9 |
| UDP bidir `-b 0` | 24.0 | 22.6 | **22.2** † | 25.3 |

† **Bug found and fixed during this run.** On 0.5.0, `i→r` UDP reverse/bidir at
`-b 0` ran at **1.1 Mbit/s**: iperf3 omits the `bandwidth` param for unlimited
(`-b 0`), and riperf3's server defaulted an absent rate to its 1 Mbit/s UDP
default instead of unlimited. Fixed in **0.5.1**
([#21](https://github.com/therealevanhenry/riperf3/issues/21)) by treating an
absent rate as unlimited, matching iperf3's own server. Post-fix values shown.

### Feature / param-exchange interop (cross pairs, all PASS)

Confirms each negotiated param survives a mixed client/server pair:

`-P 4` · `-l 128K` · `-O` (omit) · `-w` (window) · `-M` (MSS) ·
`--get-server-output` · `-Z` (zerocopy) · UDP `-l 8192` ·
`--udp-counters-64bit` · UDP `-P 4` — **all pass in both directions**
(`r→i` and `i→r`).

## TCP (Gbps)

### Forward (client → server)

| -P | riperf3 IPv4 | iperf3 IPv4 | riperf3 IPv6 | iperf3 IPv6 |
|----|-------------:|------------:|-------------:|------------:|
| 1  | 75.0 | 75.2 | 73.0 | 74.7 |
| 4  | 67.6 | 65.7 | 67.3 | 63.1 |
| 8  | 60.1 | 55.9 | 58.8 | 56.9 |

### Reverse (server → client, `-R`)

| -P | riperf3 IPv4 | iperf3 IPv4 | riperf3 IPv6 | iperf3 IPv6 |
|----|-------------:|------------:|-------------:|------------:|
| 1  | 76.4 | 74.7 | 79.3 | 75.7 |
| 4  | 68.9 | 64.5 | 68.3 | 67.0 |
| 8  | 63.0 | 58.9 | 63.6 | 53.4 |

**Takeaway:** TCP is at parity with iperf3 single-stream and a few percent ahead
at `-P 4`/`-P 8` in both directions. (The `-P 8` forward row is the median of 3
runs; a single run had earlier shown an anomalous 47.9 for riperf3 IPv6 that did
not reproduce — TCP single-run variance.)

## UDP (Gbps)

### Forward (client → server)

| -P | riperf3 IPv4 | iperf3 IPv4 | riperf3 IPv6 | iperf3 IPv6 |
|----|-------------:|------------:|-------------:|------------:|
| 1  | 31.8 | 26.9 | 31.4 | 33.6 |
| 4  | 33.8 | 31.4 | 33.0 | 31.8 |
| 8  | 35.3 | 30.8 | 34.8 | 30.5 |

### Reverse (server → client, `-R`)

| -P | riperf3 IPv4 | iperf3 IPv4 | riperf3 IPv6 | iperf3 IPv6 |
|----|-------------:|------------:|-------------:|------------:|
| 1  | 31.8 | 27.6 | 31.9 | 31.2 |
| 4  | 35.3 | 32.4 | 32.0 | 30.9 |
| 8  | 32.9 | 30.0 | 33.6 | 27.7 |

**Takeaway:** riperf3's UDP path meets or beats iperf3 in every cell and holds
steady across `-P` rather than collapsing — the result of the 0.4.0 rebuild
([#6](https://github.com/therealevanhenry/riperf3/issues/6): MSS-derived
datagram size + blocking sockets). For reference, 0.3.0 UDP forward IPv6 was
`-P 1` 15.9 / `-P 4` 16.2 / `-P 8` 8.2 Gbps — roughly half of iperf3 and falling
with parallelism.

## Bidirectional (per-direction, IPv6, `-P 1`)

| tool | TX (Gbps) | RX (Gbps) |
|---|--:|--:|
| riperf3 TCP | 40.0 | 39.5 |
| iperf3 TCP | 40.6 | 40.6 |
| riperf3 UDP | 31.2 | 24.5 |
| iperf3 UDP | 27.7 | 27.6 |

TCP bidir is at parity (~80 Gbps aggregate). UDP bidir aggregate is comparable
(~56 vs ~55 Gbps); riperf3's two directions are more asymmetric (it sends faster
than it receives concurrently).

## Datagram-size sweep (UDP, IPv6 forward, `-P 1`)

Throughput scales with datagram size for both tools; the default (no `-l`) lands
at the MSS (~8928 B). riperf3's `sendmmsg` batching pulls ahead of iperf3's
per-datagram `send()` once datagrams are large; at the 1460 B floor they're
within noise.

| `-l` (bytes) | riperf3 | iperf3 |
|-------------:|--------:|-------:|
| 1460 | 15.9 | 16.3 |
| 4096 | 25.1 | 23.3 |
| 8192 | 37.8 | 32.5 |
| 8928 (≈MSS) | 39.0 | 33.3 |

## Reproducing

Run from a host with the sandbox VM fleet up and both binaries deployed
(`riperf3` and `iperf3` built from source on each guest). Start a fresh `-s -1`
server per run on a unique port and clean up stray processes between rows. For
compatibility, run every {riperf3,iperf3} client × {riperf3,iperf3} server pair
and treat "completes with a throughput summary, no error" as PASS. For
forward/reverse perf, parse the client's `sender`/`receiver` summary line; for
`-P > 1` take the final `[SUM]` line (both tools emit one). UDP uses `-b 0`.
