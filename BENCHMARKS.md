# Benchmarks & compatibility

Cross-tool compatibility and a statistically-rigorous throughput comparison of
`riperf3` against the reference `iperf3`, on a two-VM sandbox over a virtio-net
bridge. Throughput numbers measure protocol and CPU efficiency of the
implementations, **not** physical link speed — there is no physical NIC in the
path, so the ceiling is set by the guests' CPU and the host's virtio bridge.

Reproducible via the `riperf3-matrix` skill (`compat` and `campaign` modes).

## Test environment

| | |
|---|---|
| Date | 2026-05-29 |
| Host | Intel i9-13900K, Linux 7.0.10-arch1-1 (Arch), KVM |
| Guests | 2× Debian 13 (Trixie), Linux 6.12.74-cloud, 8 vCPU, 8 GB RAM each |
| NIC | virtio-net (vhost=on), bridged, MTU 9000; IPv4 `172.20.0.0/24` + IPv6 `fd00:20::/64` |
| riperf3 | 0.5.1 |
| iperf3 | 3.20+ (cJSON 1.7.15), built from source |

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
| UDP reverse `-b 0` | 31.5 | 28.1 | 31.7 | 27.1 |
| UDP bidir `-b 0` | 19.9 | 23.7 | 23.2 | 27.2 |

Feature interop (cross pairs, all PASS): `-P 4`, `-l 128K`, `-O` (omit), `-w`
(window), `-M` (MSS), `--get-server-output`, `-Z` (zerocopy), UDP `-l 8192`,
`--udp-counters-64bit`, UDP `-P 4`.

> The earlier 1 Mbit/s throttle on `i→r` UDP reverse/bidir at `-b 0` — iperf3
> omits the `bandwidth` param for unlimited and riperf3's server defaulted it to
> 1 Mbit/s — was fixed in 0.5.1 ([#21](https://github.com/therealevanhenry/riperf3/issues/21)).

## Performance — statistical campaign

A replicated campaign rather than single runs, so the riperf3-vs-iperf3
comparison is defensible rather than anecdotal.

**Design.** 32 cells = {TCP, UDP} × {forward, reverse} × {P1, P8} × {IPv4, IPv6}
× {riperf3, iperf3}, each tool head-to-head against itself. **N = 30** runs per
cell (`-t 5` s each), **960 runs total**, run in **randomized order** across all
(cell, tool, iteration) tuples so host/thermal drift can't systematically favor
either tool. 2 warm-ups discarded; fresh `-s -1` server per run on a unique
port; hard `timeout` wrappers; VMs confirmed idle and isolated for the duration.
0 failed runs. Per-cell coefficient of variation was 1.5–8.5% (TCP reverse P1
highest, ~5–7%; UDP ~3.5–8.5%), giving the tight 95% CIs below. Significance is
Welch's t (two-sided, normal approx at n=30); "parity" = not significant at
p<0.05.

### Throughput: riperf3 vs iperf3 (mean Gbps [95% CI])

| cell | riperf3 | iperf3 | Δ | p | verdict |
|---|--:|--:|--:|--:|---|
| TCP fwd P1 v4 | 74.4 [73.6–75.2] | 74.1 [73.6–74.6] | +0.4% | 0.58 | parity |
| TCP fwd P1 v6 | 75.7 [74.9–76.5] | 75.5 [75.0–76.0] | +0.3% | 0.64 | parity |
| TCP fwd P8 v4 | 61.5 [61.0–62.0] | 56.8 [56.4–57.3] | +8.2% | <1e-4 | **riperf3** |
| TCP fwd P8 v6 | 62.4 [62.1–62.8] | 56.8 [56.2–57.3] | +10.0% | <1e-4 | **riperf3** |
| TCP rev P1 v4 | 73.9 [72.0–75.7] | 75.5 [74.7–76.4] | −2.2% | 0.10 | parity |
| TCP rev P1 v6 | 75.3 [73.9–76.7] | 75.4 [73.8–77.1] | −0.2% | 0.89 | parity |
| TCP rev P8 v4 | 62.1 [61.7–62.5] | 58.7 [57.9–59.5] | +5.8% | <1e-4 | **riperf3** |
| TCP rev P8 v6 | 63.0 [62.7–63.3] | 58.4 [57.8–59.0] | +7.9% | <1e-4 | **riperf3** |
| UDP fwd P1 v4 | 34.8 [33.7–35.8] | 29.8 [29.0–30.6] | +16.8% | <1e-4 | **riperf3** |
| UDP fwd P1 v6 | 35.6 [34.6–36.5] | 30.0 [29.1–31.0] | +18.4% | <1e-4 | **riperf3** |
| UDP fwd P8 v4 | 33.0 [32.6–33.4] | 29.4 [28.9–29.8] | +12.4% | <1e-4 | **riperf3** |
| UDP fwd P8 v6 | 33.0 [32.4–33.5] | 29.2 [28.9–29.6] | +12.8% | <1e-4 | **riperf3** |
| UDP rev P1 v4 | 33.6 [32.7–34.4] | 29.2 [28.5–30.0] | +14.9% | <1e-4 | **riperf3** |
| UDP rev P1 v6 | 33.0 [32.2–33.8] | 29.1 [28.4–29.8] | +13.3% | <1e-4 | **riperf3** |
| UDP rev P8 v4 | 31.2 [30.8–31.6] | 28.5 [28.1–28.9] | +9.6% | <1e-4 | **riperf3** |
| UDP rev P8 v6 | 31.6 [31.2–32.1] | 28.4 [28.0–28.9] | +11.2% | <1e-4 | **riperf3** |

**Findings.**
- **TCP single-stream is a statistical dead heat** (P1, both directions, both
  families: Δ within ±2.2%, not significant). Both ~75 Gbps.
- **TCP multi-stream: riperf3 significantly faster** at P8 (+5.8% to +10.0%,
  p<1e-4) — its thread-per-stream model scales better on the 8-vCPU guests.
- **UDP: riperf3 significantly faster in every cell** (+9.6% to +18.4%,
  p<1e-4), the result of the 0.4.0 UDP rebuild ([#6](https://github.com/therealevanhenry/riperf3/issues/6): MSS-derived datagram size + blocking sockets).
- No cell where iperf3 is significantly faster.

### UDP loss (%) — directional asymmetry

| direction | riperf3 | iperf3 |
|---|--:|--:|
| forward (server receives) | **0.00** (P8) | 0.8–1.2 |
| reverse (server sends) | **1.2–1.8** | 0.4–0.8 |

Forward, riperf3 is faster **and** loss-free even at P8. Reverse, riperf3 is
faster but drops 2–3× more than iperf3. The asymmetry tracks the sender: forward
uses the client's `sendmmsg` path (loss-free); reverse uses the server's
per-packet blocking sender, which bursts harder into the receiver. Goodput still
favors riperf3 (≈31.1 vs 28.3 Gbps reverse P8 v6), so it's a characteristic, not
a regression — tracked in
[#25](https://github.com/therealevanhenry/riperf3/issues/25).

## Single-run supplements

### Bidirectional (per direction, IPv6, P1)

| tool | TX | RX |
|---|--:|--:|
| riperf3 TCP | 40.0 | 39.5 |
| iperf3 TCP | 40.6 | 40.6 |
| riperf3 UDP | 31.2 | 24.5 |
| iperf3 UDP | 27.7 | 27.6 |

TCP bidir is at parity (~80 Gbps aggregate); UDP bidir aggregate is comparable.

### UDP datagram-size sweep (IPv6 forward, P1)

Throughput scales with datagram size; the default (no `-l`) lands at the MSS
(~8928 B). riperf3's `sendmmsg` batching pulls ahead once datagrams are large.

| `-l` (bytes) | riperf3 | iperf3 |
|-------------:|--------:|-------:|
| 1460 | 15.9 | 16.3 |
| 4096 | 25.1 | 23.3 |
| 8192 | 37.8 | 32.5 |
| 8928 (≈MSS) | 39.0 | 33.3 |

## Reproducing

`/riperf3-matrix compat` for the interop gate; `/riperf3-matrix campaign` for the
statistical throughput run (`bench.sh` samples N randomized iterations/cell to a
CSV; `analyze.py` computes the per-cell CIs and Welch's-t verdicts above). UDP
uses `-b 0`. Direction-aware parse: forward → client `sender` line, reverse →
client `receiver` line, `-P>1` → `[SUM]`. See the skill for the full procedure
and the VM-fleet isolation rules.
