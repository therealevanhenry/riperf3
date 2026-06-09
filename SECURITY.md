# Security Policy

## Reporting a vulnerability

Please use [GitHub private vulnerability reporting](https://github.com/therealevanhenry/riperf3/security/advisories/new) — do **not** open a public issue for security problems.

If you cannot use GitHub's reporting flow, email evan.henry@gmail.com with "riperf3 security" in the subject.

You can expect an acknowledgement within a few days. Fixes ship as a patch release; credit is given unless you ask otherwise.

## Scope notes

riperf3 parses untrusted network input (the iperf3 control protocol, length-prefixed JSON) and is commonly run as a long-lived server. Parsing robustness, resource exhaustion on the control channel, and the RSA authentication path are all in scope. The `unsafe` inventory is documented in `riperf3/src/lib.rs`.
