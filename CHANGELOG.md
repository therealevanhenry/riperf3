# Changelog

All notable changes to riperf3 are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
While the crate is pre-1.0 (`0.x`), a minor-version bump may carry a breaking
change, per the SemVer 0.x convention; breaking changes are called out below.

This changelog begins at 0.6.0. For earlier releases (0.1.1–0.5.4), see the
[git history](https://github.com/therealevanhenry/riperf3/commits/main) and
release tags.

## [0.6.1] - 2026-05-30

A cross-platform compatibility and CI release. There are **no data-path or
wire-protocol changes** — the Linux fast path is byte-for-byte unchanged, so
throughput is identical to 0.6.0 (the iperf3 compatibility matrix was re-verified;
the performance campaign carries forward).

### Added
- **Native FreeBSD CI** (#39): a `vmactions/freebsd-vm` job builds and runs the
  full test suite on real FreeBSD, so FreeBSD-specific code (the `sendfile` arity,
  the FreeBSD `tcp_info` fields, the `sendmmsg` sender, `TCP_CONGESTION`) is
  observed rather than assumed. Promoted to a required check.
- **macOS `--bind-dev` verified** (#72): `--bind-dev` was already implemented on
  macOS via `IP_BOUND_IF`; its tests now select the loopback name per-OS and run
  on macOS, so the path is exercised in native CI.

### Fixed
- **Windows: `--cport`/`-B`/`--mptcp` connect failed with `WSAEWOULDBLOCK`** (#79).
  The non-blocking socket2 connect path treated Windows' in-progress
  `WSAEWOULDBLOCK` as fatal — it only accepted Unix's `EINPROGRESS`. It now also
  accepts `WouldBlock`, awaits writability, and surfaces a genuine failure via
  `take_error()`, matching the Unix path.
- **Compilation on other-Unix** (#78): the library failed to compile on Unix
  targets that aren't Linux/macOS/FreeBSD (NetBSD/OpenBSD/illumos). Two `cfg`
  gates — the zerocopy sender dispatch and `set_dont_fragment`'s no-op fallback —
  promised more than their implementations covered. `-Z` falls back to the normal
  sender and `--dont-fragment` becomes a no-op on those targets.

### Known issues

Tracked for a follow-up; the notable user-facing ones:

- **Windows UDP `-P`/`--bidir` multi-stream hangs** during stream setup
  ([#80](https://github.com/therealevanhenry/riperf3/issues/80)). A native-winsock
  limitation: a connected UDP data socket and the recycled wildcard listener share
  one port (`SO_REUSEADDR`), and winsock doesn't route a new stream's handshake to
  the listener the way Linux/BSD do (iperf3 sidesteps this by running under
  Cygwin). Single-stream UDP is unaffected. The Windows native CI check stays
  informational until this is fixed.
- **TCP `--bind-dev` is applied after `connect()`**
  ([#88](https://github.com/therealevanhenry/riperf3/issues/88)), so it may not
  constrain routing for TCP (the UDP path binds before connect). macOS
  `--bind-dev` over IPv6 (`IPV6_BOUND_IF`) is also not yet implemented.

## [0.6.0] - 2026-05-30

End-to-end iperf3 `-J` JSON faithfulness (client **and** server), a locked-down
public API surface, and native cross-platform CI. The headline is JSON: a
riperf3 `-J` document is now field-for-field comparable to iperf3's on both
sides of the connection.

### Added
- **Server-side `-J` and `--json-stream` output** (#50). The server now emits the
  same structured JSON document as the client, with server-perspective byte
  attribution and address reporting (`accepted_connection`), so `-J` is usable
  regardless of which side you run.
- **Faithful client `-J` document** (#36): a typed, iperf3-compatible `end` block
  with real local/remote addresses; `intervals` with per-stream `TCP_INFO`
  extremes; and `start` metadata (timestamp, cookie, `system_info`, MSS,
  socket buffer sizes).
- **`--extra-data` surfaced in `-J` output** (#35), matching iperf3's top-level
  `extra_data` field.
- **Native macOS and Windows CI runners plus a musl build check**, so
  cross-platform behavior is observed rather than assumed. (FreeBSD CI is still
  outstanding — see Known issues, #39.)
- **`cargo-semver-checks` gate** in CI (a required check) to catch unintended
  public-API breaks before release.

### Changed
- **API (breaking).** The `-J` output model types and `Server` are now
  `#[non_exhaustive]`, so future field additions are non-breaking. This is the
  breaking change that motivates the 0.6.0 minor bump: downstream code that
  constructed these structs with literal initializers must adapt.
- **CLI (breaking).** `-b`/`--bitrate` and `--fq-rate` rate suffixes are now
  parsed as **decimal** (1000-based: `10M` = 10,000,000 bits/s), matching
  iperf3's `unit_atof_rate`. Size suffixes (`-l`, `-w`, `-n`, `-k`) remain
  **binary** (1024-based), matching iperf3's `unit_atof`. Previously rate
  suffixes were parsed as binary (#56).
- **CLI.** Fractional suffixes are now accepted (`1.5M`, `0.5K`), matching
  iperf3's `sscanf`-based parse (#73).

### Fixed
- UDP receiver is drained at teardown so reverse/bidirectional tests no longer
  reset an iperf3 server at version ≤ 3.12 (#48).

### Internal
- Real-iperf3 wire-interop CI gate against current and 3.12 iperf3 (#38), now
  also validating the server's `-J` output (#50).
- Linux/Unix-only feature tests are gated by target so the native macOS/Windows
  CI runners pass cleanly (#71, #76). (The macOS `--bind-dev` test is gated
  pending its underlying fix — see Known issues, #72.)

### Known issues

Tracked for a follow-up cleanup after 0.6.0. See the
[issue tracker](https://github.com/therealevanhenry/riperf3/issues) for the
complete list; the notable user-facing ones:

- **`-s -D` daemon mode is broken** — a daemonized server listens but never
  serves, so any client (riperf3 or iperf3) hangs after connecting. Use a
  foreground `-s` server (or a process manager) until this is fixed.
  ([#81](https://github.com/therealevanhenry/riperf3/issues/81))
- **Options accepted but not yet effective** (silent no-ops):
  `--get-server-output` ([#33](https://github.com/therealevanhenry/riperf3/issues/33)),
  `--pacing-timer` ([#32](https://github.com/therealevanhenry/riperf3/issues/32)),
  `-O`/`--omit` (warm-up isn't excluded from the summary)
  ([#31](https://github.com/therealevanhenry/riperf3/issues/31)),
  `-T`/`--title` (doesn't prefix output lines)
  ([#34](https://github.com/therealevanhenry/riperf3/issues/34)), and UDP
  `-w`/`--window` ([#59](https://github.com/therealevanhenry/riperf3/issues/59)).
  The server also accepts client-only flags rather than rejecting them as iperf3
  does ([#65](https://github.com/therealevanhenry/riperf3/issues/65)).
- **Reverse + byte/block limit** (`-R -n` / `-R -k`) never terminates
  ([#60](https://github.com/therealevanhenry/riperf3/issues/60)).
- **`-J` fidelity gaps:** `--json-stream` is not valid line-delimited JSON
  ([#62](https://github.com/therealevanhenry/riperf3/issues/62)); integral
  floats render as `N.0` where cJSON omits the decimal
  ([#57](https://github.com/therealevanhenry/riperf3/issues/57)); the final
  interval is occasionally dropped
  ([#55](https://github.com/therealevanhenry/riperf3/issues/55)); bidir interval
  `sum` lumps both directions
  ([#54](https://github.com/therealevanhenry/riperf3/issues/54));
  `congestion_used` is never populated
  ([#37](https://github.com/therealevanhenry/riperf3/issues/37)).
- **Platform-specific:** on Windows, `--cport` fails with `WSAEWOULDBLOCK`
  ([#79](https://github.com/therealevanhenry/riperf3/issues/79)) and UDP
  `--bidir -P 4` hangs ([#80](https://github.com/therealevanhenry/riperf3/issues/80));
  on macOS, retransmit counts always read 0
  ([#40](https://github.com/therealevanhenry/riperf3/issues/40)) and `--bind-dev`
  is Linux-only ([#72](https://github.com/therealevanhenry/riperf3/issues/72)).
  The library does not yet compile on NetBSD/OpenBSD/illumos
  ([#78](https://github.com/therealevanhenry/riperf3/issues/78)), and FreeBSD has
  no CI coverage ([#39](https://github.com/therealevanhenry/riperf3/issues/39)).

[0.6.1]: https://github.com/therealevanhenry/riperf3/releases/tag/v0.6.1
[0.6.0]: https://github.com/therealevanhenry/riperf3/releases/tag/v0.6.0
