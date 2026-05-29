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
| riperf3 | 0.2.0 |
| iperf3 | 3.20+ (cJSON 1.7.15), built from source |
| Per-run | `-t 10` (10 s), client→server unless noted |

> This is a fuller per-`-P`/per-direction sweep than the summary table in the
> README's Performance section; it's a different run, so absolute numbers
> differ slightly from those round figures.

Methodology: each cell is a single 10 s run, server started fresh (`-s -1`) per
run on a unique port, every invocation wrapped in a hard `timeout` guard, with
cleanup between rows. Reverse (`-R`) numbers are the client's *receive* rate;
forward numbers are the client's *send* rate. UDP runs target `-b 100G` (i.e.
"send as fast as possible").

## TCP (Gbps)

### Forward (client → server)

| -P | riperf3 IPv4 | iperf3 IPv4 | riperf3 IPv6 | iperf3 IPv6 |
|----|-------------:|------------:|-------------:|------------:|
| 1  | 73.4 | 74.8 | 74.7 | 76.2 |
| 4  | 67.1 | 64.0 | 69.6 | 67.5 |
| 8  | 61.6 | 50.8 | 63.5 | 51.0 |

### Reverse (server → client, `-R`)

| -P | riperf3 IPv4 | iperf3 IPv4 | riperf3 IPv6 | iperf3 IPv6 |
|----|-------------:|------------:|-------------:|------------:|
| 1  | 74.4 | 77.4 | 74.8 | 77.5 |
| 4  | 67.3 | 65.8 | 67.1 | 64.9 |
| 8  | 62.1 | 59.6 | 62.2 | 59.3 |

**Takeaway:** riperf3 is at parity with iperf3 single-stream. At `-P 8`
*forward* it pulls ahead ~21–25% (61.6 vs 50.8 Gbps IPv4; 63.5 vs 51.0 IPv6);
at `-P 8` *reverse* the two are roughly at parity (~4–5% ahead). No measurable
IPv4/IPv6 difference for either tool.

## UDP (Gbps)

### Forward (client → server)

| -P | riperf3 IPv4 | iperf3 IPv4 | riperf3 IPv6 | iperf3 IPv6 |
|----|-------------:|------------:|-------------:|------------:|
| 1  | 17.4 | 31.7 | 15.9 | 33.4 |
| 4  | 13.7 | 31.9 | 16.2 | 31.6 |
| 8  |  8.4 | 30.5 |  8.2 | 30.2 |

### Reverse (server → client, `-R`)

| -P | riperf3 IPv4 | iperf3 IPv4 | riperf3 IPv6 | iperf3 IPv6 |
|----|-------------:|------------:|-------------:|------------:|
| 1  | 16.5 | 32.1 | 16.2 | 32.4 |
| 4  | 13.9 | 29.3 | 14.3 | 32.4 |
| 8  |  6.0 | 29.3 |  6.3 | 29.2 |

**Takeaway:** riperf3's UDP send path delivers roughly half of iperf3
single-stream, and total throughput falls as `-P` rises rather than scaling —
from ~17 Gbps at `-P 1` to ~8 Gbps at `-P 8` (per-stream rate craters to
~1 Gbps), while iperf3 holds ~30 Gbps across all `-P`. (`-P 4` is flat-to-noise
vs `-P 1`; the drop is pronounced by `-P 8`.) This is a real efficiency bug in
the busy-spin pacing path, not the memory-safety trade-off described in the
README — tracked in [#6](https://github.com/therealevanhenry/riperf3/issues/6).

## Not measured

- **Bidirectional (`--bidir`)** is omitted: the two tools label per-direction
  summary lines differently, so a like-for-like aggregate needs a
  direction-aware parser this harness doesn't yet have. Spot-checked TCP
  `--bidir -P 1` shows ~43 Gbps *per direction* for both tools (≈86 Gbps
  aggregate) — parity. Additionally, `riperf3 -u --bidir -P 8 -b 100G` hangs
  (does not honor `-t`), tracked in
  [#5](https://github.com/therealevanhenry/riperf3/issues/5).

## Reproducing

Run from a host with the sandbox VM fleet up and both binaries deployed
(`riperf3` and `iperf3` built from source on each guest). Start a fresh
`-s -1` server per run on a unique port, wrap every client and server
invocation in a `timeout` guard, and clean up stray processes between rows.
For forward/reverse, parse the client's `sender`/`receiver` summary line
respectively; sum per-stream lines for `-P > 1` (riperf3 emits no final
`[SUM]` row for TCP — see [#4](https://github.com/therealevanhenry/riperf3/issues/4)).
