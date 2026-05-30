# Benchmarks & compatibility

Cross-tool compatibility and a statistically-rigorous throughput comparison of
`riperf3` against the reference `iperf3`, on a two-VM sandbox over a virtio-net
bridge. Throughput numbers measure protocol and CPU efficiency of the
implementations, **not** physical link speed — there is no physical NIC in the
path, so the ceiling is set by the guests' CPU and the host's virtio bridge.

Reproducible from the committed harness in [`scripts/`](scripts): `compat.sh`
(interop grid), `bench.sh` + `analyze.py` (statistical campaign), and
`interop.sh` (the loopback CI gate). See [Reproducing](#reproducing).

## Test environment

| | |
|---|---|
| Date | 2026-05-30 |
| Host | Intel i9-13900K, Linux 7.0.10-arch1-1 (Arch), KVM |
| Guests | 2× Debian 13 (Trixie), Linux 6.12.90-cloud, 8 vCPU, 8 GB RAM each |
| NIC | virtio-net (vhost=on), bridged, MTU 9000; IPv4 `172.20.0.0/24` + IPv6 `fd00:20::/64` |
| riperf3 | 0.6.0 |
| iperf3 | 3.20+ (cJSON 1.7.15), built from source |

## Compatibility matrix (iperf3 interop)

Every client→server tool pairing across protocol × direction, plus
param-exchange features. **All 44 cells interoperate** — each completes with a
valid result and no protocol error. `r` = riperf3, `i` = iperf3; the
interop-relevant pairings are `r→i` and `i→r`.

### Base: protocol × direction (Gbps; bidir is two-way aggregate)

| config | r→r | r→i | i→r | i→i |
|---|--:|--:|--:|--:|
| TCP forward | 72.1 | 70.9 | 73.2 | 74.2 |
| TCP reverse | 75.2 | 75.1 | 73.8 | 75.0 |
| TCP bidir | 81.0 | 84.3 | 82.9 | 82.0 |
| UDP forward `-b 0` | 34.9 | 35.9 | 31.4 | 32.7 |
| UDP reverse `-b 0` | 30.4 | 26.8 | 30.7 | 27.4 |
| UDP bidir `-b 0` | 51.5 | 55.0 | 38.8 | 53.3 |

Feature interop (cross pairs `r→i` and `i→r`, all PASS): `-P 4`, `-l 128K`, `-O`
(omit), `-w` (window), `-M` (MSS), `--get-server-output`, `-Z` (zerocopy), UDP
`-l 8192`, `--udp-counters-64bit`, UDP `-P 4`.

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
either tool. 2 warm-ups per cell discarded; fresh `-s -1` server per run on a
unique port; hard `timeout` wrappers; VMs confirmed idle and isolated for the
duration. **0 failed runs** (1 transient blip auto-recovered on retry). Per-cell
coefficient of variation was 1.0–7.0%. Significance is Welch's t (two-sided,
normal approx at n=30); "parity" = not significant at p<0.05.

### Throughput: riperf3 vs iperf3 (mean Gbps [95% CI])

| cell | riperf3 | iperf3 | Δ | p | verdict |
|---|--:|--:|--:|--:|---|
| TCP fwd P1 v4 | 73.7 [72.8–74.5] | 73.8 [73.2–74.4] | −0.2% | 0.81 | parity |
| TCP fwd P1 v6 | 74.4 [73.6–75.2] | 72.8 [71.2–74.4] | +2.2% | 0.09 | parity |
| TCP fwd P8 v4 | 61.9 [61.6–62.2] | 57.6 [57.3–57.9] | +7.5% | <1e-4 | **riperf3** |
| TCP fwd P8 v6 | 62.4 [62.0–62.7] | 57.8 [57.4–58.2] | +7.9% | <1e-4 | **riperf3** |
| TCP rev P1 v4 | 74.8 [73.5–76.1] | 74.6 [73.8–75.4] | +0.3% | 0.79 | parity |
| TCP rev P1 v6 | 76.3 [75.7–76.9] | 74.8 [73.4–76.1] | +2.1% | 0.03 | **riperf3** |
| TCP rev P8 v4 | 62.4 [61.9–62.9] | 59.5 [58.8–60.1] | +5.0% | <1e-4 | **riperf3** |
| TCP rev P8 v6 | 63.3 [63.0–63.5] | 59.9 [59.5–60.3] | +5.7% | <1e-4 | **riperf3** |
| UDP fwd P1 v4 | 35.3 [34.4–36.1] | 31.1 [30.3–31.8] | +13.6% | <1e-4 | **riperf3** |
| UDP fwd P1 v6 | 34.8 [34.1–35.6] | 30.7 [29.9–31.5] | +13.5% | <1e-4 | **riperf3** |
| UDP fwd P8 v4 | 34.1 [33.4–34.9] | 29.2 [28.9–29.6] | +16.8% | <1e-4 | **riperf3** |
| UDP fwd P8 v6 | 33.1 [32.6–33.7] | 28.8 [28.4–29.2] | +15.1% | <1e-4 | **riperf3** |
| UDP rev P1 v4 | 34.3 [33.5–35.2] | 30.2 [29.5–30.8] | +13.7% | <1e-4 | **riperf3** |
| UDP rev P1 v6 | 34.2 [33.4–35.1] | 30.7 [30.1–31.3] | +11.4% | <1e-4 | **riperf3** |
| UDP rev P8 v4 | 31.7 [31.3–32.0] | 28.9 [28.5–29.3] | +9.6% | <1e-4 | **riperf3** |
| UDP rev P8 v6 | 32.3 [31.9–32.7] | 28.5 [28.1–28.9] | +13.1% | <1e-4 | **riperf3** |

**Findings.**
- **TCP single-stream is a statistical dead heat** (P1, both directions, both
  families: Δ within ±2.2%). Both ~74–76 Gbps. Three of four cells are not
  significant; the one marginal exception (TCP rev P1 v6, +2.1%, p=0.03) is a
  hair above the threshold and inside the noise of the rest.
- **TCP multi-stream: riperf3 significantly faster** at P8 (+5.0% to +7.9%,
  p<1e-4) — its thread-per-stream model scales better on the 8-vCPU guests.
- **UDP: riperf3 significantly faster in every cell** (+9.6% to +16.8%,
  p<1e-4), the result of the 0.4.0 UDP rebuild ([#6](https://github.com/therealevanhenry/riperf3/issues/6): MSS-derived datagram size + blocking sockets).
- No cell where iperf3 is significantly faster (13 riperf3, 3 parity, 0 iperf3).

### UDP loss (%) at `-b 0`, P8

| direction | riperf3 | iperf3 |
|---|--:|--:|
| forward (server receives) | 1.3–5.5 | 0.5–2.2 |
| reverse (server sends) | 1.0–2.7 | 0.4–1.3 |

UDP loss at `-b 0` is receiver-side socket-buffer overflow on a saturated link —
kernel `RcvbufErrors` on the receiving host, while sender `SndbufErrors` stay 0
(the sender never drops). It is roughly symmetric by direction; if anything,
forward drops a touch more. riperf3 loses somewhat more than iperf3 in both
directions because it pushes ~10–17% more throughput, so it overruns the
receiver's buffer harder — higher goodput, slightly higher loss, a
characteristic rather than a regression.

> **Correction (0.5.4).** Earlier editions reported forward as "0.00 / loss-free"
> and attributed the gap to a `sendmmsg`-vs-per-packet sender split. Both were
> wrong. riperf3 had a reporting bug — forward UDP never printed the
> server-measured receiver loss, so it *looked* loss-free
> ([#25](https://github.com/therealevanhenry/riperf3/issues/25), fixed in 0.5.4)
> — and the campaign never passed `--sendmmsg`, so both directions used the same
> per-packet sender. Kernel counters confirm forward drops the same ~1–5% as
> reverse; the "asymmetry" was measurement, not packet loss.

## Single-run supplements

### Bidirectional (per direction, IPv6, P1)

| tool | TX | RX |
|---|--:|--:|
| riperf3 TCP | 39.2 | 39.2 |
| iperf3 TCP | 41.5 | 41.5 |
| riperf3 UDP | 27.0 | 21.9 |
| iperf3 UDP | 29.3 | 26.2 |

TCP bidir aggregate is close (~78–83 Gbps; iperf3 edges it a few percent in this
single run); UDP bidir aggregate is likewise comparable. These are single
non-campaign runs — directional, not statistical.

### UDP datagram-size sweep (IPv6 forward, P1)

Throughput scales with datagram size; the default (no `-l`) lands at the MSS
(~8928 B). riperf3's UDP path — blocking sockets and MSS-derived datagram sizing
([#6](https://github.com/therealevanhenry/riperf3/issues/6)) — pulls ahead once
datagrams are large.

| `-l` (bytes) | riperf3 | iperf3 |
|-------------:|--------:|-------:|
| 1460 | 15.7 | 16.5 |
| 4096 | 24.3 | 23.2 |
| 8192 | 37.8 | 32.4 |
| 8928 (≈MSS) | 37.0 | 33.3 |

## Reproducing

The harness lives in [`scripts/`](scripts) and drives the two-VM sandbox over
SSH (server on `sandbox-server-1`, client on `sandbox-client-1`, data crossing
the bridge). With both binaries built on the VMs at
`~/riperf3/target/release/riperf3` and `~/iperf/src/iperf3`:

```bash
# Compatibility grid + feature spot-checks (cross-tool, over the bridge):
./scripts/compat.sh "$RIPERF3" "$IPERF3"

# Statistical campaign (~2h: 64 warmup + 960 measured, randomized, seeded):
N=30 WARMUP=2 DURATION=5 PROTOS="TCP UDP" SEED=20260530 \
  ./scripts/bench.sh "$RIPERF3" "$IPERF3" campaign.csv
./scripts/analyze.py campaign.csv      # per-cell CIs, UDP loss, Welch's-t verdicts
```

`bench.sh` samples N randomized iterations/cell to a CSV; `analyze.py` computes
the per-cell CIs and Welch's-t verdicts above. UDP uses `-b 0`. Direction-aware
parse: forward → client `sum_sent`, reverse → client `sum_received`, UDP →
`sum`; `-P>1` aggregates are already summed in `-J`. For pure wire-interop
without a sandbox, `./scripts/interop.sh <riperf3-bin> <iperf3-bin>` is the
loopback CI gate. The `riperf3-matrix` skill wraps these scripts with the
provisioning steps and VM-fleet isolation rules.
