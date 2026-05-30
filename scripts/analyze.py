#!/usr/bin/env python3
"""Reduce a bench.sh campaign CSV to the BENCHMARKS.md throughput tables.

Per config cell ({TCP,UDP} x {fwd,rev} x {P1,P8} x {v4,v6}) it computes, for each
tool, the mean throughput, 95% CI, and coefficient of variation, then a head-to-
head riperf3-vs-iperf3 verdict via Welch's t (two-sided). Per the campaign design
(n=30) the p-value uses a normal approximation, so this depends only on the
stdlib -- no scipy/numpy. Also emits the UDP-loss summary at -b 0.

Usage: analyze.py <campaign.csv>
"""
import sys
import csv
import math
from collections import defaultdict

Z95 = 1.959963984540054  # standard-normal two-sided 95%


def normal_sf(x):
    """Upper-tail standard-normal survival function, P(Z > x), via erf."""
    return 0.5 * math.erfc(x / math.sqrt(2.0))


def stats(xs):
    n = len(xs)
    m = sum(xs) / n
    var = sum((x - m) ** 2 for x in xs) / (n - 1) if n > 1 else 0.0
    sd = math.sqrt(var)
    ci = Z95 * sd / math.sqrt(n) if n > 0 else 0.0
    cv = (sd / m * 100.0) if m else 0.0
    return {"n": n, "mean": m, "sd": sd, "ci": ci, "cv": cv}


def welch_p(a, b):
    """Two-sided p-value for H0: mean(a)==mean(b), Welch t + normal approx."""
    na, nb = len(a), len(b)
    if na < 2 or nb < 2:
        return float("nan")
    ma, mb = sum(a) / na, sum(b) / nb
    va = sum((x - ma) ** 2 for x in a) / (na - 1)
    vb = sum((x - mb) ** 2 for x in b) / (nb - 1)
    se = math.sqrt(va / na + vb / nb)
    if se == 0:
        return 1.0 if ma == mb else 0.0
    t = (ma - mb) / se
    return 2.0 * normal_sf(abs(t))


def fmt_p(p):
    if p != p:  # NaN
        return "n/a"
    if p < 1e-4:
        return "<1e-4"
    return f"{p:.2f}"


# Stable cell ordering matching BENCHMARKS.md: proto, dir, parallel, family.
PROTO_ORD = {"TCP": 0, "UDP": 1}
DIR_ORD = {"forward": 0, "reverse": 1}
PAR_ORD = {"P1": 0, "P8": 1}
FAM_ORD = {"v4": 0, "v6": 1}
DIR_LBL = {"forward": "fwd", "reverse": "rev"}


def main():
    if len(sys.argv) != 2:
        sys.exit("usage: analyze.py <campaign.csv>")
    # data[(proto,dir,par,fam)][tool] = [gbps,...] ; loss[...] = [lost_percent,...]
    data = defaultdict(lambda: defaultdict(list))
    loss = defaultdict(lambda: defaultdict(list))
    with open(sys.argv[1], newline="") as f:
        for row in csv.DictReader(f):
            key = (row["proto"], row["dir"], row["parallel"], row["family"])
            tool = row["tool"]
            data[key][tool].append(float(row["gbps"]))
            if row["proto"] == "UDP":
                loss[key][tool].append(float(row["lost_percent"]))

    keys = sorted(
        data,
        key=lambda k: (PROTO_ORD[k[0]], DIR_ORD[k[1]], PAR_ORD[k[2]], FAM_ORD[k[3]]),
    )

    print("### Throughput: riperf3 vs iperf3 (mean Gbps [95% CI])\n")
    print("| cell | riperf3 | iperf3 | Δ | p | verdict |")
    print("|---|--:|--:|--:|--:|---|")
    cv_lo, cv_hi = 100.0, 0.0
    for k in keys:
        proto, d, par, fam = k
        r = data[k].get("riperf3", [])
        i = data[k].get("iperf3", [])
        if not r or not i:
            continue
        rs, is_ = stats(r), stats(i)
        cv_lo = min(cv_lo, rs["cv"], is_["cv"])
        cv_hi = max(cv_hi, rs["cv"], is_["cv"])
        delta = (rs["mean"] - is_["mean"]) / is_["mean"] * 100.0
        p = welch_p(r, i)
        if p == p and p < 0.05:
            verdict = "**riperf3**" if rs["mean"] > is_["mean"] else "**iperf3**"
        else:
            verdict = "parity"
        label = f"{proto} {DIR_LBL[d]} {par} {fam}"
        rcell = f"{rs['mean']:.1f} [{rs['mean']-rs['ci']:.1f}–{rs['mean']+rs['ci']:.1f}]"
        icell = f"{is_['mean']:.1f} [{is_['mean']-is_['ci']:.1f}–{is_['mean']+is_['ci']:.1f}]"
        print(f"| {label} | {rcell} | {icell} | {delta:+.1f}% | {fmt_p(p)} | {verdict} |")

    if cv_lo <= cv_hi:  # at least one cell contributed (else the seed values invert)
        print(f"\n_Per-cell CV range: {cv_lo:.1f}–{cv_hi:.1f}%._")

    # UDP loss at -b 0, P8, by direction (aggregate over families).
    udp_p8 = defaultdict(lambda: defaultdict(list))
    for k in keys:
        proto, d, par, fam = k
        if proto == "UDP" and par == "P8":
            for tool, xs in loss[k].items():
                udp_p8[d][tool].extend(xs)
    if udp_p8:
        print("\n### UDP loss (%) at `-b 0`, P8\n")
        print("| direction | riperf3 | iperf3 |")
        print("|---|--:|--:|")
        for d in ("forward", "reverse"):
            if d not in udp_p8:
                continue
            r = udp_p8[d].get("riperf3", [0])
            i = udp_p8[d].get("iperf3", [0])
            who = "server receives" if d == "forward" else "server sends"
            print(
                f"| {d} ({who}) | {min(r):.1f}–{max(r):.1f} | {min(i):.1f}–{max(i):.1f} |"
            )

    # Compact significance summary to sanity-check the verdict column.
    wins = {"riperf3": 0, "iperf3": 0, "parity": 0}
    for k in keys:
        r, i = data[k].get("riperf3", []), data[k].get("iperf3", [])
        if not r or not i:
            continue
        p = welch_p(r, i)
        if p == p and p < 0.05:
            wins["riperf3" if stats(r)["mean"] > stats(i)["mean"] else "iperf3"] += 1
        else:
            wins["parity"] += 1
    print(
        f"\n_Verdicts: riperf3 {wins['riperf3']}, iperf3 {wins['iperf3']}, "
        f"parity {wins['parity']} (of {sum(wins.values())} cells)._"
    )


if __name__ == "__main__":
    main()
