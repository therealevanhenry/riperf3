# Changelog

All notable changes to riperf3 are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
While the crate is pre-1.0 (`0.x`), a minor-version bump may carry a breaking
change, per the SemVer 0.x convention; breaking changes are called out below.

This changelog begins at 0.6.0. For earlier releases (0.1.1–0.5.4), see the
[git history](https://github.com/therealevanhenry/riperf3/commits/main) and
release tags.

## [0.7.1] - 2026-06-08

The first non-breaking `0.7.x` patch — a batch of iperf3 **output-faithfulness**
fixes (flags and report fields that diverged from iperf3) plus internal cleanup
from the 0.7.0 API refactor. No public API change. The default data path is
unchanged: the cross-tool compatibility matrix is all-PASS and a fresh full N=30
throughput campaign vs iperf3 is regression-free (parity-or-faster) — see
[BENCHMARKS.md](BENCHMARKS.md).

### Added
- **`congestion_used` now reports the congestion-control algorithm actually in
  effect** (#37). Read back via `getsockopt(TCP_CONGESTION)` at stream creation
  (the kernel default when `-C` is unset) and surfaced in the `end` block's
  `sender_tcp_congestion`/`receiver_tcp_congestion`, matching iperf3; previously
  it was always absent. TCP-only (Linux/FreeBSD).

### Changed
- **A `-w/--window` the kernel cannot satisfy now aborts the test** (#97),
  matching iperf3's `IESETBUF2` ("socket buffer size not set correctly"). When the
  realized `SO_SNDBUF`/`SO_RCVBUF` is smaller than requested (the kernel clamped to
  `wmem_max`/`rmem_max`), riperf3 errors instead of silently running with a smaller
  buffer. **Behavior change**: a previously-tolerated oversized `-w` now fails the
  run, surfacing the misconfiguration rather than hiding it.
- **The client rejects server-only options** (#100), like iperf3's `IESERVERONLY`.
  `-D/--daemon`, `-1/--one-off`, `--idle-timeout`, `--server-bitrate-limit`,
  `--server-max-duration`, `--rsa-private-key-path`, `--authorized-users-path`,
  `--time-skew-threshold`, and `--use-pkcs1-padding` are now rejected on a client,
  before any side effects. (`--use-pkcs1-padding` was previously accepted on the
  client; it is server-only in iperf3.)

### Fixed
- **`-i 0` now emits one whole-test interval** (#107) covering `0.00`–`<duration>`,
  matching iperf3's "one interval = the whole test"; riperf3 emitted none. Affects
  text, `-J`, and `--json-stream`.
- **`test_start.duration` is now `0` for byte/block-limited (`-n`/`-k`) runs**
  (#114), matching iperf3 — the `-t` window doesn't apply; the limit is reported in
  `bytes`/`blocks`. riperf3 reported the default `-t`.

### Internal
- Extracted the CLI arg→builder mapping into `Cli::build_client`/`build_server` so
  the wiring tests exercise the real `main.rs` mapping instead of a hand-maintained
  copy (#124) — closing the drift blind spot and covering every previously-untested
  flag (49 → 80 CLI wiring tests).
- Triaged dead/test-only code surfaced by the 0.7.0 module retraction (#125) and
  dropped redundant Windows-target `as i32` casts flagged by clippy (#129). No
  behavior change.

## [0.7.0] - 2026-06-08

A minor bump (the first `0.x` minor since 0.6.0). The headline is a deliberate
**library API narrowing** — encapsulating configuration behind the builders,
retracting internal modules from the public surface, and marking the remaining
public types `#[non_exhaustive]` — so that **0.7.0 is the last breaking release**
for the current set of known issues and every 0.7.x dot release can stay
non-breaking. It also carries a batch of rate/byte-limited (`-b`/`-n`/`-k`) and
platform fixes. The default (unlimited) Linux data path is unchanged and was
re-benchmarked regression-free against 0.6.3 — see
[BENCHMARKS.md](BENCHMARKS.md).

### Changed
- **BREAKING (library API): `riperf3::reporter::spawn_interval_reporter` gains a
  `ReporterEnd` argument** (#55). Callers pass an `Arc<ReporterEnd>` and signal
  the authoritative end-of-test time via `ReporterEnd::finish(end_secs)`. The new
  public `ReporterEnd` type is the only API addition required by the fix. The CLI
  binary is unaffected; only direct library consumers need to update.
- **BREAKING (library API): server daemonization removed from the library**
  (#81). `ServerBuilder::daemon()` and the `Server::daemon` field are gone, and
  `Server::run()` no longer forks. A library consumer that wants a daemon must
  `daemon()`/fork *before* constructing its async runtime — doing it from inside
  `run()` cannot work (see Fixed). The CLI is unaffected; `-s -D` behaves as
  before, only correctly.
- **BREAKING (library API): configuration is encapsulated behind the builders**
  (#43). Every `Client`/`Server` field was `pub`; they are now `pub(crate)`, so
  the only way to construct a configured client or server is through
  `ClientBuilder`/`ServerBuilder`, and `build()`-time validation can no longer be
  bypassed. The CLI is unaffected.
- **BREAKING (library API): the public surface was narrowed to the intended API**
  (#67). The implementation modules — `net`, `protocol`, `reporter`, `stream`,
  `utils`, `auth`, `cpu`, `tcp_info`, `units` — are now `pub(crate)` rather than
  `#[doc(hidden)] pub`. The public API is now exactly: `Client`/`ClientBuilder`,
  `Server`/`ServerBuilder`, `TransportProtocol`, the result model
  (`TestResultsJson`/`StreamResultJson` and the `json_report` module), the error
  types (`RiperfError`/`ConfigError`), and `set_cpu_affinity`. A consumer that
  reached into the old internal items must move to the builder API.
- **BREAKING (library API): public types are now `#[non_exhaustive]`** (the 0.7.0
  API freeze). `RiperfError`, `ConfigError`, `TransportProtocol`,
  `TestResultsJson`, and `StreamResultJson` are marked `#[non_exhaustive]`, so a
  downstream `match` needs a wildcard arm and the structs can't be built by
  struct literal — but future error variants, a future transport, and future
  result fields become *additive*. That is what lets 0.7.0 be the last breaking
  release for the current API. (The `-J` `json_report` model was already
  `#[non_exhaustive]`.)

### Removed
- **BREAKING (library API): `TestConfig` is no longer public.** It is
  server-internal (built from the received parameter JSON) and was never part of
  any public signature.
- **BREAKING (library API): inert builder setters removed** (#122).
  `ClientBuilder::{pidfile, logfile, affinity}` and `ServerBuilder::{pidfile,
  logfile}` are gone — they stored values the library never read; the CLI
  realizes them at the process level (it writes the pidfile, redirects stdout,
  and calls `set_cpu_affinity` itself). The CLI flags `-I`/`--pidfile`,
  `--logfile`, and `-A`/`--affinity` are unchanged.

### Fixed
- **`-s -D` daemon mode now actually serves** (#81): the server called
  `daemon()` *after* the multi-threaded tokio runtime was built. `daemon()`
  forks, and a fork of a multi-threaded process keeps only the calling thread in
  the child, so the daemon had no tokio worker threads — it accepted the control
  connection but never ran a test, and every client hung. The CLI now daemonizes
  *before* building the runtime, matching iperf3's `daemon(1, 0)` (keeps the
  working directory so relative `-I`/`--logfile` paths resolve as given;
  redirects std{in,out,err} to `/dev/null`) and writes the pidfile after the
  fork so it records the daemon's pid.
- **`--json-stream` now emits valid line-delimited JSON** (#62): it printed text
  banners (the `[ ID] Interval` header, the `- - -` separator, the final summary
  lines) interleaved with *bare* per-stream interval objects that had no
  `event`/`data` wrapping and no `start`/`end` events — so the output was neither
  parseable NDJSON nor iperf3's event schema. Both the client and the server now
  emit pure NDJSON: a `{"event":"start","data":{…}}` line, one
  `{"event":"interval","data":{"streams":[…],"sum":{…}}}` per interval (streamed
  live and flushed as it happens), then `{"event":"end","data":{…}}`, with no
  banners. The `start`/`interval`/`end` payloads reuse the typed `-J` model, so
  each is byte-for-byte the corresponding section of the batched `-J` report
  (including the cJSON float formatting, #57).
- **The final (partial) interval is no longer dropped** (#55): the interval
  reporter looped `tick -> if done break`, so a run that ended part-way through an
  interval lost its last interval (a 2s `-i 1` run intermittently printed 1
  interval instead of 2; `-J`/`--json-stream` lost the tail too). The test driver
  now hands the reporter the authoritative end time and the reporter flushes a
  final interval `[last_boundary, end]` before stopping, matching iperf3. A
  duration run passes its exact `-t`, so a boundary-aligned end produces no
  spurious trailing interval; the senders stop at the deadline, so the summary is
  unchanged. The final interval reuses the last-sampled `Cwnd`/`RTT` when the
  socket has already closed (reporting `Retr 0` for the sub-interval, as iperf3
  does) instead of leaving those columns blank.
- **UDP `-P > 1` no longer hangs setup on native Windows** (#80): the server's
  UDP design mirrors iperf3 — one connected data socket per stream, recycled on
  the same port via `SO_REUSEADDR`, with the kernel demultiplexing incoming
  datagrams by 4-tuple. Native winsock breaks this: once a connected and a
  wildcard UDP socket share a port, a new source's datagram is silently dropped,
  so streams 2..N never finish their connect handshake and any `-u -P >1` (hence
  UDP `--bidir -P`) hung until the client's 30 s connect timeout. (iperf3 itself
  only runs on Windows under Cygwin, whose socket layer emulates Unix demux, so
  it never hits this; a native-MSVC build does.) The server now has a second UDP
  path that binds **one** unconnected socket for the whole test and
  demultiplexes streams by client source address in userspace — correct on every
  platform, and wire-compatible with an iperf3 client (verified against the 3.20
  binary over IPv4 and IPv6, forward/reverse/bidir `-P 4`). It is the default on
  Windows; the connected-socket recycling path stays the default on Unix because
  it mirrors iperf3 and gives one socket and thread per stream (kernel-parallel
  receive that scales better at high `-P`). At modest parallelism the two paths
  measure comparably on throughput and loss.
- **Reverse byte/block-limited tests (`-R -n` / `-R -k`) now terminate** (#60):
  the client's end-of-test check summed only the sender streams' bytes, so in
  reverse — where the client only receives — the total never reached the limit
  and the transfer ran indefinitely at line rate. The check now trips when
  either the bytes sent or the bytes received reach the target, matching
  iperf3's `bytes_sent >= N || bytes_received >= N`; a bidir byte/block limit
  likewise ends at whichever direction reaches the target first.
- **TCP `-b`/`--bitrate` is now honored by the client** (#102): the client
  applied `-b` only to UDP, so a rate-limited TCP test ran unthrottled at line
  rate. The token-bucket rate limiter now paces the TCP sender too (per stream,
  iperf3 semantics); zerocopy is used only on the unlimited path.
- **Byte/block-limited summaries report measured elapsed, not the `-t` window**
  (#103): an `-n`/`-k` run computed its summary bitrate and `seconds` against the
  default `-t` duration instead of the time the transfer actually took. The
  report now separates the `-t` parameter (under `test_start`) from the measured
  elapsed time that drives the summary, on both the client and the `-s -J`
  server.
- **Byte/block-limited transfers no longer overshoot the limit** (#117): the
  sender free-ran between the client's 100 ms polls and pushed ~100 ms of extra
  line rate past `-n`/`-k` (e.g. `-n 512K` transferred ~925 MB). The sending
  streams now share an atomic byte budget and stop at ~N, matching iperf3's
  test-wide total (`-n 512K` is now exactly 0.52 MB).
- **iperf3's `num=0`/`blocks=0` is read as unlimited at server ingest** (#119): a
  plain `-t` run from an iperf3 client sends `num=0`, which the server decoded as
  a zero-byte limit (so an iperf3 reverse-duration client received nothing). The
  server now normalizes `0` to "no limit" on receipt; also fixes a latent UDP
  `max_duration` mishandling.
- **The server rejects client-only options, matching iperf3** (#65): running
  `-s` with a client-only flag (e.g. the `-A n,m` comma form) was silently
  accepted and ignored; iperf3's server rejects these with `IECLIENTONLY`.
  riperf3 now rejects exactly iperf3's client-only set for the flags it exposes
  (adjudicated against iperf3 3.20), at the top of `main()` before any side
  effects, with iperf3's canonical error text naming the offending flag. (The
  bare `-A n`, `--cport`, and `-m`/`--mptcp` stay accepted on a server, as iperf3
  accepts them.)

### Added
- **`StreamCounters::peek_sent_interval` / `peek_received_interval`** (#55):
  non-draining reads of the interval byte counters, used to skip an empty final
  partial interval on the receiver side.
- **`RIPERF3_UDP_SERVER_DEMUX` environment variable** (#80): overrides which
  server UDP path is used — `0`/`false`/`no`/empty force the connected-socket
  recycling path, any other value forces the single-socket userspace demux;
  unset falls back to the platform default (demux on Windows, recycling on Unix).
  Primarily a testing/escape hatch — it lets either path be exercised on one
  build.

## [0.6.3] - 2026-06-04

An iperf3-fidelity and correctness release. There are **no public Rust API
changes** (SemVer-clean) and **no data-path/throughput changes** — the Linux
fast path is unchanged. The `-J`/text output changes below move *toward* iperf3
parity and remain parseable; parity was verified against iperf3 3.12 and 3.21 in
the interop CI.

### Added
- **`-T`/`--title` now prefixes output** (#34): client text lines are prefixed
  with `<title>:  ` (colon + two spaces), matching iperf3's `iperf_printf`.
  Text mode only — `-J` and `--json-stream` are never titled.

### Fixed
- **`-J` integral floats drop the trailing `.0`** (#57): the JSON report now
  renders floats the way iperf3's bundled cJSON does — integral doubles as
  integer tokens (`0`, `1`, `10485760`), fractionals via `%.15g`/`%.17g` — so the
  blob is byte-compatible with iperf3 for consumers that diff raw text. Still
  parses identically.
- **`-w`/`--window` is now applied to UDP sockets** (#59): `SO_SNDBUF`/`SO_RCVBUF`
  are set on UDP data sockets (client and server), matching iperf3's
  `iperf_udp_buffercheck`. Previously `-u -w <N>` left the kernel defaults in
  place while still reporting the requested size.
- **macOS no longer reports a misleading `Retr 0`** (#40): macOS exposes no
  sender retransmit *packet* count via the Rust `libc` binding, so riperf3 now
  reports no retransmit info there instead of a perpetual zero. The coupled
  `Cwnd` column is suppressed with it; the faithful fix is tracked in #96.
- **`-Z` zerocopy temp files get unique names** (#42): each zerocopy sender now
  uses a `<pid>-<seq>` temp file, removing a create/truncate race under
  `-Z -P>1`.

### Documentation
- **Unsafe-audit table completed** (#44): added the `getsockopt(TCP_MAXSEG)` row;
  the table now maps 1:1 to every production `unsafe` block.

## [0.6.2] - 2026-06-04

A packaging fix release. There are **no data-path, wire-protocol, or public API
changes** — behavior and throughput are identical to 0.6.1.

### Fixed
- **`riperf3` failed to compile as a dependency of downstream crates** (#92): the
  library calls `socket2`'s `set_mss`, which socket2 gates behind its `all`
  feature, but the crate never enabled that feature itself. It built only because
  `tokio` transitively enabled `all` on socket2 0.5. Once a downstream resolved a
  newer `tokio` that moved to socket2 0.6, riperf3's own socket2 0.5 dependency
  lost `all` and the build failed with `error[E0599]: no method named set_mss`.
  riperf3 now enables socket2's `all` feature explicitly, so it compiles
  regardless of sibling feature unification (#93).

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
  outstanding — #39.)
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
  pending its underlying fix — #72.)

### Known issues

Tracked for a follow-up cleanup after 0.6.0. See the
[issue tracker](https://github.com/therealevanhenry/riperf3/issues) for the
complete list; the notable user-facing ones:

- **Options accepted but not yet effective** (silent no-ops):
  `--get-server-output` ([#33](https://github.com/therealevanhenry/riperf3/issues/33)),
  `--pacing-timer` ([#32](https://github.com/therealevanhenry/riperf3/issues/32)),
  and `-O`/`--omit` (warm-up isn't excluded from the summary)
  ([#31](https://github.com/therealevanhenry/riperf3/issues/31)).
- **`-J` fidelity gaps:** bidir interval `sum` lumps both directions
  ([#54](https://github.com/therealevanhenry/riperf3/issues/54)), and
  `congestion_used` is never populated
  ([#37](https://github.com/therealevanhenry/riperf3/issues/37)).

[0.6.1]: https://github.com/therealevanhenry/riperf3/releases/tag/v0.6.1
[0.6.0]: https://github.com/therealevanhenry/riperf3/releases/tag/v0.6.0
