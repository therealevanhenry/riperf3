# Benchmarks & compatibility

Cross-tool compatibility and a statistically-rigorous throughput comparison of
`riperf3` against the reference `iperf3`, on a two-VM sandbox over a virtio-net
bridge. Throughput numbers measure protocol and CPU efficiency of the
implementations, **not** physical link speed — there is no physical NIC in the
path, so the ceiling is set by the guests' CPU and the host's virtio bridge.

Wire-compatibility is independently reproducible from the committed
[`scripts/interop.sh`](scripts/interop.sh) — a loopback gate (no second host)
that also runs in CI. The throughput campaign and the cross-bridge compatibility
matrix below were measured on our two-VM sandbox with internal tooling (the
`riperf3-matrix` skill), not a turnkey script; the method is documented under
[Reproducing](#reproducing) so the numbers stay auditable, but they are
environment-specific.

> **0.7.1 status.** Re-verified at the `0.7.1` patch: the compatibility matrix is
> all-PASS (incl. iperf3 3.12 interop and the `-w 256K` cell that exercises #97) and
> a fresh full N=30 throughput campaign reproduces the **same verdict** as 0.7.0 —
> parity-or-faster vs iperf3 (12 riperf3 / 3 parity / 1 within-noise; the lone noise
> cell is again TCP rev P1 v4, this run −1.8% at p=0.012). 0.7.1 is a batch of iperf3
> output-faithfulness fixes (#100 client server-only rejection, #114/#107 report
> fields, #37 `congestion_used` read-back, #97 `-w` clamp abort) plus internal
> cleanup (#124/#125/#129) — all CLI / reporting / socket-setup, with **zero
> throughput-data-path impact**, so the tables below carry forward from the 0.7.0
> campaign and this re-run confirms them regression-free.
>
> **0.7.0 status.** Re-verified at the final 0.7.0 commit (post the breaking-API
> set): the compatibility matrix is all-PASS and the throughput campaign was
> **re-measured fresh** (full N=30) — the riperf3-vs-iperf3 verdict is stable
> (parity at TCP single-stream, riperf3 faster everywhere else; 12 riperf3 / 3
> parity / 1 within-noise). The absolute Gbps differ from the 0.6.0 edition
> because the sandbox host and guest kernels changed between campaigns — **not** a
> riperf3 change: a controlled **same-environment 0.6.3-vs-0.7.0** run found every
> UDP cell at statistical parity (Δ −2.6% to +0.4%, all p>0.14) with identical
> loss, confirming the 0.7.0 data path is unchanged. 0.7.0's headline change is a
> large library **API narrowing** (field encapsulation #43, internal-module
> retraction #67, `#[non_exhaustive]` hardening, and removal of inert builder
> setters #122) — all compile-time/visibility, zero data-path impact. Its
> runtime fixes (Windows UDP `-P>1` #80; daemon/interval/JSON-stream #81/#55/#62;
> reverse `-n`/`-k`, TCP `-b` pacing and `-n`/`-k` accounting #60/#102/#103/#117)
> touch only rate/byte-limited or non-Linux paths; the default unlimited Linux
> data path is unchanged, as this campaign confirms.

## Test environment

| | |
|---|---|
| Date | 2026-06-08 |
| Host | Intel i9-13900K, Linux 7.0.11-arch1-1 (Arch), KVM |
| Guests | 2× Debian 13 (Trixie), Linux 6.12.90+deb13.1-cloud-amd64, 8 vCPU, 8 GB RAM each |
| NIC | virtio-net (vhost=on), bridged, MTU 9000; IPv4 `172.20.0.0/24` + IPv6 `fd00:20::/64` |
| riperf3 | 0.7.1 |
| iperf3 | 3.20+ (cJSON 1.7.15), built from source |

## Compatibility matrix (iperf3 interop)

Every client→server tool pairing across protocol × direction, plus
param-exchange features and an older-iperf3 (3.12) cross-check. **All 48 cells
interoperate** — each completes with a valid result and no protocol error.
`r` = riperf3, `i` = iperf3, `o` = older iperf3 (3.12); the interop-relevant
pairings are `r→i`/`i→r` (and `r→o`/`o→r` for 3.12). Throughput here is
single-run and illustrative (the rigorous figures are the
[campaign](#performance--statistical-campaign)
below); the column to read is PASS/interop, not the Gbps.

### Base: protocol × direction (Gbps; bidir is per-direction, one way)

| config | r→r | r→i | i→r | i→i |
|---|--:|--:|--:|--:|
| TCP forward | 74.0 | 72.0 | 74.7 | 74.9 |
| TCP reverse | 66.4 | 74.8 | 64.5 | 75.3 |
| TCP bidir | 41.0 | 42.0 | 42.2 | 44.4 |
| UDP forward `-b 0` | 34.9 | 34.9 | 32.3 | 32.2 |
| UDP reverse `-b 0` | 31.9 | 28.1 | 34.6 | 31.5 |
| UDP bidir `-b 0` | 29.6 | 21.4 | 28.4 | 28.0 |

Feature interop (cross pairs `r→i` and `i→r`, all PASS): `-P 4`, `-l 128K`, `-O`
(omit), `-w` (window), `-M` (MSS), `--get-server-output`, `-Z` (zerocopy), UDP
`-l 8192`, `--udp-counters-64bit`, UDP `-P 4`. Plus 4 forward cross-checks
against older iperf3 (3.12) — TCP and UDP, both pairings (`r→o`, `o→r`) —
guarding the results-decode class
([#24](https://github.com/therealevanhenry/riperf3/issues/24)). That makes 24
base + 20 feature + 4 older-iperf3 = 48 cells.

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
duration. **0 failed runs.** Per-cell coefficient of variation was 1.2–8.9%.
Significance is Welch's t (two-sided, normal approx at n=30); "parity" = not
significant at p<0.05.

### Throughput: riperf3 vs iperf3 (mean Gbps [95% CI])

| cell | riperf3 | iperf3 | Δ | p | verdict |
|---|--:|--:|--:|--:|---|
| TCP fwd P1 v4 | 72.8 [72.2–73.4] | 72.5 [71.9–73.2] | +0.4% | 0.57 | parity |
| TCP fwd P1 v6 | 73.6 [73.0–74.2] | 73.4 [72.6–74.2] | +0.3% | 0.68 | parity |
| TCP fwd P8 v4 | 60.0 [59.5–60.4] | 56.2 [55.7–56.8] | +6.6% | <1e-4 | **riperf3** |
| TCP fwd P8 v6 | 60.8 [60.3–61.3] | 56.3 [55.7–56.9] | +8.0% | <1e-4 | **riperf3** |
| TCP rev P1 v4 | 73.3 [72.5–74.2] | 74.5 [74.2–74.9] | −1.6% | 0.009 | iperf3 |
| TCP rev P1 v6 | 74.4 [74.0–74.9] | 74.5 [73.4–75.5] | −0.0% | 0.97 | parity |
| TCP rev P8 v4 | 60.4 [59.8–61.1] | 58.9 [58.7–59.2] | +2.6% | <1e-4 | **riperf3** |
| TCP rev P8 v6 | 61.6 [61.2–62.0] | 59.0 [58.5–59.4] | +4.5% | <1e-4 | **riperf3** |
| UDP fwd P1 v4 | 32.2 [31.3–33.0] | 28.8 [28.1–29.4] | +11.9% | <1e-4 | **riperf3** |
| UDP fwd P1 v6 | 33.6 [32.6–34.7] | 29.4 [28.5–30.3] | +14.3% | <1e-4 | **riperf3** |
| UDP fwd P8 v4 | 31.2 [30.6–31.7] | 28.0 [27.5–28.5] | +11.5% | <1e-4 | **riperf3** |
| UDP fwd P8 v6 | 31.8 [31.3–32.3] | 27.9 [27.4–28.3] | +14.0% | <1e-4 | **riperf3** |
| UDP rev P1 v4 | 30.7 [29.9–31.5] | 27.6 [27.1–28.2] | +11.0% | <1e-4 | **riperf3** |
| UDP rev P1 v6 | 32.4 [31.5–33.3] | 27.9 [27.2–28.5] | +16.3% | <1e-4 | **riperf3** |
| UDP rev P8 v4 | 29.5 [29.1–29.8] | 27.1 [26.7–27.5] | +8.7% | <1e-4 | **riperf3** |
| UDP rev P8 v6 | 30.4 [30.1–30.6] | 27.5 [27.1–28.0] | +10.3% | <1e-4 | **riperf3** |

**Findings.**
- **TCP single-stream is a statistical dead heat** (P1, both directions, both
  families: Δ within ±1.6%). Both ~73–75 Gbps. Three of the four cells are
  parity; the sign of the residual flips run-to-run within the single-stream
  noise band — this campaign measured TCP rev P1 v4 at −1.6% (p=0.009) favoring
  iperf3, where the prior campaign measured the same cell at +2.3% favoring
  riperf3. It is variance, not a data-path difference.
- **TCP multi-stream: riperf3 significantly faster** at P8 (+2.6% to +8.0%,
  p<1e-4) — its thread-per-stream model scales better on the 8-vCPU guests.
- **UDP: riperf3 significantly faster in every cell** (+8.7% to +16.3%,
  p<1e-4), the result of the 0.4.0 UDP rebuild ([#6](https://github.com/therealevanhenry/riperf3/issues/6): MSS-derived datagram size + blocking sockets).
- Tally: 12 riperf3, 3 parity, 1 iperf3 — the lone iperf3 cell is the −1.6%
  single-stream residual above, inside the noise.

### UDP loss (%) at `-b 0`, P8

| direction | riperf3 (mean / max) | iperf3 (mean / max) |
|---|--:|--:|
| forward (server receives) | 3.4 / 6.4 | 0.4 / 1.8 |
| reverse (server sends) | 2.0 / 2.7 | 0.2 / 0.5 |

UDP loss at `-b 0` is receiver-side socket-buffer overflow on a saturated link —
kernel `RcvbufErrors` on the receiving host, while sender `SndbufErrors` stay 0
(the sender never drops). It is roughly symmetric by direction. riperf3 loses
somewhat more than iperf3 in both directions because it pushes ~9–16% more
throughput, so it overruns the receiver's buffer harder — higher goodput,
slightly higher loss, a characteristic rather than a regression (the
0.6.3-vs-0.7.0 control measured the same loss in both versions).

> **Correction (0.5.4).** Earlier editions reported forward as "0.00 / loss-free"
> and attributed the gap to a `sendmmsg`-vs-per-packet sender split. Both were
> wrong. riperf3 had a reporting bug — forward UDP never printed the
> server-measured receiver loss, so it *looked* loss-free
> ([#25](https://github.com/therealevanhenry/riperf3/issues/25), fixed in 0.5.4)
> — and the campaign never passed `--sendmmsg`, so both directions used the same
> per-packet sender. Kernel counters confirm forward drops the same ~1–6% as
> reverse; the "asymmetry" was measurement, not packet loss.

## Single-run supplements

These are single non-campaign runs — directional, not statistical — carried
forward from the 0.6.x edition. The 0.6.3-vs-0.7.0 control confirmed the Linux
data path is unchanged, so they remain representative of 0.7.0.

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

**Wire-compatibility — turnkey, no sandbox.** The committed loopback gate proves
riperf3↔iperf3 wire-interop on a single host; it's the same check CI runs:

```bash
./scripts/interop.sh <riperf3-bin> <iperf3-bin>
```

**Throughput + cross-bridge compatibility matrix — environment-specific.** These
numbers were measured on our two-VM sandbox (two hosts over a real virtio-net
bridge, riperf3 + iperf3 built on each VM) via internal tooling — the
`riperf3-matrix` skill, which drives provisioning, the cross-tool grid, and a
randomized N=30 campaign (per-cell 95% CIs + Welch's-t) under VM-fleet
isolation. That orchestration assumes our sandbox, so it isn't shipped as a
turnkey script. The method is, so the results stay auditable and the campaign is
replicable on any two hosts: N=30 randomized iterations/cell, 2 warm-ups
discarded, a fresh `-s -1` server per run on a unique port, UDP at `-b 0`, and a
direction-aware parse (forward → client `sum_sent`, reverse → `sum_received`,
UDP → `sum`; `-P>1` aggregates already summed in `-J`). Cross-version regression
checks (e.g. 0.6.3-vs-0.7.0) reuse the campaign by pointing the two binaries at
different riperf3 builds in the same run, so environment drift cancels out.
