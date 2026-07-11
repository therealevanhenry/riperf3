# Changelog

All notable changes to riperf3 are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
While the crate is pre-1.0 (`0.x`), a minor-version bump may carry a breaking
change, per the SemVer 0.x convention; breaking changes are called out below.

This changelog begins at 0.6.0. For earlier releases (0.1.1–0.5.4), see the
[git history](https://github.com/therealevanhenry/riperf3/commits/main) and
release tags.

## [Unreleased]

### Breaking

- `Client::run` returns `RunOutcome { report, termination }` instead of `Report` (#293).
  A server-terminated or relayed-error run is now `Ok` — carrying the partial report plus
  `Termination::ServerTerminated`/`ServerError(msg)` — instead of an `Err` that discarded it.
  `Err` now covers runs that produced no report (connect/handshake failures; the
  `ControlSocketClosed`/`RecvResultsFailed` classes stay `Err` for now — a follow-up).
  The `RiperfError::ServerTerminated` /
  `ServerErrorRelayed` variants are removed. Migration: `client.run().await?` →
  `client.run().await?.report`; branch on `outcome.termination` for the ending.
- `Server::run_once` / `BoundServer::run_once` likewise return `RunOutcome` instead of `Report`
  (#293): a report-producing abnormal end (client-terminate, control-close, a
  `--server-bitrate-limit`/`--server-max-duration` self-terminate) is now `Ok` with the partial
  report plus a server-side `Termination`, not an `Err` that discarded it. Migration:
  `server.run_once().await?` → `server.run_once().await?.report`.
- The builder output default flips to quiet (#294): a bare `run()`/`run_once()` returns the
  report and prints nothing. Opt into iperf3's full text/JSON output with `.emit_output(true)`
  (the CLI sets this). Wire protocol, CLI output, and exit codes are unchanged.
- `Start::sock_bufsize` is `Option<i64>` (was `Option<u64>`): iperf3 renders the requested
  `-w` verbatim, negatives included (#392).

### Added

- `riperf3::ErrorSinkGuard`: the CLI's `--logfile` hook for the library's error lines (#364).
- `Termination::errexit_message()`: the CLI's exit-code hook for the client's abnormal endings (#293).
- `riperf3::outcome` is a public module documenting the #293 run contract.

### Fixed

- Auth flag combinations are validated at parse time like iperf3 (IESETCLIENTAUTH /
  IESETSERVERAUTH / IESERVERAUTHUSERS sentences, key files loaded up front, client
  password resolved at the getpass slot — env else TTY-only prompt): a half-configured
  client no longer connects and a half-configured server no longer listens and serves
  UNAUTHENTICATED. RSA key PEMs now accept PKCS#1 alongside PKCS#8/SPKI, like OpenSSL.
  New lib API: `validate_public_key_file`, `validate_private_key_file`,
  `read_auth_password` (#395).
- Runtime auth denials now match iperf3's surface: bare control-socket close (no wire
  byte — 0xFF is the busy-server signal only), no "Accepted connection" block, and the
  unstamped `error - no error` line/doc string. A client receiving 0xFF reports iperf3's
  IEACCESSDENIED text; new lib variant `RiperfError::ServerBusy` (#395).
- `--cport` binds `port + i` per stream like iperf3 (`-P 2` no longer dies on a source-port
  collision; 65536 wraps to ephemeral), UDP honors `--cport` at all (was silently ephemeral),
  and a failed data-stream dial reports iperf3's `unable to connect stream: …` class (#428).
- A setup-phase kill (failed data accept, idle timeout, …) now parks the round in iperf3's
  sync-close drain until the peer consumes the error relay — a one-off server no longer
  closes its listener at the kill instant, which RST'd a real iperf3 client's queued data
  socket before it could read the SERVER ERROR frame (#390).
- `--server-bitrate-limit` breaches on iperf3's moving average — the last `rate/N` seconds
  (default 5) of one-second samples, evaluated only once the window fills — instead of a
  whole-test average at 1 Hz (breach at ~5 s like iperf3, not ~1 s; bursts age out; a quiet
  prefix no longer dilutes). The `/N` averaging-interval half now wires through (new
  `ServerBuilder::server_bitrate_limit_interval`), and a limit of 0 disables the check
  like iperf3 instead of killing every test (#410).
- The client's params blob carries `repeating_payload` / `dont_fragment` / `flowlabel` like
  iperf3, and the server honors the first two on its send paths: reverse/bidir TCP payload
  fills the repeating pattern — now iperf3's ASCII-digit fill; the previous 0x00..0xFF ramp
  the client sent was a wire divergence — and UDP v4 egress sets DF (iperf3's gate —
  UDP+IPv4 only; the TCP client no longer sets DF where iperf3 never did) (#414).
- `-w 0` is a no-op like iperf3 (0 = kernel autotuning): the window is never applied to
  data sockets — was clamping both buffers to kernel minimums, a live throughput hit — and
  the params blob omits the `"window"` key, so a `-w 0` client no longer shrinks a riperf3
  server's buffers either (#415).
- Rate-set server documents place `target_bitrate` at iperf3's get_parameters slot (right
  after `system_info`, shifting the TCP trio); the duplicate on_connect emission is a
  recorded deviation (#377).
- `--get-server-output` no longer alphabetizes the embedded server document
  (serde_json `preserve_order`); the params/results wire blobs now match iperf3's key
  order too (#378).
- `-w -1` renders `sock_bufsize: -1` like iperf3 in every document (was clamped to 0, or
  u64-wrapped in the setup doc); the setup-doc buffer actuals are computed once at param
  ingest like iperf3, so emit-time fd exhaustion can't blank them, and on the listener's
  address family (#392).
- The client's SERVER_ERROR relay renders iperf3's bare `end: {}` at any stage — a mid-run
  breach no longer emits the populated finalize end; on a `Termination::ServerError` ending
  `outcome.report.end` is bare in every mode (partial stats ride `intervals`) (#404).
- A peer RST after the completed results exchange takes iperf3's IERECVMESSAGE class over
  the populated document instead of a raw-io skeleton; `Termination::RecvMessageFailed` is
  the new server-side ending (#406).
- Control bytes (`0x01`-`0x1F`) in whitespace positions of a wire blob parse like cJSON's
  skip rule; NUL stays a terminator like iperf3's strlen-based parse entry (#402). A
  wrong-typed params field stays a strict IERECVPARAMS error where iperf3 warns and
  defaults the field — a recorded deviation (#401).
- A `run()`/`run_once()` future dropped mid-test (`tokio::time::timeout`, `select!`) no
  longer leaks parked stream tasks and their sockets: an abort guard fires on cancellation,
  disarmed by the normal teardown paths; a cancel mid-setup remains #381's scope (#380).
- Mid-setup errors and cancellations tear down earlier-spawned stream tasks on both roles:
  the server's setup phase now runs inside the teardown gate's block, the client's
  `create_streams` pushes partial progress into `ctx.streams` instead of a local vec, and
  each task is abort-guarded the moment it spawns (#381).
- Refused rounds (`--server-max-duration`/`--server-bitrate-limit`) park until the client
  closes or 10 s of control-socket silence, mirroring iperf3's bounded cleanup drain: the
  refusal doc renders at round end, and a signal landing in the park abandons it — the
  interrupt skeleton emits alone, carrying the refused round's `target_bitrate` (#386).
- `--logfile` receives the SERVER-ERROR relay receipt and the interrupt notice like iperf3's
  `iperf_err` routing; both previously went to stderr (#364).
- Wire-blob parsing mirrors cJSON's UTF-8 BOM skip and non-object params root; four residual
  strictness divergences are recorded deviations (#367).
- Rate-breach and duration-watchdog `-J` documents render a bare `end: {}` like iperf3 (#368).
- Exchange-phase send failures map to iperf3's IESENDMESSAGE/IESENDRESULTS classes over the
  populated document instead of a raw-io skeleton (#371).

## [0.8.0] - 2026-06-28

Architecture-and-API release. Wire protocol and CLI flags unchanged; success-path
`-J`/text output byte-identical. Breaking changes are library-API only.

### Breaking

- `Client::run` returns `riperf3::Report` (the rich `-J`-schema report) instead of
  `TestResultsJson` (#137). Migration: `result.streams.iter().map(|s| s.bytes).sum()`
  → `result.end.sum_sent.bytes` (fwd) / `.sum_received.bytes` (rev);
  `result.cpu_util_total` → `result.end.cpu_utilization_percent.host_total`.
- `TestResultsJson` / `StreamResultJson` no longer re-exported from the crate root (#137).
- Seven `Report` fields are now `Option` so a refusal document can omit what the test
  never produced (#261): `Start::{sock_bufsize, sndbuf_actual, rcvbuf_actual, test_start}`,
  `End::{sum_sent, sum_received, cpu_utilization_percent}`. Always `Some` once a run
  reaches TestStart, so success output is unchanged. Migration: `…sum_sent.as_ref().unwrap()`.

### Added

- `Server::run_once() -> Result<Report>`: serve one test and return its report (#137).
- `riperf3::json_report` is public; `Report` re-exported at the crate root (#137).

### Changed

- Removed the dead async UDP sender/receiver variants; documented the deliberate
  `spawn_blocking`/blocking-socket UDP design (#146).
- Extracted the shared client/server data-stream setup (`StreamMeta`/`DataStream::from_meta`
  + a socket-capture helper) so a new stream field is compiler-enforced across all call
  sites; no behavior change (#144).
- UDP sender datagram counts now come from an authoritative per-stream datagram counter
  (incremented per send batch) instead of `bytes/blksize` derivation, so a future
  short/partial send can't silently corrupt the count; wire/`-J`/text output is
  byte-identical (#256).
- Control-state transitions are validated against a legal-next table; an out-of-order byte
  logs a hardening diagnostic (debug/`-V`-gated) but is still tolerated exactly as iperf3
  does — default output unchanged (#145).
- Expanded CI: cross-compile checks for NetBSD, Intel macOS, and aarch64 Linux (gnu+musl),
  plus a rustdoc gate (#272).

### Fixed

- Client `-J` upfront-refusal document is now byte-faithful to iperf3 (#261): omits the
  unreached `start`/`end` fields, emits `end: {}`, real on-connect timestamp (was epoch-0).
- Final partial interval now reports the genuinely-final `TCP_INFO` sample (cwnd/rtt/snd_wnd),
  captured before the sender drops its socket, instead of the prior interval's stale values (#245).
- Client relay of `SERVER_ERROR` mirrors iperf3's per-code `perr` trailing `: ` (code 160 and
  the unknown-code fallback; codes 27/37/120 stay bare) (#248).
- `snd_wnd` is signed end-to-end: macOS emits interval `-1` / `max_snd_wnd: 0` like iperf3,
  Linux/FreeBSD the real value (#161).
- Deliberate deviation from iperf3 (#261): where iperf3 emits the `"error"` key **twice**
  on a relayed refusal (an upstream defect, [esnet/iperf#2051](https://github.com/esnet/iperf/issues/2051)),
  riperf3 emits a single clean `"error"` key — the bare message a conformant last-wins
  parser resolves to.

## [0.7.4] - 2026-06-12

A non-breaking faithfulness patch in two waves: 18 issues closed across 17
PRs, every fix pinned red-first against live iperf 3.21 ground truth (the
pinned d39cf41 build) and through at least two adversarial cold-review
rounds; every merge gated on the 52-cell cross-tool compat matrix (incl.
iperf3 3.12 cross-pairs), all clean. The release campaign (N=30 per cell)
puts riperf3 at parity-or-faster in every cell: TCP -P1 at parity, TCP -P8
+4-10%, UDP +9-14% (Welch's t, p<0.05; baseline saved).

Two user-visible behavior corrections to note: `-f` uppercase letters now
mean BYTE-rates as in iperf3 (`-f K` previously printed Kbits/sec), and
`--server-max-duration` now refuses over-limit requests upfront at param
exchange like iperf3 (previously an unfaithful mid-test timer; the in-flight
watchdog is now iperf3's duration+omit+40s grace, flag-independent).

### Fixed

- **The setup-starvation flake family's root cause** (#195): `udp_connect_client` died on the first transient ICMP bounce (the server's per-stream listener rebind gap) with ~29 s of handshake budget unused — it now rides through transient reset/refused feedback within its unchanged 30 s deadline, with distinct exhaustion diagnoses. Test-harness layers: deadlines above the client's own, a bounded pre-data retry with mode-aware classifiers, `udp_serial` coverage, diagnosable panics. Verified across 20/20 repeated local stress rounds and 24/24 CI stress runs.
- **Server self-terminate is wire-faithful** (#224): `--server-bitrate-limit` and the `--server-max-duration` timer relay `SERVER_ERROR(-2)` + the `(i_errno, errno)` pair — not `SERVER_TERMINATE` — with no summary dump and the one-off exiting 0, exactly like iperf 3.21 (live-verified both directions, including real-iperf3 peers and 3.12). The client adopts the relayed `iperf_strerror` (codes 27/37/120/160 mapped; `int_errno=%d` fallback; unconditional errno append).
- **Exactly one JSON render on terminate paths** (#225): the CLI no longer appends a second document (`-J`) or a second error+end event pair (`--json-stream`) after the library already rendered the error into the active sink.
- **UDP bidir `-J` end block matches iperf3's shape exactly** (#214): six UDP-shaped aggregates (TCP bidir keeps four; pinned negative), `sum` ordered first, sender-figure bytes/packets/lost_percent provenance (live-proven on lossy and terminated runs), per-direction jitter averaged over `num_streams`, the server's strict no-graft zeros, and the per-stream sender-figure rule.
- **`--json-stream` wins the mode dispatch over `-J`** on both roles (#220): the hybrid is stream mode (full event stream incl. `end`; the document only under `--json-stream-full-output`), matching `OPT_JSON_STREAM`'s implies-`-J` rule.
- **Adaptive unit auto-scaling** (#221): no more forced `-f m` default — absent `-f`, every figure auto-scales like iperf3, with the `unit_snprintf` precision ladder (<10 → 2 dp, <100 → 1 dp, else 0 dp, round-aware boundaries) in adaptive and fixed modes, down to the `0.00 Bytes` stall rows; the Transfer column is always adaptive (`-f` drives only Bitrate).
- **The missing text lines** (#222): unconditional connect banners (`Connecting to host …` / `Accepted connection from <host>, port <p>` with v4-mapped addresses unmapped), the `Reverse mode … is sending` banner, per-stream preambles on both roles, `iperf Done.` closing clean client runs, and the full `-V` detail block in iperf3's order and timing (version/uname, `Control connection MSS`, defaulted-UDP block size, `Time:`, Cookie/TCP MSS/`Target Bitrate`, `Starting Test:` with the bytes/blocks/time variants, `Test Complete. Summary Results:`, sender-side CPU utilization, snd/rcv congestion) — printed post-param-exchange so `--get-server-output` relays carry them.
- **The reporter end-race** (#159): the final interval flush now runs after the senders stop (done → grace → finish), so intervals always cover what the END block accounts — the windows-latest dropped-tick/empty-intervals family's mechanism. The flush-after-stop invariant is debug-asserted and mutation-pinned; the resulting intervals==END property is stricter than iperf 3.21's own behavior.
- **The max-duration timer could never fire alongside a bitrate limit**
  (#237): the select loop recreated the sleep every iteration, so 1 Hz rate
  ticks reset it; one absolute pinned deadline now survives any loop
  re-entry.
- **`--server-max-duration` is the upfront param-exchange check** (#230):
  `(time + omit) > max` or an unbounded request (`-n`/`-k`/`-t 0`) refuses
  with `SERVER_ERROR` + errno 37 before the test starts — text/`-J`/
  `--json-stream` refusal shapes live-matched to iperf3, persistent servers
  serve the next test. Byte/block-limited clients now send `time: 0` on the
  wire like iperf3, so the unbounded rule works cross-tool. The 160-watchdog
  arms at iperf3's `duration + omit + 40s` for every bounded test, and
  self-terminate now closes stream tasks like `server_timer_proc` — a wedged
  peer can no longer hang the server's shutdown (previously: forever).
- **`-f` uppercase = byte-rates, and the server's `-f` works at all** (#241,
  #242): eight case-sensitive format letters with iperf3's 1024-divisor
  byte-rate ladder; the server-side flag — previously silently ignored — is
  wired at the interval reporter and both summary paths. The Transfer column
  stays always-adaptive, like iperf3.
- **Per-stream UDP packets/lost_percent provenance** (#238, #239): the pct
  denominator is strictly the sender-side count with iperf3's asymmetric
  fallbacks (packets falls back to the measured count; lost_percent goes to
  0.0, never the measured pct — live-proven on terminated runs), and client
  sender entries report the LOCAL sent count rather than the peer-measured
  figure.
- **TCP bidir retransmit aggregates** (#236): per-direction totals like
  iperf3's per-pass accumulator — the reverse-sent aggregate now carries the
  peer's exchanged per-stream counts (previously a fabricated 0; live-proven
  exact cross-tool), the forward aggregate can no longer mix directions, and
  single-direction `-R` docs show the peer's real totals.
- **Receiving-side packet figures consume the peer's exchanged counts**
  (#235, consume half): exact against true-counter (iperf3) peers where
  bytes-derived figures lose the tail partial datagram; netted of the peer's
  omitted baseline with hardened fallbacks (zero/negative/mixed exchanged
  sets). The counter half for riperf3's own senders is #256 (0.8.0).
- **Signals are honored at the client's central state wait** (#231): a
  SIGTERM/SIGINT landing in the setup phases or the post-test waits now
  dumps and exits signal-normal like `iperf_catch_sigend` (previously it
  hung until the control read returned — against a wedged server, forever);
  pre-data dumps report a zero-second window and post-exchange dumps keep
  the peer halves, both like iperf3.

### Added

- `hammer.yml`: a permanent `workflow_dispatch` flake hammer (N shard runners × M suite iterations, validated inputs, red-baseline caching) for statistical pre-merge verification.
- `timeout-minutes` on every CI job and `permissions: contents: read` on ci.yml.
- **The server's `-V` placeholder rows** (#246): `[ N] (sender/receiver
  statistics not available)` for the unmeasured half, verbose-gated and
  TCP-`[SUM]`-only, exactly matching iperf3's emission sites and row slots.

### Deferred to 0.8.0

#245 (live TCP_INFO through the final flush), #256 (true datagram counters
in the UDP senders — #235's exactness half), #248 (the perr `: ` suffix on
relayed duration-expired lines).

## [0.7.3] - 2026-06-11

A non-breaking patch continuing 0.7.2's faithfulness campaign: 18 PRs closing
24 issues — signal/failure-mode parity with iperf3's sigend and select loops,
bidir text-output parity, json-stream completions, error-sink parity, strftime
timestamps, platform fixes for Windows/FreeBSD/macOS, and a shared
test-support crate. Every merge was gated on the cross-tool compat matrix
(48-52 cells incl. iperf3 3.12 cross-pairs), all clean — fourteen consecutive
gates, plus a perf-drift bench against the 0.7.2 baseline at parity.

### Added
- **Signal-handling parity with iperf_got_sigend** (#210, completing #158): a
  signal mid-test dumps the accumulated stats on both roles, tells the peer
  via CLIENT_TERMINATE/SERVER_TERMINATE (cross-tool verified live against
  iperf3 in both directions), and exits via the signal-normal path; a second
  signal during the dump window hard-exits, dying by the signal. New additive
  lib API: `interrupt`/`with_interrupt` (a watch channel carrying the
  caller's message into the `-J` error key). The server's CLIENT_TERMINATE
  handling also gained the missing stats dump (and stopped leaking the
  reporter).
- **`--json-stream-full-output`** (#213): the third leg of discard_json —
  intervals retained on both roles and the complete monolithic document
  printed after the stream's `end` event.
- **Error-sink parity with iperf_exit** (#198): `-J` errors land in the JSON
  document's `error` key on stdout (byte-matching iperf3's pre-test document
  shape) with stderr silent; `--json-stream` emits `error` + empty `end`
  events; `--logfile` receives the error line; usage errors exit 1 like
  getopt (with iperf3's parameter-error sentences and usage trailer for the
  no-mode and both-modes cases). Parse-time rejections stay on stderr in
  every mode, matching iperf3's post-parse-only sink gating.
- **Control-socket watch during the data phase** (#170): control death mid-test
  surfaces promptly as `control socket has closed unexpectedly` in every
  end-condition mode (`-n`/`-k` previously had no watch and could hang);
  `SERVER_TERMINATE` renders a partial summary from local data (peer half
  zeroed, like iperf3) before `the server has terminated`; `-J` carries the
  blob `error` key, `--json-stream` the `error` event.
- **`-b rate/burst` honored** (#160): iperf3's multisend batch on both TCP and
  UDP, both roles, carried in the param exchange; present burst range-checked
  per IEBURST. Pacing green-light waits are interruptible (a burst-sized debt
  no longer outlives the test); `--pacing-timer` accepts unit suffixes; the
  TCP sender re-checks `done` after the throttle sleep.
- **Server-side `--bind-dev`** (#149): applied pre-bind on the TCP listener
  and every UDP server socket, like iperf3's netannounce (Linux-only on the
  server — netannounce is SO_BINDTODEVICE-only; the client keeps Linux+macOS).
  Platforms that can't honor it now reject at config time instead of silently
  no-opping (FreeBSD/NetBSD).
- **Windows signal handling** (#158): Ctrl+C/Break/close/logoff/shutdown take
  the clean pidfile-unlink path (tokio ≥ 1.44 for the handler-park fix); the
  pidfile is unlinked via an RAII guard (panics included); a second signal
  during teardown exits immediately, dying by the signal.
- **`riperf3-test-support`** (#192): dev-only crate consolidating the
  port allocator, UDP serialization lock, refused-retry runner, and child
  guard; fixed bind sleeps retired from the test harness (#177).
- Paced-UDP compat coverage: `-b 100M` forward/reverse cells in the interop
  matrix (#163's missing signal).

### Changed
- **Repeated CLI flags are last-wins** (#205), like iperf3's getopt — wrapper
  scripts appending override flags work against the drop-in.
- **`-l 0` means "unset"** and block size is bounds-checked at config like
  iperf3 (IEBLOCKSIZE / IEUDPBLOCKSIZE), including negotiated params from
  broken/hostile peers (#188).
- **`-S/--tos` accepts strtol base-0 forms** (hex/octal) with the IEBADTOS
  range check (#167).
- **Top-level CLI errors print iperf3's shape** (`riperf3: error - ...`,
  exit 1); control-connect failures carry the canonical IECONNECT sentence
  (#151).
- **UDP `SO_SNDBUF` is grow-only and skipped entirely under `-w`** (#163):
  iperf3 never sets it outside `-w`; the old unconditional set shrank the
  buffer up to ~90× at small batch products. (The filed #163 throughput
  symptom was fixed by 0.7.2's #190 quantum batching — verified by a rate
  sweep at 10M/100M/1G and burst=1.)
- **The `-O` omit boundary keeps TCP_INFO extremes** (#199): iperf3's
  iperf_reset_stats never clears max cwnd/snd_wnd/RTT (or the RTT mean's sum);
  the old full reset under-read every `max_*` after a warm-up.
- **`--timestamps[=FORMAT]` renders localtime through strftime** (#202) with
  iperf3's `"%c "` default and verbatim user formats (was hardcoded HH:MM:SS
  UTC ignoring the argument); the prefix now covers every printed line —
  verbose output and the server's listening banner included, like
  iperf_printf (#216). Windows keeps a documented HH:MM:SS fallback.
- **Per-tick `- - - -` separator at `-P > 1`** (#204), matching
  iperf_print_intermediate's first-stream rule (live A/B byte-parity,
  including the no-header-reprint-after-omit shape).

### Fixed
- **Bidir text interval rows match iperf_print_intermediate** (#143, #187):
  role tags (`[TX-C]`-style) on rows and SUMs, per-direction passes (a
  direction's rows then ITS SUM), no SUM at P=1, no cross-direction
  aggregate, the `[Role]` bidir header, mode-selected UDP headers, and UDP
  sender rows carry iperf3's trailing sent-datagram count.
- **END-block SUM jitter is the mean across streams** (#169), not the max —
  divergent at `-P ≥ 2` UDP.
- **`--get-server-output` × `--json-stream`** (#168): a json-stream server
  attaches its JSON report (populated intervals, per discard_json); a
  json-stream client emits `server_output_*` events before `end`;
  `--timestamps` prefixes ride the capture per line.
- **Server UDP paths apply TOS** (#154) on both architectures, like
  iperf_common_sockopts — reverse/bidir egress is marked.
- **Live `tcpi_snd_wnd`/`tcpi_reord_seen` on Linux** (#161, partial): a UAPI
  tcp_info mirror unlocks the fields libc truncates; FreeBSD forwards its
  value. (macOS's `-1` sentinel needs i64 report fields — deferred to 0.8.0.)
- **Windows reset-class ICMP noise** (#180) no longer aborts UDP demux setup
  or the shared teardown drain.
- The UDP pacing test tolerates a dropped final interval on loaded Windows
  runners (#159 — the reporter end-race is tracked for 0.7.4).

## [0.7.2] - 2026-06-10

A non-breaking patch closing out the long-standing faithfulness backlog: every
accepted-but-inert or diverging flag now behaves like iperf3. No public API
break (`cargo-semver-checks`: no semver update required). Release gate: the
cross-tool compatibility matrix is all-PASS (48/48, incl. iperf3 3.12
interop), a fresh full N=30 throughput campaign vs iperf3 is regression-free
(12 cells faster / 3 parity / 1 within single-stream noise, confirmed by a
controlled same-environment 0.7.1-vs-0.7.2 A/B at parity), and the new
`-O`/`--get-server-output` paths are cross-tool verified in both roles — see
[BENCHMARKS.md](BENCHMARKS.md).

### Added
- **Real `-O/--omit` semantics** (#31): the run lasts `omit + time`, statistics
  reset at the boundary (interval timeline restarts at 0), the summary covers
  only the post-omit window, and `-J` carries `test_start.omit` plus
  `"omitted"` intervals. The `-n`/`-k` end check copies iperf3's asymmetric
  test-level accounting — sent counts post-omit net, received counts gross —
  so reverse/bidir limits end at the boundary exactly like iperf3. Capped at
  600 s (`MAX_OMIT_TIME`).
- **`--get-server-output` wired end to end** (#33): the server returns its
  rendered output (text) or report (`-J`) in the results exchange; the client
  prints it after its own report or attaches it to the `-J` blob — closing the
  last inert builder field (#122).
- **`--pacing-timer` wired** (#32): the `-b` throttle wakes on the configured
  quantum (default 1000 µs, always sent in the param exchange like iperf3).
- **Real macOS Retr/Cwnd** (#96): `tcpi_txretransmitpackets` via a hand-rolled
  `tcp_connection_info` binding (the `libc` crate's layout is mislaid),
  restoring the Retr and Cwnd columns on macOS.
- **MSRV declared**: `rust-version = "1.85"` (floor set by the dependency
  tree), enforced by a pinned-toolchain CI job. Plus CONTRIBUTING.md,
  SECURITY.md (private vulnerability reporting enabled), a README accuracy
  pass, and rustdoc for all 71 builder setters (#165).

### Changed
- **The `-b` rate limiter is iperf3's cumulative-average throttle** (#116): a
  send is green-lit while cumulative bytes ≤ elapsed × rate, bounding
  overshoot to one block. The old token bucket's burst floor overshot a
  low-rate `-b` by up to 2× with TCP's 128 KiB default block. Cross-tool
  byte-parity verified at 1M/200M/5G/10G.
- **Per-option socket-option error policy** (#45), matching iperf3: `-S/--tos`
  failures are fatal (`IESETTOS`, incl. IPv6 `IPV6_TCLASS`); best-effort
  options stay tolerated, each documented at its call site.
- **Conflicting end conditions are rejected** (#140): `-t` with `-n`/`-k`
  errors like iperf3's `IEENDCONDITIONS` (value-based, after `unit_atoi`-style
  scale-then-truncate parsing); `-i` outside {0} ∪ [0.1, 60] errors like
  `IEINTERVAL`.
- **`--bind-dev` applies before `connect()`** (#88) for TCP control and data
  sockets (previously bound after, so routing didn't take effect), with macOS
  IPv6 support via `IPV6_BOUND_IF`.

### Fixed
- **Bidir `-J` interval sums split per direction** (#54): `sum` +
  `sum_bidir_reverse`, mirroring the end block; interval jitter is the mean
  across receiving streams (#142), like iperf3's `avg_jitter`.
- **The pidfile is unlinked on every exit path** (#105): SIGINT/SIGTERM
  handlers (installed before the pidfile is written) and normal exit; signal
  exits report code 0 with iperf3's interrupt notice.
- **Exchanged retransmit totals are real** (#156): `sender_has_retransmits: 1`
  now ships per-stream kernel totals snapshotted while the socket is open
  (previously `-1`, which iperf3 3.12 renders as a huge bogus count), rebased
  to the post-omit window under `-O` (#171).
- **The abort path joins the interval reporter** (#147): a server-initiated
  abort no longer leaks a parked reporter task into a library consumer's
  runtime.
- **FreeBSD `snd_cwnd` is bytes, not bytes×MSS** (#155).

### Internal
- Dead duplicate report fields removed (#139); per-PR post-merge drift gates
  (compat + bench vs baseline) ran clean throughout the campaign.

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
