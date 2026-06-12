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

> **0.7.4 status.** Fully re-measured at the `0.7.4` patch: the compatibility
> matrix is all-PASS (**52/52**, run as eight per-merge smokes plus a closing
> run on the release commit — every one clean) and a fresh full N=30 campaign
> lands **12 riperf3 / 4 parity / 0 slower** — riperf3 is significantly faster
> in every UDP cell (+8.7% to +13.9%) and every TCP `-P8` cell (+4.1% to
> +10.2%), with TCP single-stream one parity noise band, as in every campaign
> since 0.7.0. Cross-campaign absolutes moved DOWN ~3% vs the stored 0.7.3
> baseline — for **both tools in lockstep** (iperf3, an unchanged binary, reads
> 0 faster / 9 parity / 7 slower against its own 0.7.3-campaign numbers), the
> environment-shift signature, third campaign running. The one riperf3 cell
> exceeding iperf3's shift (UDP rev P1 v6, −7.4% cross-campaign) was settled by
> a controlled same-environment **v0.7.3-vs-v0.7.4 A/B** (30v30
> ABBA-interleaved in that exact cell): **parity** (−1.51%, p=0.34). New
> honesty note this edition: the campaign recorded **108/960 failed runs**
> (prior campaigns: 0) — uniformly distributed across tools (57 iperf3 / 51
> riperf3), cells, and time, the harness's fixed 0.4 s server-start sleep
> racing under host load (connect-refused class, direction-unbiased; a
> retry-on-refused hardening is queued). Per-cell n is 22–30; verdicts are
> unaffected. 0.7.4's throughput-relevant changes are the terminate-path and
> reporter end-race redesigns
> ([#159](https://github.com/therealevanhenry/riperf3/issues/159),
> [#230](https://github.com/therealevanhenry/riperf3/issues/230)) — control
> plane, not the data path — and the campaign confirms the data path held.
>
> **0.7.3 status.** Fully re-measured at the `0.7.3` patch: the compatibility
> matrix is all-PASS (**52/52** — four paced-UDP cells, `-b 100M` forward and
> reverse cross-pairs, joined the matrix with
> [#206](https://github.com/therealevanhenry/riperf3/pull/206)) and a fresh full
> N=30 campaign lands **11 riperf3 / 5 parity / 0 slower**. New this edition: a
> per-cell cross-campaign Welch comparison against the stored 0.7.2 baseline —
> **zero regressions; 9 cells significantly faster (+1.3% to +6.5%), 7 parity**,
> so 0.7.3 is statistically faster-or-equal to 0.7.2 in every cell. The
> 0.7.2-era trailing cell (TCP fwd P1 v4,
> [#174](https://github.com/therealevanhenry/riperf3/issues/174)) read **parity
> this run** (−0.4%, p=0.80) — the single-stream residual wandered again,
> consistent with the noise-band hypothesis. One in-campaign control: a
> single-run drift smoke flagged tcp-rev-P8-v6 at −18.1%, and the
> same-environment N=20 ABBA 0.7.2-vs-main A/B in that exact cell measured
> **parity (+1.25%, p=0.42)** — the N=1 wander pattern, third campaign running.
> 0.7.3's throughput-relevant changes are the grow-only UDP `SO_SNDBUF`
> ([#163](https://github.com/therealevanhenry/riperf3/issues/163), never shrinks
> the buffer) and the `-b rate/burst` multisend batch
> ([#160](https://github.com/therealevanhenry/riperf3/issues/160), engages only
> with an explicit burst); the unlimited campaign cells confirm the default data
> path moved, if anywhere, upward.
>
> **0.7.2 status.** Fully re-measured at the `0.7.2` patch (tables below): the
> compatibility matrix is all-PASS (48/48, incl. the new `-O`/`--get-server-output`
> paths cross-verified against real iperf3 in both roles) and a fresh full N=30
> campaign lands **12 riperf3 / 3 parity / 1 trailing** — the trailing cell is TCP
> fwd P1 v4 at −3.1% (p=0.019), tracked in
> [#174](https://github.com/therealevanhenry/riperf3/issues/174). Two controls keep
> that cell honest: the prior campaign's noise cell (TCP **rev** P1 v4, −1.8%) read
> +0.2% parity this run — the single-stream residual wanders — and a **controlled
> same-environment 0.7.1-vs-0.7.2 A/B** (both binaries built from source on the
> VMs, 30v30 ABBA-interleaved in the trailing cell) measured −1.0% at p=0.375:
> parity. 0.7.2's throughput-relevant change is the `-b` limiter rewrite (#116,
> cumulative-average throttle) — it only engages with `-b > 0`, and the unlimited
> campaign cells plus cross-tool byte-parity checks at 1M–10G confirm the default
> data path is unchanged. Absolute Gbps shifted a few percent vs the 0.7.1-era
> tables (environment, as with every campaign — the A/B is the proof).
>
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
| Date | 2026-06-11 |
| Host | Intel i9-13900K, Linux 7.0.11-arch1-1 (Arch), KVM |
| Guests | 2× Debian 13 (Trixie), Linux 6.12.90+deb13.1-cloud-amd64, 8 vCPU, 8 GB RAM each |
| NIC | virtio-net (vhost=on), bridged, MTU 9000; IPv4 `172.20.0.0/24` + IPv6 `fd00:20::/64` |
| riperf3 | 0.7.3 |
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
| TCP forward | 68.9 | 73.7 | 73.6 | 66.1 |
| TCP reverse | 53.2 | 74.9 | 62.9 | 74.9 |
| TCP bidir | 40.1 | 30.1 | 37.8 | 44.9 |
| UDP forward `-b 0` | 31.8 | 38.3 | 30.0 | 27.5 |
| UDP reverse `-b 0` | 30.8 | 25.4 | 35.0 | 27.0 |
| UDP bidir `-b 0` | 20.9 | 17.8 | 26.3 | 24.9 |

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
cell (`-t 5` s each), **960 runs attempted**, run in **randomized order** across
all (cell, tool, iteration) tuples so host/thermal drift can't systematically
favor either tool. 2 warm-ups per cell discarded; fresh `-s -1` server per run
on a unique port; hard `timeout` wrappers; VMs confirmed idle and isolated for
the duration. **This campaign recorded 108 failed runs (11%; prior campaigns
0)** — uniformly distributed across tools (57 iperf3 / 51 riperf3), protocols,
directions, and time, i.e. the harness's fixed 0.4 s server-start sleep racing
under host load (connect-refused class), not a tool signal; the drops are
throughput-independent so the comparison is unbiased. Retained n = 22–30 per
cell; per-cell coefficient of variation was 3.6–10.1%. Significance is Welch's
t (two-sided, normal approx); "parity" = not significant at p<0.05.

### Throughput: riperf3 vs iperf3 (mean Gbps [95% CI])

| cell | riperf3 | iperf3 | Δ | p | verdict |
|---|--:|--:|--:|--:|---|
| TCP fwd P1 v4 | 68.1 [66.3–70.0] | 69.8 [67.9–71.7] | −2.4% | 0.21 | parity |
| TCP fwd P1 v6 | 68.0 [66.3–69.6] | 70.0 [68.3–71.8] | −3.0% | 0.091 | parity |
| TCP fwd P8 v4 | 58.5 [57.1–59.9] | 55.5 [54.7–56.4] | +5.3% | 0.0005 | **riperf3** |
| TCP fwd P8 v6 | 60.1 [59.0–61.1] | 54.5 [53.4–55.6] | +10.2% | <1e-4 | **riperf3** |
| TCP rev P1 v4 | 68.2 [66.0–70.4] | 70.0 [68.0–72.1] | −2.6% | 0.23 | parity |
| TCP rev P1 v6 | 68.9 [66.6–71.1] | 69.5 [67.3–71.7] | −0.9% | 0.69 | parity |
| TCP rev P8 v4 | 58.2 [56.8–59.6] | 55.9 [54.4–57.4] | +4.1% | 0.028 | **riperf3** |
| TCP rev P8 v6 | 59.6 [58.0–61.3] | 55.4 [53.7–57.1] | +7.6% | 0.0005 | **riperf3** |
| UDP fwd P1 v4 | 31.5 [30.4–32.6] | 28.7 [27.9–29.5] | +9.8% | 0.0001 | **riperf3** |
| UDP fwd P1 v6 | 32.8 [31.7–33.9] | 29.7 [28.6–30.8] | +10.7% | 0.0001 | **riperf3** |
| UDP fwd P8 v4 | 31.4 [30.6–32.3] | 27.6 [27.0–28.2] | +13.9% | <1e-4 | **riperf3** |
| UDP fwd P8 v6 | 31.1 [30.6–31.6] | 27.7 [27.2–28.1] | +12.5% | <1e-4 | **riperf3** |
| UDP rev P1 v4 | 30.4 [29.5–31.4] | 27.2 [26.3–28.0] | +12.1% | <1e-4 | **riperf3** |
| UDP rev P1 v6 | 30.5 [29.3–31.6] | 28.0 [27.1–28.9] | +8.7% | 0.0011 | **riperf3** |
| UDP rev P8 v4 | 28.6 [28.2–29.1] | 25.3 [25.0–25.7] | +12.8% | <1e-4 | **riperf3** |
| UDP rev P8 v6 | 29.2 [28.6–29.7] | 25.6 [25.1–26.1] | +13.8% | <1e-4 | **riperf3** |

### Cross-campaign: 0.7.4 vs the 0.7.3 baseline (Welch per cell, BOTH tools)

Absolutes moved DOWN this campaign — for both tools. iperf3 is an unchanged
binary between the two campaigns, so its shift measures the environment:

| cells | faster | parity | slower | shift range |
|---|--:|--:|--:|---|
| riperf3 0.7.4 vs 0.7.3 baseline | 0 | 6 | 10 | −2.0% to −7.4% |
| iperf3 vs its own 0.7.3-campaign numbers | 0 | 9 | 7 | −0.7% to −5.2% |

The lockstep movement is the environment-shift signature (third campaign
running; the stored-baseline lesson from 0.7.2). The one riperf3 cell whose
shift exceeded iperf3's by more than noise — UDP rev P1 v6, −7.4% vs iperf3's
−3.7% — was settled by a controlled same-environment **v0.7.3-vs-v0.7.4 A/B**
(both tags built from source on the VMs, 30v30 ABBA-interleaved in that exact
cell): **parity** — v0.7.3 33.91 Gbps vs v0.7.4 33.40 Gbps, −1.51%, p=0.34.
(Both A/B means sit well above either campaign's reading of that cell — more
environment evidence.) The same-run head-to-head table above is the controlled
comparison; the cross-campaign absolutes are not.

**Findings.**
- **TCP single-stream is one noise band** (~68–70 Gbps, all four cells parity
  this run) — the single-stream residual keeps wandering between cells without
  sticking, four campaigns running
  ([#174](https://github.com/therealevanhenry/riperf3/issues/174) history).
- **TCP multi-stream: riperf3 significantly faster in all four P8 cells**
  (+4.1% to +10.2%).
- **UDP: riperf3 significantly faster in every cell** (+8.7% to +13.9%,
  p≤0.0011).
- **No regression vs 0.7.3**: the cross-campaign dips are environmental
  (lockstep with unchanged iperf3), and the worst residual cell measured
  parity in a same-environment A/B.
- Tally vs iperf3: **12 riperf3, 4 parity, 0 iperf3.**

### UDP loss (%) at `-b 0`, P8

| direction | riperf3 (mean / max) | iperf3 (mean / max) |
|---|--:|--:|
| forward (server receives) | 2.5 / 4.2 | 1.0 / 3.2 |
| reverse (server sends) | 1.0 / 1.6 | 0.2 / 0.3 |

UDP loss at `-b 0` is receiver-side socket-buffer overflow on a saturated link —
kernel `RcvbufErrors` on the receiving host, while sender `SndbufErrors` stay 0
(the sender never drops). riperf3 loses more than iperf3 in both directions
because it pushes ~9–14% more throughput, so it overruns the receiver's buffer
harder — higher goodput, higher loss, a characteristic rather than a regression
(the 0.6.3-vs-0.7.0 control measured the same loss in both versions). Note both
tools' absolute loss moved vs the 0.7.1-era tables (iperf3's forward mean tripled
too) — environment shift between campaigns, same as the throughput absolutes.

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
checks (e.g. 0.6.3-vs-0.7.0, 0.7.1-vs-0.7.2) reuse the campaign by pointing the
two binaries at different riperf3 builds in the same run — ABBA-interleaved
blocks in the cell under question, Welch verdict — so environment drift cancels
out. This same-environment A/B is the standing method for deciding whether an
off-looking cell is drift or a regression: campaign deltas compare against a
*stored* baseline measured in a *past* environment, and the environment moves
several percent between campaigns.
