# Benchmarks & compatibility

Cross-tool compatibility and a statistically-rigorous throughput comparison of
`riperf3` against the reference `iperf3`, measured on a two-VM KVM sandbox over a
virtio-net bridge. Throughput numbers measure protocol and CPU efficiency of the
implementations, **not** physical link speed — there is no physical NIC in the
path, so the ceiling is set by the guests' CPU and the host's virtio bridge.

Wire-compatibility is independently reproducible from the committed
[`scripts/interop.sh`](scripts/interop.sh) — a loopback gate (no second host)
that also runs in CI. The throughput campaign and the cross-bridge compatibility
matrix are environment-specific; the methodology is documented under
[Reproducing](#reproducing) so the numbers stay auditable and replicable on any
two hosts.

## Per-release history

> **0.8.0.** Compatibility matrix all-PASS (**52/52**, incl. iperf3 3.12
> cross-pairs). Fresh N=30 campaign: **12 riperf3 / 1 parity / 3 iperf3** —
> riperf3 significantly faster in every UDP cell (+10.5% to +19.8%) and every TCP
> `-P8` cell (+4.4% to +9.6%); the three iperf3-favored cells are single-stream
> TCP at −1.8% to −2.5% (the wandering single-stream residual,
> [#174](https://github.com/therealevanhenry/riperf3/issues/174), which has read
> parity or a ±3% one-cell wobble in prior campaigns — at N=30 the ~2% gap clears
> significance rather than noise). **No regression vs 0.7.4**: absolutes moved up
> for both tools in lockstep (iperf3, an unchanged binary, +5.0% to +15.6%;
> riperf3 +6.4% to +13.2% — the same environment shift). 0.8.0 is an
> architecture/API release; its only data-path-adjacent change is the
> authoritative UDP datagram counter (#256), byte-identical on the wire.
>
> **0.7.4.** Matrix all-PASS (**52/52**). N=30 campaign: **12 riperf3 / 4 parity
> / 0 slower** — faster in every UDP cell (+8.7% to +13.9%) and every TCP `-P8`
> cell (+4.1% to +10.2%), TCP single-stream at parity. No regression vs 0.7.3:
> cross-campaign movement was lockstep with the unchanged iperf3 binary, and the
> one shifted cell measured parity in a same-environment A/B. Throughput-relevant
> changes were the terminate-path and reporter end-race redesigns (#230, #159) —
> control plane, not data path.
>
> **0.7.3.** Matrix all-PASS (**52/52**; paced-UDP `-b 100M` cross-pairs joined
> via #206). N=30 campaign: **11 riperf3 / 5 parity / 0 slower**; statistically
> faster-or-equal to 0.7.2 in every cell. Changes were the grow-only UDP
> `SO_SNDBUF` (#163) and the `-b rate/burst` multisend batch (#160), neither on
> the default unlimited path.
>
> **0.7.2.** Matrix all-PASS (48/48, incl. `-O`/`--get-server-output`). N=30
> campaign: **12 riperf3 / 3 parity / 1 trailing** (TCP fwd P1 v4, −3.1%, the
> single-stream residual; a same-environment A/B measured parity). The `-b`
> limiter rewrite (#116) engages only with `-b > 0`; the default data path is
> unchanged.
>
> **0.7.1.** Matrix all-PASS (incl. iperf3 3.12 interop). N=30 campaign
> reproduces the 0.7.0 verdict — parity-or-faster (12 riperf3 / 3 parity / 1
> within-noise). Output-faithfulness fixes (#100/#114/#107/#37/#97) plus internal
> cleanup, with zero data-path impact.
>
> **0.7.0.** Matrix all-PASS; campaign re-measured fresh (N=30) — parity at TCP
> single-stream, riperf3 faster elsewhere (12 riperf3 / 3 parity / 1
> within-noise). A same-environment 0.6.3-vs-0.7.0 control found every UDP cell at
> parity, confirming the data path is unchanged across the 0.7.0 library API
> narrowing (#43/#67/#122).

## Test environment

| | |
|---|---|
| Date | 2026-06-27 |
| Host | Recent high-core-count x86_64 desktop, Linux + KVM |
| Guests | 2× Debian 13 (Trixie), Linux 6.12 cloud kernel, 8 vCPU / 8 GB RAM each |
| NIC | virtio-net (vhost=on), bridged, MTU 9000, dual-stack IPv4/IPv6 |
| riperf3 | 0.8.0 |
| iperf3 | 3.20+ (cJSON 1.7.15), built from source |

## Compatibility matrix (iperf3 interop)

Every client→server tool pairing across protocol × direction, plus
param-exchange features and an older-iperf3 (3.12) cross-check. **All 52 cells
interoperate** — each completes with a valid result and no protocol error.
`r` = riperf3, `i` = iperf3, `o` = older iperf3 (3.12); the interop-relevant
pairings are `r→i`/`i→r` (and `r→o`/`o→r` for 3.12). Throughput here is
single-run and illustrative (the rigorous figures are the
[campaign](#performance--statistical-campaign)
below); the column to read is PASS/interop, not the Gbps.

### Base: protocol × direction (Gbps; bidir is per-direction, one way)

| config | r→r | r→i | i→r | i→i |
|---|--:|--:|--:|--:|
| TCP forward | 74.4 | 75.2 | 74.4 | 71.7 |
| TCP reverse | 77.3 | 73.9 | 77.1 | 77.9 |
| TCP bidir | 41.9 | 42.8 | 42.1 | 40.1 |
| UDP forward `-b 0` | 33.1 | 36.7 | 29.5 | 33.1 |
| UDP reverse `-b 0` | 30.6 | 27.7 | 30.8 | 27.6 |
| UDP bidir `-b 0` | 23.2 | 22.3 | 25.9 | 25.9 |

Feature interop (cross pairs `r→i` and `i→r`, all PASS): `-P 4`, `-l 128K`, `-O`
(omit), `-w` (window), `-M` (MSS), `--get-server-output`, `-Z` (zerocopy), UDP
`-l 8192`, `--udp-counters-64bit`, UDP `-P 4`, and (since 0.7.3) **paced UDP at
`-b 100M`, forward and reverse** — the rate-accuracy class
([#163](https://github.com/therealevanhenry/riperf3/issues/163)) previously had
no cross-tool coverage. Plus 4 forward cross-checks against older iperf3 (3.12)
— TCP and UDP, both pairings (`r→o`, `o→r`) — guarding the results-decode class
([#24](https://github.com/therealevanhenry/riperf3/issues/24)). That makes 24
base + 24 feature + 4 older-iperf3 = 52 cells.

> The earlier 1 Mbit/s throttle on `i→r` UDP reverse/bidir at `-b 0` — iperf3
> omits the `bandwidth` param for unlimited and riperf3's server defaulted it to
> 1 Mbit/s — was fixed in 0.5.1 ([#21](https://github.com/therealevanhenry/riperf3/issues/21)).

## Performance — statistical campaign

A replicated campaign rather than single runs, so the riperf3-vs-iperf3
comparison is defensible rather than anecdotal.

**Design.** 32 cells = {TCP, UDP} × {forward, reverse} × {P1, P8} × {IPv4, IPv6}
× {riperf3, iperf3}, each tool head-to-head against itself. **N = 30** runs per
cell (`-t 5` s each), **960 runs**, run in **randomized order** across all
(cell, tool, iteration) tuples so host/thermal drift can't systematically favor
either tool. 2 warm-ups per cell discarded; a fresh `-s -1` server per run on a
unique port; hard `timeout` wrappers; the VMs idle and isolated for the
duration. All 960 runs completed. Per-cell coefficient of variation was
1.4–7.9%. Significance is Welch's t (two-sided, normal approx); "parity" = not
significant at p<0.05.

### Throughput: riperf3 vs iperf3 (mean Gbps [95% CI])

| cell | riperf3 | iperf3 | Δ | p | verdict |
|---|--:|--:|--:|--:|---|
| TCP fwd P1 v4 | 74.0 [73.3–74.7] | 75.6 [75.1–76.1] | −2.2% | 0.0001 | **iperf3** |
| TCP fwd P1 v6 | 75.1 [74.4–75.7] | 76.5 [76.0–77.0] | −1.8% | 0.0005 | **iperf3** |
| TCP fwd P8 v4 | 63.3 [63.0–63.6] | 58.4 [58.0–58.9] | +8.3% | <1e-4 | **riperf3** |
| TCP fwd P8 v6 | 63.9 [63.4–64.3] | 58.3 [57.6–58.9] | +9.6% | <1e-4 | **riperf3** |
| TCP rev P1 v4 | 75.0 [74.3–75.7] | 76.9 [76.3–77.5] | −2.5% | 0.0001 | **iperf3** |
| TCP rev P1 v6 | 75.0 [73.4–76.6] | 75.1 [73.0–77.2] | −0.2% | 0.93 | parity |
| TCP rev P8 v4 | 63.5 [63.0–63.9] | 60.8 [60.4–61.1] | +4.4% | <1e-4 | **riperf3** |
| TCP rev P8 v6 | 64.2 [63.8–64.7] | 61.0 [60.7–61.3] | +5.2% | <1e-4 | **riperf3** |
| UDP fwd P1 v4 | 34.8 [34.2–35.5] | 31.0 [30.3–31.7] | +12.4% | <1e-4 | **riperf3** |
| UDP fwd P1 v6 | 34.9 [33.9–35.9] | 31.1 [30.4–31.9] | +12.1% | <1e-4 | **riperf3** |
| UDP fwd P8 v4 | 35.4 [34.7–36.1] | 29.6 [29.2–29.9] | +19.8% | <1e-4 | **riperf3** |
| UDP fwd P8 v6 | 33.8 [33.1–34.4] | 29.6 [29.2–30.0] | +14.3% | <1e-4 | **riperf3** |
| UDP rev P1 v4 | 34.0 [33.2–34.8] | 30.4 [29.8–31.1] | +11.8% | <1e-4 | **riperf3** |
| UDP rev P1 v6 | 33.5 [32.7–34.4] | 29.7 [29.1–30.4] | +12.7% | <1e-4 | **riperf3** |
| UDP rev P8 v4 | 32.4 [32.0–32.8] | 29.3 [28.9–29.7] | +10.5% | <1e-4 | **riperf3** |
| UDP rev P8 v6 | 32.7 [32.3–33.0] | 29.2 [28.8–29.5] | +12.0% | <1e-4 | **riperf3** |

### Cross-campaign: 0.8.0 vs the 0.7.4 baseline (Welch per cell, BOTH tools)

Absolutes moved up this campaign — for both tools. iperf3 is an unchanged binary
between the two campaigns, so its shift measures the environment, not riperf3:

| cells | faster | parity | slower | shift range |
|---|--:|--:|--:|---|
| riperf3 0.8.0 vs 0.7.4 baseline | 16 | 0 | 0 | +6.4% to +13.2% |
| iperf3 vs its own 0.7.4-campaign numbers | 16 | 0 | 0 | +5.0% to +15.6% |

Both tools faster in all 16 cells, in lockstep. iperf3 (unchanged) reading +5.0%
to +15.6% can only be the environment; riperf3's near-identical +6.4% to +13.2%
rides the same shift, not a 0.8.0 data-path gain — and 0.8.0 changes no TCP data
path (its one data-path-adjacent change, the UDP datagram counter #256, is
byte-identical on the wire: `datagrams == bytes/blksize` for every riperf3
sender). The same-run head-to-head table above is the controlled comparison;
cross-campaign absolutes are not — the environment moves several percent between
campaigns (see [Reproducing](#reproducing) for the same-environment A/B method
used to settle any off-looking cell).

**Findings.**
- **TCP single-stream reads marginally iperf3-favored this run** (~74–75 vs
  ~75–77 Gbps; three of four cells −1.8% to −2.5% at p<0.05, the fourth parity)
  — the wandering single-stream residual
  ([#174](https://github.com/therealevanhenry/riperf3/issues/174)), parity-or-±3%
  in prior campaigns; at N=30 the ~2% gap clears significance.
- **TCP multi-stream: riperf3 significantly faster in all four P8 cells**
  (+4.4% to +9.6%).
- **UDP: riperf3 significantly faster in every cell** (+10.5% to +19.8%,
  all p<1e-4).
- **No regression vs 0.7.4**: every cell up +6.4% to +13.2%, lockstep with the
  unchanged iperf3 binary (environment), and 0.8.0 touches no TCP data path.
- Tally vs iperf3: **12 riperf3, 1 parity, 3 iperf3.**

### UDP loss (%) at `-b 0`, P8

| direction | riperf3 (mean / max) | iperf3 (mean / max) |
|---|--:|--:|
| forward (server receives) | 3.1 / 7.6 | 1.0 / 3.2 |
| reverse (server sends) | 1.7 / 2.7 | 0.7 / 1.0 |

UDP loss at `-b 0` is receiver-side socket-buffer overflow on a saturated link —
kernel `RcvbufErrors` on the receiving host, while sender `SndbufErrors` stay 0
(the sender never drops). riperf3 loses more than iperf3 in both directions
because it pushes ~10–20% more throughput, so it overruns the receiver's buffer
harder — higher goodput, higher loss, a characteristic rather than a regression
(a same-environment 0.6.3-vs-0.7.0 control measured the same loss in both
versions). Both tools' absolute loss moves between campaigns with the
environment, as the throughput absolutes do.

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
data path is unchanged, so they remain representative.

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
are measured on a two-VM KVM sandbox (two guests over a virtio-net bridge, with
riperf3 and iperf3 built from source on each). The orchestration is
sandbox-specific, but the method is portable and the results stay auditable — it
replicates on any two hosts:

- N=30 randomized iterations per cell, 2 warm-ups discarded;
- a fresh `-s -1` server per run on a unique port, with hard `timeout` wrappers;
- UDP at `-b 0` (unlimited, so neither tool is rate-capped);
- a direction-aware parse (forward → client `sum_sent`, reverse → `sum_received`,
  UDP → `sum`; `-P>1` aggregates already summed in `-J`);
- per-cell 95% CIs and a Welch's-t verdict.

To decide whether an off-looking cell is environment drift or a real regression,
build both versions from source on the same hosts and run them ABBA-interleaved
in that cell (Welch verdict): a same-environment A/B cancels the drift that a
comparison against a stored baseline — measured in a past environment — cannot.
