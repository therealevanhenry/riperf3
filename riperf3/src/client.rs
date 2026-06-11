use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::net::TcpStream;

use crate::cpu::CpuSnapshot;
use crate::error::{ConfigError, Result, RiperfError};
use crate::net;
use crate::protocol::{self, TestParams, TestResultsJson, TestState, TransportProtocol};
use crate::stream::{self, DataStream, StreamCounters, UdpRecvStats};
use crate::utils::*;

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq)]
pub struct Client {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) protocol: TransportProtocol,
    pub(crate) duration: u32,
    pub(crate) num_streams: u32,
    pub(crate) blksize: usize,
    /// Whether `blksize` came from an explicit `-l`. When false for UDP, the
    /// datagram size is derived from the control-socket MSS at run time
    /// (iperf3 parity, issue #6) rather than using the `blksize` default.
    /// Internal: set by the builder from whether `.blksize()` was called.
    blksize_explicit: bool,
    pub(crate) reverse: bool,
    pub(crate) bidir: bool,
    pub(crate) omit: u32,
    pub(crate) no_delay: bool,
    pub(crate) mss: Option<i32>,
    pub(crate) window: Option<i32>,
    pub(crate) bandwidth: u64,
    /// `-b rate/burst` block count (0 = unset) — iperf3's multisend batch (#160).
    pub(crate) burst: u32,
    pub(crate) pacing_timer: u32,
    pub(crate) tos: i32,
    pub(crate) congestion: Option<String>,
    pub(crate) udp_counters_64bit: bool,
    pub(crate) connect_timeout: Option<Duration>,
    pub(crate) title: Option<String>,
    pub(crate) extra_data: Option<String>,
    pub(crate) verbose: bool,
    pub(crate) json_output: bool,
    pub(crate) json_stream: bool,
    /// #210: fired by the consumer (the CLI's first signal) with the
    /// formatted interrupt message; the run dumps stats, sends
    /// CLIENT_TERMINATE, and returns normally.
    pub(crate) interrupt: Option<InterruptWatch>,
    pub(crate) json_stream_full_output: bool,
    pub(crate) bytes_to_send: Option<u64>,
    pub(crate) blocks_to_send: Option<u64>,
    pub(crate) repeating_payload: bool,
    pub(crate) zerocopy: bool,
    pub(crate) gsro: bool,
    pub(crate) sendmmsg: bool,
    pub(crate) dont_fragment: bool,
    pub(crate) cport: Option<u16>,
    pub(crate) get_server_output: bool,
    pub(crate) forceflush: bool,
    pub(crate) timestamps: Option<String>,
    pub(crate) bind_address: Option<String>,
    pub(crate) bind_dev: Option<String>,
    pub(crate) fq_rate: Option<u64>,
    pub(crate) flowlabel: Option<i32>,
    pub(crate) ip_version: Option<u8>,
    pub(crate) mptcp: bool,
    pub(crate) skip_rx_copy: bool,
    pub(crate) rcv_timeout: Option<u64>,
    pub(crate) snd_timeout: Option<u64>,
    pub(crate) file: Option<String>,
    pub(crate) format_char: char,
    pub(crate) interval: Option<f64>,
    pub(crate) cntl_ka: Option<String>,
    pub(crate) username: Option<String>,
    pub(crate) password: Option<String>,
    pub(crate) rsa_public_key_path: Option<String>,
    pub(crate) use_pkcs1_padding: bool,
}

/// Build the peer half of a stream's end-block pair from the server's
/// per-stream results entry (#184, generalizing #25): the opposite role of
/// the local stream. When the peer RECEIVED (local sender), its measured
/// loss/jitter is the only receiver view that exists — without it, forward
/// UDP looks loss-free even when the link drops packets (#25). When the peer
/// SENT, iperf3's sender line shows zero jitter/loss over the sent datagram
/// count. The exchange carries GROSS packets/errors plus omitted_* baselines;
/// subtract for the post-omit summary (#31) — this also reads a real iperf3
/// server's omit results correctly. `is_udp` gates the datagram columns so a
/// TCP pair line stays a plain byte line.
fn peer_half_summary(
    x: &protocol::StreamResultJson,
    local_is_sender: bool,
    is_udp: bool,
    peer_has_retransmits: bool,
    end: f64,
    role_tag: Option<&'static str>,
) -> crate::reporter::StreamSummary {
    let peer_is_sender = !local_is_sender;
    let (jitter, lost, total) = if !is_udp {
        (None, None, None)
    } else if peer_is_sender {
        // Peer sent: zero jitter/loss over its sent count.
        (Some(0.0), Some(0), Some(x.packets - x.omitted_packets))
    } else {
        // Peer received: its measured stats.
        (
            Some(x.jitter),
            Some(x.errors - x.omitted_errors),
            Some(x.packets - x.omitted_packets),
        )
    };
    // A TCP peer sender renders the retransmit total it exchanged (#156/#184),
    // when it reported having one; receivers and UDP carry none.
    let retransmits = (!is_udp && peer_is_sender && peer_has_retransmits).then_some(x.retransmits);
    crate::reporter::StreamSummary {
        stream_id: x.id,
        start: 0.0,
        end,
        bytes: x.bytes,
        is_sender: peer_is_sender,
        retransmits,
        jitter,
        lost,
        total_packets: total,
        role_tag,
    }
}

/// Bytes transferred so far against an `-n`/`-k` limit. Faithful to iperf3's
/// `bytes_sent >= N || bytes_received >= N` end check (`iperf_client_api.c`):
/// the client's senders accumulate in forward, its receivers in reverse, and in
/// bidir whichever direction reaches the limit first ends the test. Counting
/// only sent bytes leaves a reverse `-n`/`-k` test spinning forever (#60).
fn transferred_bytes(streams: &[DataStream]) -> u64 {
    // The two sides are deliberately asymmetric, copying iperf3's test-level
    // counters (#31, review r3): iperf_reset_stats zeroes test->bytes_sent at
    // the omit boundary (iperf_api.c:3675) so the SEND side counts post-omit
    // NET — but it never touches test->bytes_received, so the RECEIVE side
    // counts GROSS, warm-up included (end check, iperf_client_api.c:771-772).
    // The asymmetry is load-bearing: gross received is monotonic, so a
    // reverse/bidir limit cannot race either reporter's boundary baselines
    // (the pre-r3 net-received check hung when a mistimed baseline swallowed
    // warm-up bytes).
    let sent: u64 = streams
        .iter()
        .filter(|s| s.is_sender)
        .map(|s| s.counters.bytes_sent_net())
        .sum();
    let received: u64 = streams
        .iter()
        .filter(|s| !s.is_sender)
        .map(|s| s.counters.bytes_received())
        .sum();
    sent.max(received)
}

/// What the mid-test control watch observed (#170).
/// The #210 interrupt receiver, newtyped so `Client`'s PartialEq derive (the
/// CLI-glue test convention) keeps working: two wired watches compare equal —
/// only PRESENCE is part of a config comparison.
#[derive(Clone, Debug)]
pub struct InterruptWatch(pub(crate) tokio::sync::watch::Receiver<Option<String>>);

impl PartialEq for InterruptWatch {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

// The watch receiver is not UnwindSafe (its shared slot uses interior
// mutability), which would strip the marker from Client/Server and their
// builders — a semver break (CI's auto_trait_impl_removed). riperf3's usage
// is panic-consistent: the receiver is only ever POLLED (changed +
// borrow_and_update) and the channel's state is a version counter plus an
// Arc'd value slot, so observing it across an unwind cannot expose a broken
// invariant of ours.
impl std::panic::UnwindSafe for InterruptWatch {}
impl std::panic::RefUnwindSafe for InterruptWatch {}

#[derive(Debug)]
enum ControlEvent {
    /// A local interrupt (the CLI's first signal, #210) carrying the
    /// formatted iperf3 message ("interrupt - the client has terminated by
    /// signal …"); the test dumps its stats and sends CLIENT_TERMINATE.
    Interrupted(String),
    /// SERVER_TERMINATE arrived: stop, render a partial summary, error with
    /// iperf3's IESERVERTERM.
    Terminated,
    /// SERVER_ERROR arrived (#224): the server failed and is relaying its
    /// (i_errno, errno) pair; the PAYLOAD is still on the socket — the
    /// consumer reads it outside the select (watch_control must stay
    /// cancel-safe: a single 1-byte read).
    ServerError,
    /// The control connection died (EOF or I/O error): iperf3's select sees
    /// it immediately and errexits with IECTRLCLOSE.
    Closed,
}

/// Watch the control socket during the data phase, like iperf3's select over
/// control + data fds (#170). Cancel-safe (recv_state is a single 1-byte
/// read). Any state OTHER than ServerTerminate is logged and ignored — iperf3
/// treats e.g. a re-sent TEST_RUNNING as a no-op, and the old code's
/// first-byte-ends-the-wait behavior turned stray bytes into a truncated test.
/// Resolve when the library consumer fires the interrupt watch (#210);
/// pends forever when no watch is wired, so it is select-safe everywhere.
pub(crate) async fn wait_interrupt(
    rx: Option<&mut tokio::sync::watch::Receiver<Option<String>>>,
) -> String {
    match rx {
        Some(rx) => loop {
            if rx.changed().await.is_err() {
                // Sender dropped without firing: never resolve.
                std::future::pending::<()>().await;
            }
            if let Some(msg) = rx.borrow_and_update().clone() {
                return msg;
            }
        },
        None => std::future::pending().await,
    }
}

async fn watch_control(ctrl: &mut tokio::net::TcpStream) -> ControlEvent {
    loop {
        match protocol::recv_state(ctrl).await {
            Ok(TestState::ServerTerminate) => return ControlEvent::Terminated,
            Ok(TestState::ServerError) => return ControlEvent::ServerError,
            Ok(other) => {
                log::debug!("ignoring control state {other:?} during the data phase");
            }
            // Recorded deviation (r1 n3): iperf3 splits EOF (IECTRLCLOSE) /
            // read error (IERECVMESSAGE) / unknown state byte (IEMESSAGE);
            // riperf3 folds all three into the closed class — the headline
            // kill case (FIN→EOF) matches byte-for-byte.
            Err(_) => return ControlEvent::Closed,
        }
    }
}

impl Client {
    /// Chainable form of [`ClientBuilder::interrupt`] for an already-built
    /// client (#210).
    pub fn with_interrupt(mut self, rx: tokio::sync::watch::Receiver<Option<String>>) -> Self {
        self.interrupt = Some(InterruptWatch(rx));
        self
    }

    pub async fn run(&self) -> Result<TestResultsJson> {
        let mut interrupt = self.interrupt.clone().map(|w| w.0);
        // -T/--title: prefix every client text line with "<title>:  " (#34),
        // matching iperf3. Run-scoped (cleared on drop) and only in plain-text
        // mode — `-J` and `--json-stream` emit machine JSON, which iperf3 never
        // titles. Held for the whole run so the reporter task and the preamble
        // both see it.
        let _title_guard = (!self.json_output && !self.json_stream)
            .then(|| crate::macros::OutputTitleGuard::set(self.title.clone()));
        // --timestamps prefixes every text report line, run-scoped like the
        // title; never in the machine-JSON modes (#168).
        let _ts_guard = (!self.json_output && !self.json_stream)
            .then_some(self.timestamps.as_deref())
            .flatten()
            // The bare-flag "%c " default is clap's default_missing_value;
            // by here the format is always concrete.
            .map(crate::macros::OutputTimestampGuard::set);

        // ---- Generate cookie and connect ----
        let cookie = protocol::make_cookie();
        let mut ctrl = net::tcp_connect(
            &self.host,
            self.port,
            self.connect_timeout,
            None,
            self.bind_address.as_deref(),
            self.bind_dev.as_deref(),
            self.mptcp,
            self.ip_version,
        )
        .await
        .map_err(|e| {
            // iperf3 raises IECONNECT for ANY netdial failure
            // (iperf_client_api.c:441) — refused, timed out (netdial sets
            // ETIMEDOUT), and bind-local failures alike — so wrap every
            // error from the control connect. The io kind is preserved so
            // callers (and the test harness's refused-retry) can still
            // classify it (#151). The `(os error N)` suffix std's io::Error
            // appends is a deliberate, recorded deviation from iperf3's bare
            // strerror text: substring matchers survive, and strerror text
            // varies by platform/locale anyway (review r1 n4).
            let (kind, detail) = match e {
                RiperfError::Io(io) => (io.kind(), io.to_string()),
                // iperf3's suffix is strerror(ETIMEDOUT) — glibc's text;
                // macOS/BSD say "Operation timed out" (recorded, like the
                // os-error suffix above).
                RiperfError::ConnectionTimeout => (
                    std::io::ErrorKind::TimedOut,
                    "Connection timed out".to_string(),
                ),
                // Not dial failures: the family-conflict validation (#15)
                // keeps its Protocol classification (pinned by the lib
                // tests). Recorded deviations sharing that variant (a
                // net.rs error split would be needed to reclassify): a
                // failed `-B` local bind (review r1 n3) and resolve_host's
                // "no IPvX address found" (r2 n2) — both fold into
                // IECONNECT in iperf3's netdial.
                other => return other,
            };
            RiperfError::Io(std::io::Error::new(
                kind,
                format!(
                    "unable to connect to server - server may have stopped running \
                     or use a different port, firewall issue, etc.: {detail}"
                ),
            ))
        })?;
        net::configure_tcp_stream(&ctrl, true)?;

        // The control connection's MSS sizes UDP datagrams (issue #6) and feeds
        // the `-J` start.tcp_mss_default field (#36 PR3).
        let control_mss_opt = net::tcp_maxseg(&ctrl);
        let control_mss = control_mss_opt.unwrap_or(0);

        // Resolve the UDP datagram size now that the control connection exists:
        // when `-l` wasn't given, derive it from the control-socket MSS so a
        // jumbo-frame path uses large datagrams instead of the 1460 floor
        // (iperf3 parity, issue #6). TCP keeps its own block size unchanged.
        let blksize = if self.protocol == TransportProtocol::Udp && !self.blksize_explicit {
            resolve_udp_blksize(None, control_mss_opt)
        } else {
            self.blksize
        };

        // Apply control connection options (bind_dev is applied inside
        // tcp_connect, pre-connect — #88)
        if let Some(ref spec) = self.cntl_ka {
            let (idle, intv, cnt) = parse_keepalive(spec);
            net::set_tcp_keepalive(&ctrl, idle, intv, cnt)?;
        }

        protocol::send_cookie(&mut ctrl, &cookie).await?;

        if self.verbose {
            vprintln!("Connecting to host {}, port {}", self.host, self.port);
        }

        let done = Arc::new(AtomicBool::new(false));
        // Signal `done` on every exit path (incl. early `?` returns) so a UDP
        // sender parked on the start barrier can't leak if setup fails (#5).
        let _done_guard = stream::DoneOnDrop(done.clone());
        // Released at TestStart so UDP senders don't transmit during stream
        // setup (issue #5): the create-streams handshake is lost under a flood.
        let start = Arc::new(AtomicBool::new(false));
        // Interval samples + TCP_INFO extremes the reporter collects during the
        // run, read back at DisplayResults for the `-J` blob (#36 PR2).
        let interval_data = Arc::new(Mutex::new(crate::reporter::CollectedIntervals::default()));
        let mut streams: Vec<DataStream> = Vec::new();
        let mut byte_budget: Option<(Arc<AtomicI64>, i64)> = None;
        let mut cpu_start: Option<CpuSnapshot> = None;
        let mut server_results: Option<TestResultsJson> = None;
        // Authoritative test duration captured from run_test: `-t` for a duration
        // run, the measured elapsed for `-n`/`-k`. Drives the summary window (#103).
        let mut measured_secs = self.duration as f64;
        // Wall-clock at TestStart, for the `-J` start.timestamp (#36 PR3).
        let mut test_start_millis = 0u64;

        // ---- State machine: react to server-driven transitions ----
        loop {
            let state = protocol::recv_state(&mut ctrl).await?;

            match state {
                TestState::ParamExchange => {
                    let params = self.build_params(blksize);
                    protocol::send_params(&mut ctrl, &params).await?;
                }

                TestState::CreateStreams => {
                    (streams, byte_budget) =
                        self.create_streams(&cookie, &done, &start, blksize).await?;
                }

                TestState::TestStart => {
                    // All streams are set up — release the UDP senders.
                    start.store(true, Ordering::Relaxed);
                    cpu_start = Some(CpuSnapshot::now());
                    test_start_millis = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as u64)
                        .unwrap_or(0);

                    // --json-stream: emit the `start` event now, before the reporter
                    // streams any `interval` events (faithful event order, #62).
                    if self.json_stream {
                        self.emit_json_stream_start(
                            &streams,
                            cpu_start.as_ref(),
                            blksize,
                            &interval_data,
                            &StartMeta {
                                cookie: String::from_utf8_lossy(
                                    &cookie[..protocol::COOKIE_SIZE - 1],
                                )
                                .into_owned(),
                                tcp_mss_default: control_mss,
                                start_time_millis: test_start_millis,
                            },
                        );
                    }
                }

                TestState::TestRunning => {
                    let (secs, event) = self
                        .run_test(
                            &mut ctrl,
                            &streams,
                            &done,
                            blksize,
                            interval_data.clone(),
                            byte_budget.as_ref(),
                            &mut interrupt,
                        )
                        .await?;
                    measured_secs = secs;
                    match event {
                        Some(ControlEvent::Terminated) => {
                            // SERVER_TERMINATE mid-test: iperf3 temporarily
                            // flips to DISPLAY_RESULTS, renders a summary from
                            // the PARTIAL local data (no peer half), then
                            // errexits with IESERVERTERM (#170). A -J run
                            // carries the message in the blob's "error" key,
                            // like iperf_json_finish.
                            self.print_results(
                                &streams,
                                cpu_start.as_ref(),
                                None,
                                blksize,
                                &interval_data,
                                &StartMeta {
                                    cookie: String::from_utf8_lossy(
                                        &cookie[..protocol::COOKIE_SIZE - 1],
                                    )
                                    .into_owned(),
                                    tcp_mss_default: control_mss,
                                    start_time_millis: test_start_millis,
                                },
                                measured_secs,
                                Some("the server has terminated"),
                            );
                            return Err(RiperfError::ServerTerminated);
                        }
                        Some(ControlEvent::Interrupted(msg)) => {
                            // iperf_got_sigend (#210): dump the accumulated
                            // stats (the same DISPLAY_RESULTS flip), tell the
                            // peer via CLIENT_TERMINATE, and return normally —
                            // the signal-normal exit is the CALLER's business
                            // (iperf3 exits 0 on TERM/INT/HUP).
                            let _ =
                                protocol::send_state(&mut ctrl, TestState::ClientTerminate).await;
                            self.print_results(
                                &streams,
                                cpu_start.as_ref(),
                                None,
                                blksize,
                                &interval_data,
                                &StartMeta {
                                    cookie: String::from_utf8_lossy(
                                        &cookie[..protocol::COOKIE_SIZE - 1],
                                    )
                                    .into_owned(),
                                    tcp_mss_default: control_mss,
                                    start_time_millis: test_start_millis,
                                },
                                measured_secs,
                                Some(&msg),
                            );
                            return Ok(self.build_results(
                                &streams,
                                cpu_start.as_ref(),
                                blksize,
                                measured_secs,
                            ));
                        }
                        Some(ControlEvent::ServerError) => {
                            // #224: read the relay pair (safe here, outside
                            // the select) and ADOPT the mapped error like
                            // iperf_handle_message_client. NO text dump —
                            // that is SERVER_TERMINATE's shape — but the
                            // JSON sinks render the full document/events
                            // with the error inside, like iperf3's json_top
                            // (the CLI suppresses its generic re-render).
                            let msg = match protocol::read_server_error_payload(&mut ctrl).await {
                                Some((i_errno, os_errno)) => {
                                    crate::error::iperf3_strerror(i_errno, os_errno)
                                }
                                None => "server error".to_string(),
                            };
                            if self.json_output || self.json_stream {
                                self.print_results(
                                    &streams,
                                    cpu_start.as_ref(),
                                    None,
                                    blksize,
                                    &interval_data,
                                    &StartMeta {
                                        cookie: String::from_utf8_lossy(
                                            &cookie[..protocol::COOKIE_SIZE - 1],
                                        )
                                        .into_owned(),
                                        tcp_mss_default: control_mss,
                                        start_time_millis: test_start_millis,
                                    },
                                    measured_secs,
                                    Some(&msg),
                                );
                            } else {
                                // KNOWN CORNER (r1 n5): with --logfile set,
                                // iperf_err writes this line to the logfile;
                                // riperf3's logfile plumbing lives in the
                                // CLI (#198), so this lib line stays on
                                // stderr. Revisit with the sink plumbing.
                                eprintln!("riperf3: SERVER ERROR - {msg}");
                            }
                            return Err(RiperfError::ServerErrorRelayed(msg));
                        }
                        Some(ControlEvent::Closed) | None => {}
                    }
                    // Test finished — send TestEnd
                    protocol::send_state(&mut ctrl, TestState::TestEnd).await?;
                }

                TestState::ExchangeResults => {
                    let results =
                        self.build_results(&streams, cpu_start.as_ref(), blksize, measured_secs);
                    protocol::send_results(&mut ctrl, &results).await?;
                    server_results = Some(protocol::recv_results(&mut ctrl).await?);
                }

                TestState::DisplayResults => {
                    self.print_results(
                        &streams,
                        cpu_start.as_ref(),
                        server_results.as_ref(),
                        blksize,
                        &interval_data,
                        &StartMeta {
                            cookie: String::from_utf8_lossy(&cookie[..protocol::COOKIE_SIZE - 1])
                                .into_owned(),
                            tcp_mss_default: control_mss,
                            start_time_millis: test_start_millis,
                        },
                        measured_secs,
                        None,
                    );
                    protocol::send_state(&mut ctrl, TestState::IperfDone).await?;
                    break; // test complete — server will close the connection
                }

                TestState::IperfDone => break,

                TestState::AccessDenied => {
                    return Err(RiperfError::AccessDenied);
                }
                TestState::ServerError => {
                    // #224: read the (i_errno, errno) relay pair and ADOPT
                    // the mapped error, like iperf_handle_message_client.
                    // Text mode: the "SERVER ERROR - …" receipt line only
                    // (iperf_err's shape; no summary dump — that is
                    // SERVER_TERMINATE's). JSON sinks: render the full
                    // document/events with the error inside, like iperf3's
                    // json_top; the CLI suppresses its generic re-render.
                    let msg = match protocol::read_server_error_payload(&mut ctrl).await {
                        Some((i_errno, os_errno)) => {
                            crate::error::iperf3_strerror(i_errno, os_errno)
                        }
                        None => "server error".to_string(),
                    };
                    if self.json_output || self.json_stream {
                        self.print_results(
                            &streams,
                            cpu_start.as_ref(),
                            None,
                            blksize,
                            &interval_data,
                            &StartMeta {
                                cookie: String::from_utf8_lossy(
                                    &cookie[..protocol::COOKIE_SIZE - 1],
                                )
                                .into_owned(),
                                tcp_mss_default: control_mss,
                                start_time_millis: test_start_millis,
                            },
                            measured_secs,
                            Some(&msg),
                        );
                    } else {
                        eprintln!("riperf3: SERVER ERROR - {msg}");
                    }
                    return Err(RiperfError::ServerErrorRelayed(msg));
                }

                // iperf_handle_message_client handles SERVER_TERMINATE in
                // ANY state, not just TEST_RUNNING (#210 review r1 n2): a
                // server interrupt racing the client's TestEnd lands here
                // (the ExchangeResults wait) — dump the partial summary and
                // surface IESERVERTERM instead of dying later on a bare
                // peer-disconnect with no dump.
                TestState::ServerTerminate => {
                    self.print_results(
                        &streams,
                        cpu_start.as_ref(),
                        None,
                        blksize,
                        &interval_data,
                        &StartMeta {
                            cookie: String::from_utf8_lossy(&cookie[..protocol::COOKIE_SIZE - 1])
                                .into_owned(),
                            tcp_mss_default: control_mss,
                            start_time_millis: test_start_millis,
                        },
                        measured_secs,
                        Some("the server has terminated"),
                    );
                    return Err(RiperfError::ServerTerminated);
                }

                other => {
                    if self.verbose {
                        vprintln!("Unexpected state: {other:?}");
                    }
                }
            }
        }

        // ---- Clean up ----
        done.store(true, Ordering::Relaxed);
        tokio::time::sleep(Duration::from_millis(100)).await;
        for s in streams {
            let _ = s.task.await;
        }

        server_results.ok_or_else(|| {
            RiperfError::Protocol("missing server results in control exchange".into())
        })
    }

    fn build_params(&self, blksize: usize) -> TestParams {
        let mut p = TestParams::default();
        match self.protocol {
            TransportProtocol::Tcp => p.tcp = Some(true),
            TransportProtocol::Udp => p.udp = Some(true),
        }
        p.time = Some(self.duration as i32);
        p.omit = Some(self.omit as i32);
        p.parallel = Some(self.num_streams as i32);
        p.len = Some(blksize as i32);
        if self.reverse {
            p.reverse = Some(true);
        }
        if self.bidir {
            p.bidirectional = Some(true);
        }
        if self.no_delay {
            p.nodelay = Some(true);
        }
        p.mss = self.mss;
        p.window = self.window;
        // Always carry the resolved rate (incl. 0 = unlimited) so the server
        // paces correctly in reverse/bidir; matches iperf3, which always sends
        // it. `bandwidth` is the effective rate after the build-time default
        // (UDP unset → 1 Mbit/s), so 0 here unambiguously means unlimited (#17).
        p.bandwidth = Some(self.bandwidth);
        // Sent only when set, like iperf3 (`if (test->settings->burst)`): the
        // server's reverse/bidir sender batches on the client's burst (#160).
        p.burst = (self.burst > 0).then_some(self.burst as i32);
        // Always sent, like iperf3 (default 1000 µs): the server's
        // reverse/bidir sender paces on the client's quantum (#32).
        p.pacing_timer = Some(self.pacing_timer as i32);
        // --get-server-output (#33): ask the server to return its output in
        // the results exchange, exactly like iperf3's param.
        if self.get_server_output {
            p.get_server_output = Some(1);
        }
        if self.tos != 0 {
            p.tos = Some(self.tos);
        }
        p.congestion = self.congestion.clone();
        p.title = self.title.clone();
        p.extra_data = self.extra_data.clone();
        if self.udp_counters_64bit {
            p.udp_counters_64bit = Some(1);
        }
        p.client_version = Some(format!("riperf3 {}", env!("CARGO_PKG_VERSION")));
        if let Some(bytes) = self.bytes_to_send {
            p.num = Some(bytes);
        }
        if let Some(blocks) = self.blocks_to_send {
            p.blockcount = Some(blocks);
        }

        // Auth: encrypt credentials if username and public key are set
        if let (Some(ref username), Some(ref pubkey_path)) =
            (&self.username, &self.rsa_public_key_path)
        {
            let pubkey_pem = std::fs::read(pubkey_path).unwrap_or_default();
            let password = self
                .password
                .clone()
                .or_else(|| crate::auth::read_password().ok())
                .unwrap_or_default();
            if let Ok(token) = crate::auth::encode_auth_token(
                username,
                &password,
                &pubkey_pem,
                self.use_pkcs1_padding,
            ) {
                p.authtoken = Some(token);
            }
        }

        p
    }

    async fn create_streams(
        &self,
        cookie: &[u8; protocol::COOKIE_SIZE],
        done: &Arc<AtomicBool>,
        start: &Arc<AtomicBool>,
        blksize: usize,
    ) -> Result<(Vec<DataStream>, Option<(Arc<AtomicI64>, i64)>)> {
        let mut streams = Vec::new();

        // In normal mode: client sends. Reverse: client receives. Bidir: both.
        let send_count = if self.reverse && !self.bidir {
            0
        } else {
            self.num_streams
        };
        let recv_count = if self.reverse || self.bidir {
            self.num_streams
        } else {
            0
        };
        let total = send_count + recv_count;

        // Max send duration the UDP senders self-enforce (issue #5): the
        // sender stops itself at `-t` so termination never depends on `done`
        // being set by a CPU-starved runtime. Only in duration mode;
        // byte/block-limited tests stop on `done`.
        let max_duration = (self.bytes_to_send.is_none() && self.blocks_to_send.is_none())
            .then(|| Duration::from_secs((self.duration + self.omit) as u64));

        // `-n`/`-k` shared byte budget for the sending streams: they collectively
        // stop at ~N bytes (iperf3's `-n` is the test-wide total), bounding the
        // overshoot to ~one block per stream. Only the TCP senders consume it
        // (UDP `-n` is left approximate), so build it only for a TCP run that has
        // senders. See `make_byte_budget` for the 0-is-unlimited / clamp rules.
        let byte_budget: Option<Arc<AtomicI64>> = (matches!(self.protocol, TransportProtocol::Tcp)
            && send_count > 0)
            .then(|| stream::make_byte_budget(self.bytes_to_send, self.blocks_to_send, blksize))
            .flatten();
        // -O + -n/-k (#31): the limit applies to the POST-omit window. The
        // budget holds gross N from the start — senders PAUSE at it (iperf3's
        // mt sender idles at the limit, including during warm-up, then
        // resumes when the boundary resets the counter) — and the REPORTER
        // refills it at its omit boundary, the same instant the byte
        // baselines snapshot, so limit and accounting can't skew (review r2).
        let budget_target = byte_budget.as_ref().map(|b| b.load(Ordering::Relaxed));

        match self.protocol {
            TransportProtocol::Tcp => {
                for i in 0..total {
                    let mut data_stream = net::tcp_connect(
                        &self.host,
                        self.port,
                        self.connect_timeout,
                        self.cport,
                        self.bind_address.as_deref(),
                        self.bind_dev.as_deref(),
                        self.mptcp,
                        self.ip_version,
                    )
                    .await?;
                    protocol::send_cookie(&mut data_stream, cookie).await?;
                    net::configure_tcp_stream_full(
                        &data_stream,
                        self.no_delay,
                        self.mss,
                        self.window,
                        self.congestion.as_deref(),
                    )?;
                    // Apply socket options (no-ops on non-Linux)
                    if self.dont_fragment {
                        net::set_dont_fragment(&data_stream)?;
                    }
                    if let Some(rate) = self.fq_rate {
                        net::set_fq_rate(&data_stream, rate)?;
                    }
                    if let Some(ms) = self.rcv_timeout {
                        net::set_rcv_timeout(&data_stream, ms)?;
                    }
                    if let Some(ms) = self.snd_timeout {
                        net::set_snd_timeout(&data_stream, ms)?;
                    }
                    if let Some(label) = self.flowlabel {
                        net::set_ipv6_flowlabel(&data_stream, label)?;
                    }
                    if self.tos != 0 {
                        net::set_tos(&data_stream, self.tos as u32)?;
                    }

                    // Extract raw fd for TCP_INFO (Unix only)
                    #[cfg(unix)]
                    let raw_fd = {
                        use std::os::unix::io::AsRawFd;
                        Some(data_stream.as_raw_fd())
                    };
                    #[cfg(not(unix))]
                    let raw_fd: Option<i32> = None;

                    // Capture the real socket addresses before the stream moves
                    // into its task, for the `-J` start.connected block (#36).
                    let local_addr = data_stream.local_addr().ok();
                    let peer_addr = data_stream.peer_addr().ok();
                    let sock = socket2::SockRef::from(&data_stream);
                    let sndbuf_actual = sock.send_buffer_size().ok().map(|v| v as u64);
                    let rcvbuf_actual = sock.recv_buffer_size().ok().map(|v| v as u64);
                    // #97: abort if the kernel clamped -w below the request, like
                    // iperf3 (IESETBUF2). Before the stream moves into its task.
                    net::check_socket_window(self.window, sndbuf_actual, rcvbuf_actual)?;
                    // #37: the congestion algorithm actually in effect (the kernel
                    // default when -C is unset), for `congestion_used`.
                    let congestion_used = net::tcp_congestion_used(&data_stream);

                    let stream_id = iperf3_stream_id(i);
                    let is_sender = i < send_count;
                    let counters = Arc::new(StreamCounters::new());
                    let fp = self.file.as_ref().map(std::path::PathBuf::from);

                    let task = if is_sender {
                        let buf = make_send_buffer(blksize, self.repeating_payload);
                        let c = counters.clone();
                        let d = done.clone();
                        let zc = self.zerocopy;
                        let rate = self.bandwidth;
                        let pt = self.pacing_timer;
                        let bu = self.burst;
                        let bb = byte_budget.clone();
                        tokio::spawn(async move {
                            // Zerocopy (sendfile) is used only for an unlimited,
                            // duration-based transfer; with `-b` (pacing) or
                            // `-n`/`-k` (byte budget) the copy sender runs instead,
                            // since the sendfile retry loop self-limits/paces
                            // neither cleanly (#102 + byte-limit overshoot fix).
                            if zc && rate == 0 && bb.is_none() {
                                // Zerocopy senders exist only for these targets
                                // (stream.rs). The gate must match the impls, not
                                // `unix`: other-Unix (NetBSD/OpenBSD/illumos) is
                                // `unix` with no zerocopy impl, so `#[cfg(unix)]`
                                // referenced a nonexistent fn and failed to
                                // compile there (#78). Elsewhere `-Z` cleanly
                                // falls back to the normal sender.
                                #[cfg(any(
                                    target_os = "linux",
                                    target_os = "macos",
                                    target_os = "freebsd"
                                ))]
                                {
                                    stream::run_tcp_sender_zerocopy(data_stream, c, buf, d).await
                                }
                                #[cfg(not(any(
                                    target_os = "linux",
                                    target_os = "macos",
                                    target_os = "freebsd"
                                )))]
                                {
                                    stream::run_tcp_sender(
                                        data_stream,
                                        c,
                                        buf,
                                        d,
                                        fp,
                                        rate,
                                        pt,
                                        bu,
                                        bb,
                                    )
                                    .await
                                }
                            } else {
                                stream::run_tcp_sender(data_stream, c, buf, d, fp, rate, pt, bu, bb)
                                    .await
                            }
                        })
                    } else {
                        let c = counters.clone();
                        let d = done.clone();
                        let bs = blksize;
                        let srxc = self.skip_rx_copy;
                        tokio::spawn(async move {
                            stream::run_tcp_receiver(data_stream, c, bs, d, srxc, fp).await
                        })
                    };

                    streams.push(DataStream {
                        id: stream_id,
                        is_sender,
                        counters,
                        udp_recv_stats: None,
                        task,
                        raw_fd,
                        local_addr,
                        peer_addr,
                        sndbuf_actual,
                        rcvbuf_actual,
                        congestion_used,
                    });
                }
            }
            TransportProtocol::Udp => {
                // Resolve once, honoring -4/-6, so the bind family matches the
                // peer and the connection respects the version preference (#10).
                let remote = net::resolve_host(&self.host, self.port, self.ip_version).await?;
                // Honor -B: resolve the UDP source address once (family-validated
                // against the target), then bind every stream's socket to it,
                // mirroring the TCP path (#15).
                let bind_ip = match self.bind_address.as_deref() {
                    Some(b) => Some(
                        net::resolve_bind_ip(b, remote.is_ipv6(), &self.host)
                            .await?
                            .to_string(),
                    ),
                    None => None,
                };
                // #178: every UDP stream gets a dedicated spawn_blocking OS
                // thread, spawned through the gate so the barrier below can
                // hold this side's test window until the data plane exists.
                let mut thread_gate = stream::StreamThreadGate::new();
                for i in 0..total {
                    let udp_sock = net::udp_bind(bind_ip.as_deref(), 0, remote.is_ipv6()).await?;
                    if let Some(ref dev) = self.bind_dev {
                        net::set_bind_dev(&udp_sock, dev, remote.is_ipv6())?;
                    }
                    udp_sock.connect(remote).await?;
                    protocol::udp_connect_client(&udp_sock).await?;

                    // GSO/GRO is deliberately best-effort (#45), matching
                    // iperf3 3.20+'s --gsro: its iperf_udp_gso/iperf_udp_gro
                    // disable the feature and continue when the setsockopt
                    // fails, so a kernel lacking UDP_SEGMENT/UDP_GRO degrades
                    // to plain sends rather than failing the test.
                    if self.gsro {
                        let _ = net::set_udp_gso(&udp_sock, blksize as u16);
                        let _ = net::set_udp_gro(&udp_sock);
                    }
                    if self.tos != 0 {
                        // Fatal like the TCP path (#45): iperf3's
                        // iperf_common_sockopts errors (IESETTOS) when IP_TOS
                        // can't be applied, on both roles and both protocols.
                        net::set_tos(&udp_sock, self.tos as u32)?;
                    }

                    let stream_id = iperf3_stream_id(i);
                    let is_sender = i < send_count;
                    let counters = Arc::new(StreamCounters::new());

                    // Capture real addresses for the `-J` start.connected block
                    // (#36) before the socket moves into its task.
                    let local_addr = udp_sock.local_addr().ok();
                    let peer_addr = udp_sock.peer_addr().ok();
                    let sock = socket2::SockRef::from(&udp_sock);
                    // Honor -w/--window on the UDP socket too (#59); iperf3 applies
                    // it to UDP via iperf_udp_buffercheck. Set before the read-back
                    // so sndbuf_actual/rcvbuf_actual report the realized size.
                    net::apply_socket_window(&sock, self.window);
                    let sndbuf_actual = sock.send_buffer_size().ok().map(|v| v as u64);
                    let rcvbuf_actual = sock.recv_buffer_size().ok().map(|v| v as u64);
                    // #97: abort if -w was clamped below the request (iperf3 IESETBUF2).
                    net::check_socket_window(self.window, sndbuf_actual, rcvbuf_actual)?;

                    // Convert tokio UdpSocket to std for blocking I/O
                    let std_sock = udp_sock.into_std().map_err(RiperfError::Io)?;

                    let task = if is_sender {
                        let c = counters.clone();
                        let d = done.clone();
                        let bs = blksize;
                        // Effective rate is resolved at build time (UDP unset →
                        // 1 Mbit/s); 0 means unlimited — no pacing (#17).
                        let rate = self.bandwidth;
                        // #185: honor --pacing-timer on the UDP send batch too,
                        // so a low -b over a large datagram paces smoothly.
                        let pt = self.pacing_timer;
                        let bu = self.burst;
                        let uw = self.window.is_some();
                        let u64bit = self.udp_counters_64bit;
                        let use_sendmmsg = self.sendmmsg;
                        let st = start.clone();
                        let md = max_duration;
                        thread_gate.spawn(move || {
                            if use_sendmmsg {
                                stream::run_udp_sender_sendmmsg(
                                    std_sock, c, bs, d, rate, pt, bu, uw, u64bit, st, md,
                                )
                            } else {
                                stream::run_udp_sender_blocking(
                                    std_sock, c, bs, d, rate, pt, bu, uw, u64bit, st, md,
                                )
                            }
                        })
                    } else {
                        let c = counters.clone();
                        let d = done.clone();
                        let bs = blksize;
                        let stats = Arc::new(Mutex::new(UdpRecvStats::new()));
                        let sc = stats.clone();
                        let u64bit = self.udp_counters_64bit;
                        let task = thread_gate.spawn(move || {
                            stream::run_udp_receiver_blocking(std_sock, c, sc, bs, d, u64bit)
                        });
                        streams.push(DataStream {
                            id: stream_id,
                            is_sender,
                            counters,
                            udp_recv_stats: Some(stats),
                            task,
                            raw_fd: None,
                            local_addr,
                            peer_addr,
                            sndbuf_actual,
                            rcvbuf_actual,
                            congestion_used: None,
                        });
                        continue;
                    };

                    streams.push(DataStream {
                        id: stream_id,
                        is_sender,
                        counters,
                        udp_recv_stats: None,
                        task,
                        raw_fd: None,
                        local_addr,
                        peer_addr,
                        sndbuf_actual,
                        rcvbuf_actual,
                        congestion_used: None,
                    });
                }
                // #178: hold CreateStreams until every data thread is running
                // (parked at its start gate). This gates the CLIENT's side
                // only — TestStart isn't read (so this side's clock doesn't
                // start and its senders stay parked) until the data plane
                // exists. The server sends TestStart on its own schedule once
                // the last UDP handshake arrives; the wire protocol has no
                // post-handshake signal to hold it back, so a *cross-host*
                // stall confined to the client can still cost the start of
                // the window (bounded by SO_RCVBUF for receivers — and iperf3
                // has the identical exposure). Same-host, both gates release
                // together. On timeout proceed anyway (degraded = pre-fix
                // behavior).
                thread_gate.wait(stream::STREAM_THREAD_START_TIMEOUT).await;
            }
        }

        Ok((streams, byte_budget.zip(budget_target)))
    }

    /// Wait out a `-n`/`-k` run (#31): iperf3 gates the end-condition check
    /// on !omitting, so the warm-up never satisfies the limit. The budget
    /// refill happens in the REPORTER's boundary block (same instant as the
    /// byte baselines); this poll only waits out the warm-up and then watches
    /// the net (post-omit) progress. The returned summary window excludes the
    /// warm-up.
    async fn wait_byte_limit(
        &self,
        streams: &[DataStream],
        target: u64,
        report_start: &std::time::Instant,
        boundary: Option<&Arc<crate::reporter::OmitBoundary>>,
    ) -> f64 {
        // With -O the limit applies from the boundary on — iperf3 gates its
        // end check on `!test->omitting`. Wait on the reporter's boundary
        // signal, not a parallel wall clock: the wall gate provably opened
        // before the boundary's re-baselining and read gross-as-net (review
        // r3, race C). The fallback bounds the wait for liveness if the
        // reporter died before its boundary fired.
        if let Some(b) = boundary {
            let fallback = Duration::from_secs(self.omit as u64 + 2);
            b.crossed(fallback).await;
        }
        // First check BEFORE any sleep: a warm-up that already covered the
        // gross receive target ends the test AT the boundary, like iperf3
        // (its select loop re-checks per wake, stopping within ~1 ms of the
        // omit flip — a 100 ms first poll would leak a poll's worth of line
        // rate into the post-omit window).
        loop {
            if transferred_bytes(streams) >= target {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        (report_start.elapsed().as_secs_f64() - self.omit as f64).max(0.0)
    }

    #[allow(clippy::too_many_arguments)] // test-drive knobs, 1:1 with run()'s state
    /// Returns (authoritative end seconds, server_terminated).
    async fn run_test(
        &self,
        ctrl: &mut TcpStream,
        streams: &[DataStream],
        done: &Arc<AtomicBool>,
        blksize: usize,
        collector: Arc<Mutex<crate::reporter::CollectedIntervals>>,
        byte_budget: Option<&(Arc<AtomicI64>, i64)>,
        interrupt: &mut Option<tokio::sync::watch::Receiver<Option<String>>>,
    ) -> Result<(f64, Option<ControlEvent>)> {
        // Run the interval reporter whenever intervals are enabled. It prints
        // live for text / json-stream; for plain -J it runs silently to collect
        // intervals for the final blob (#36 PR2).
        let interval_secs = self.interval.unwrap_or(1.0);
        let print_intervals = !self.json_output || self.json_stream;
        let collect_intervals = self.json_output && !self.json_stream;
        // The reporter needs the collector whenever we emit JSON: `-J` collects the
        // typed intervals for the final blob; `--json-stream` streams them live but
        // still needs the per-stream TCP_INFO extremes (max cwnd/rtt) handed back
        // for the `end` event (#62).
        let want_collector = collect_intervals || self.json_stream;
        // Clock origin shared with the reporter: `report_start` is captured right
        // before spawning it, so `report_start.elapsed()` at end-of-test is the
        // authoritative final-interval boundary (#55).
        let reporter_end = Arc::new(crate::reporter::ReporterEnd::new());
        // -O + -n/-k: the reporter signals here when its omit boundary has
        // fully crossed (baselines snapshotted, budget refilled), gating the
        // byte-limit driver's first end check (#31, review r3).
        let omit_boundary = (self.omit > 0).then(|| Arc::new(crate::reporter::OmitBoundary::new()));
        let report_start = std::time::Instant::now();
        // `>= 0.0`: `-i 0` still spawns the reporter, which emits a single
        // whole-test interval rather than none (#107).
        let interval_handle = if interval_secs >= 0.0 {
            let stream_refs: Vec<_> = streams
                .iter()
                .map(|s| crate::reporter::IntervalStreamRef {
                    id: s.id,
                    is_sender: s.is_sender,
                    counters: s.counters.clone(),
                    udp_recv_stats: s.udp_recv_stats.clone(),
                    raw_fd: s.raw_fd,
                })
                .collect();
            crate::reporter::spawn_interval_reporter(
                crate::reporter::IntervalReporterConfig {
                    interval_secs,
                    protocol: self.protocol,
                    format_char: self.format_char,
                    omit_secs: self.omit,
                    forceflush: self.forceflush,
                    json_stream: self.json_stream,
                    print: print_intervals,
                    blksize,
                    // The client keeps intervals only under
                    // --json-stream-full-output (iperf3's discard_json,
                    // second leg) (#213).
                    keep_intervals: self.json_stream_full_output,
                    bidir: self.bidir,
                    is_server: false,
                },
                stream_refs,
                done.clone(),
                reporter_end.clone(),
                want_collector.then(|| collector.clone()),
                byte_budget.cloned(),
                omit_boundary.clone(),
            )
        } else {
            None
        };

        // Determine test end condition
        let end_condition = if let Some(bytes) = self.bytes_to_send {
            EndCondition::Bytes(bytes)
        } else if let Some(blocks) = self.blocks_to_send {
            EndCondition::Blocks(blocks)
        } else {
            EndCondition::Duration(Duration::from_secs(self.duration as u64))
        };

        // The authoritative end time for the reporter's final interval (#55). A
        // duration run ends at exactly `-t`, so pass that value (not the measured
        // elapsed, which trails the deadline by a variable scheduling slack and
        // would smear a boundary-aligned end into a spurious sliver). A
        // byte/block run ends at an arbitrary instant, so use the measured
        // elapsed.
        // Every end-condition mode races the control watch (#170): iperf3's
        // client main loop is one select() over the control AND data fds, so
        // control-channel events are observed mid-transfer — previously the
        // -n/-k modes had no watch at all, control death in duration mode
        // "completed" the test, and any stray state byte truncated the wait.
        let mut control_event: Option<ControlEvent> = None;
        // Watch-arm end time (#170 review r1 n2): the reporter timeline is
        // post-omit-rebased (iperf3 restamps start_time at the boundary), so
        // an event past the warm-up reports `elapsed - omit`; during the
        // warm-up the raw elapsed matches iperf3's un-restamped clock.
        let omit_secs = self.omit as f64;
        let watch_end_secs = move |raw: f64| {
            if raw > omit_secs {
                raw - omit_secs
            } else {
                raw
            }
        };
        let end_secs = match end_condition {
            EndCondition::Duration(dur) => {
                // The UDP senders also enforce this deadline themselves inside
                // their loop (see the `deadline` passed at stream creation):
                // at a high `-b` the CPU-bound senders can saturate every core
                // and starve this async timer, so they must not depend on it to
                // stop (issue #5). Once they self-terminate, CPU frees and this
                // timer fires normally to drive the rest of the shutdown.
                // The wall clock runs omit + time (#31): iperf3 extends the
                // run by the warm-up so the measured window is a full `-t`.
                // The authoritative end time handed to the reporter stays the
                // post-omit `-t` (its timeline restarts at the boundary).
                let wall = dur + Duration::from_secs(self.omit as u64);
                tokio::select! {
                    _ = tokio::time::sleep(wall) => dur.as_secs_f64(),
                    ev = watch_control(ctrl) => {
                        control_event = Some(ev);
                        watch_end_secs(report_start.elapsed().as_secs_f64())
                    }
                    msg = wait_interrupt(interrupt.as_mut()) => {
                        control_event = Some(ControlEvent::Interrupted(msg));
                        watch_end_secs(report_start.elapsed().as_secs_f64())
                    }
                }
            }
            EndCondition::Bytes(target) => {
                tokio::select! {
                    secs = self.wait_byte_limit(streams, target, &report_start, omit_boundary.as_ref()) => secs,
                    ev = watch_control(ctrl) => {
                        control_event = Some(ev);
                        watch_end_secs(report_start.elapsed().as_secs_f64())
                    }
                    msg = wait_interrupt(interrupt.as_mut()) => {
                        control_event = Some(ControlEvent::Interrupted(msg));
                        watch_end_secs(report_start.elapsed().as_secs_f64())
                    }
                }
            }
            EndCondition::Blocks(target) => {
                // Block-based: approximate by dividing transferred bytes by blksize.
                tokio::select! {
                    secs = self.wait_byte_limit(
                        streams,
                        target.saturating_mul(blksize as u64),
                        &report_start,
                        omit_boundary.as_ref(),
                    ) => secs,
                    ev = watch_control(ctrl) => {
                        control_event = Some(ev);
                        watch_end_secs(report_start.elapsed().as_secs_f64())
                    }
                    msg = wait_interrupt(interrupt.as_mut()) => {
                        control_event = Some(ControlEvent::Interrupted(msg));
                        watch_end_secs(report_start.elapsed().as_secs_f64())
                    }
                }
            }
        };

        // End of test: hand the reporter the authoritative end time, then stop
        // the senders immediately (`done`) so no bytes leak past the deadline into
        // the final interval or the summary (#55). The reporter prioritises this
        // `finish` over `done` (see its select), so the final interval still
        // flushes; we then wait for it before tearing the streams down.
        // #159: stop the senders FIRST and give their in-flight catch-up the
        // teardown grace to land in the counters, THEN signal the flush —
        // iperf3 reads its counters after the threads join, so the intervals
        // always cover what the END block accounts. The [last_boundary,
        // end_secs] window stays authoritative (#55) — late-landing bytes
        // belong to the window they were sent in.
        done.store(true, Ordering::Relaxed);
        tokio::time::sleep(Duration::from_millis(100)).await;
        reporter_end.finish(end_secs);
        if let Some(handle) = interval_handle {
            let _ = handle.await;
        }

        // The watch outcomes surface only AFTER the cleanup above — the #147
        // class (an early return leaked the reporter into a library
        // consumer's runtime) must not regrow here.
        match control_event {
            Some(ControlEvent::Closed) => {
                // iperf3 prints no summary on IECTRLCLOSE.
                return Err(RiperfError::ControlSocketClosed);
            }
            // The caller renders the partial summary: Terminated errors with
            // IESERVERTERM (iperf3 flips to DISPLAY_RESULTS first);
            // Interrupted (#210) additionally sends CLIENT_TERMINATE and
            // returns normally (iperf3's signal-normal exit).
            Some(ev) => return Ok((end_secs, Some(ev))),
            None => {}
        }

        // The authoritative test duration: exactly `-t` for a duration run, the
        // measured elapsed for a byte/block-limited run. The summary window and
        // its derived bitrate use this, not the default `-t` (#103).
        Ok((end_secs, None))
    }

    fn build_results(
        &self,
        streams: &[DataStream],
        cpu_start: Option<&CpuSnapshot>,
        blksize: usize,
        test_duration: f64,
    ) -> TestResultsJson {
        let cpu_end = CpuSnapshot::now();
        let cpu_util = cpu_start
            .map(|start| cpu_end.utilization_since(start))
            .unwrap_or_default();

        let stream_results: Vec<_> = streams
            .iter()
            .map(|s| {
                // Net (post-omit) bytes; packets/errors stay GROSS with the
                // omitted_* baselines alongside — the reading side subtracts,
                // exactly iperf3's exchange accounting (#31).
                let bytes = if s.is_sender {
                    s.counters.bytes_sent_net()
                } else {
                    s.counters.bytes_received_net()
                };

                let (jitter, errors, packets, omitted_errors, omitted_packets) =
                    if let Some(ref udp_stats) = s.udp_recv_stats {
                        udp_stats
                            .lock()
                            .map(|st| {
                                (
                                    st.jitter,
                                    st.cnt_error,
                                    st.packet_count,
                                    st.omitted_cnt_error,
                                    st.omitted_packet_count,
                                )
                            })
                            .unwrap_or((0.0, 0, 0, 0, 0))
                    } else if s.is_sender && self.protocol == TransportProtocol::Udp {
                        // iperf3's UDP sender counts every datagram it sends
                        // (iperf_udp.c `++sp->packet_count`) and exchanges
                        // that count unconditionally (iperf_api.c
                        // `"packets"`). Fill the equivalent from sent bytes,
                        // keeping the gross+baseline convention (#184).
                        let blk = blksize.max(1) as u64;
                        let gross = (s.counters.bytes_sent() / blk) as i64;
                        let net = (bytes / blk) as i64;
                        (0.0, 0, gross, 0, gross - net)
                    } else {
                        (0.0, 0, 0, 0, 0)
                    };
                let is_udp_stream = self.protocol == TransportProtocol::Udp;
                // #156 sentinel: -1 = "no retransmit total" (receiver/UDP/no
                // TCP_INFO); the wire carries it, the peer renders it (#171
                // omit-adjustment and the fd fallback live in the method).
                let retransmits = s.sender_retransmits(is_udp_stream).unwrap_or(-1);

                protocol::StreamResultJson {
                    id: s.id,
                    bytes,
                    retransmits,
                    jitter,
                    errors,
                    omitted_errors,
                    packets,
                    omitted_packets,
                    start_time: 0.0,
                    end_time: test_duration,
                }
            })
            .collect();

        TestResultsJson {
            // Client → server payload never carries server output (#33).
            server_output_text: None,
            server_output_json: None,
            cpu_util_total: cpu_util.host_total,
            cpu_util_user: cpu_util.host_user,
            cpu_util_system: cpu_util.host_system,
            // #156: iperf3 sends 1 when this side is a retransmit-capable TCP
            // sender (check_sender_has_retransmits) — the PEER gates display
            // of our Retr column on it; 0 suppressed it cross-tool even where
            // riperf3 measures retransmits.
            sender_has_retransmits: if streams.iter().any(|s| s.is_sender) {
                i64::from(
                    self.protocol == TransportProtocol::Tcp
                        && crate::tcp_info::has_retransmit_info(),
                )
            } else {
                -1
            },
            // #37: the congestion algorithm actually in effect (read back at stream
            // creation); None for UDP / unsupported platforms.
            congestion_used: streams.first().and_then(|s| s.congestion_used.clone()),
            streams: stream_results,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn print_results(
        &self,
        streams: &[DataStream],
        cpu_start: Option<&CpuSnapshot>,
        remote_cpu: Option<&TestResultsJson>,
        blksize: usize,
        interval_data: &Arc<Mutex<crate::reporter::CollectedIntervals>>,
        start_meta: &StartMeta,
        test_duration: f64,
        error: Option<&str>,
    ) {
        // #220: stream mode WINS when both flags are set — iperf3's
        // OPT_JSON_STREAM implies -J (iperf_api.c:1280-1282), so `-J
        // --json-stream` IS stream mode (full event stream incl. `end`; the
        // monolithic doc only under --json-stream-full-output, which the
        // stream arm already honors). The old json_output-first dispatch
        // emitted a truncated stream (no end event) followed by the doc.
        // The CLI's error-sink dispatch has always been stream-first (#198).
        if self.json_stream {
            // iperf3's NDJSON tail order is: error?, server_output_json,
            // server_output_text, end (iperf_api.c:5310-5323) (#170 + #168).
            if let Some(e) = error {
                crate::reporter::emit_json_stream_line(&crate::json_report::json_stream_event(
                    "error", &e,
                ));
            }
            if self.get_server_output {
                if let Some(server) = remote_cpu {
                    // Through the shared envelope helper — a hand-built
                    // serde_json::json! map serializes alphabetically
                    // ("data" before "event"), breaking the {"event":..,
                    // "data":..} contract every other event keeps (#168 r1 n1).
                    if let Some(json) = &server.server_output_json {
                        crate::reporter::emit_json_stream_line(
                            &crate::json_report::json_stream_event("server_output_json", json),
                        );
                    }
                    if let Some(text) = &server.server_output_text {
                        crate::reporter::emit_json_stream_line(
                            &crate::json_report::json_stream_event("server_output_text", text),
                        );
                    }
                }
            }
            // --json-stream: emit the `end` event. (Previously this fell through
            // to print_results_text, printing text banners into the NDJSON — #62.)
            self.emit_json_stream_end(
                streams,
                cpu_start,
                remote_cpu,
                blksize,
                interval_data,
                start_meta,
                test_duration,
            );
        } else if self.json_output {
            self.print_results_json(
                streams,
                cpu_start,
                remote_cpu,
                blksize,
                interval_data,
                start_meta,
                test_duration,
                error,
            );
        } else {
            self.print_results_text(streams, remote_cpu, blksize, test_duration);
        }
    }

    /// `--get-server-output` (#33): print the server's returned output after
    /// our own report, like iperf3 — "Server output:" for text, "Server JSON
    /// output:" for a -J server's report (iperf_api.c); the text block ends
    /// with a blank line, the JSON block with a single newline, matching
    /// iperf3's format strings. Only consulted when WE requested it, like
    /// test->get_server_output gate (a misbehaving server can't inject).
    fn print_server_output(&self, server_results: Option<&TestResultsJson>) {
        if !self.get_server_output {
            return;
        }
        let Some(server) = server_results else { return };
        if let Some(text) = &server.server_output_text {
            crate::vprintln!("\nServer output:");
            println!("{text}");
        } else if let Some(json) = &server.server_output_json {
            crate::vprintln!("\nServer JSON output:");
            if let Ok(s) = serde_json::to_string_pretty(json) {
                println!("{s}");
            }
        }
    }

    fn print_results_text(
        &self,
        streams: &[DataStream],
        server_results: Option<&TestResultsJson>,
        blksize: usize,
        test_duration: f64,
    ) {
        crate::reporter::print_separator();

        let is_udp = matches!(self.protocol, TransportProtocol::Udp);
        // iperf3's end block pairs BOTH halves of every stream — a `sender`
        // line and a `receiver` line — in every mode (#184): the local half
        // from our counters/stats, the peer half from the results the server
        // returned (#25 generalized; pre-#184 only forward runs got the peer
        // line, so reverse lacked its sender line and bidir paired nothing).
        // Sender lines carry `0.000 ms 0/<sent>` like iperf3 — the sent count
        // is bytes/blksize locally, or the peer's reported packets.
        let mut summaries: Vec<crate::reporter::StreamSummary> = Vec::new();
        for s in streams {
            // Bidir tags every line with the STREAM's direction (#184).
            let role_tag = self
                .bidir
                .then_some(crate::reporter::bidir_role_tag(false, s.is_sender));
            let bytes = if s.is_sender {
                s.counters.bytes_sent_net()
            } else {
                s.counters.bytes_received_net()
            };

            let (jitter, lost, total) = if let Some(ref udp_stats) = s.udp_recv_stats {
                udp_stats
                    .lock()
                    .map(|st| {
                        // Post-omit stats (#31): gross minus baselines.
                        (
                            Some(st.jitter),
                            Some(st.cnt_error - st.omitted_cnt_error),
                            Some(st.packet_count - st.omitted_packet_count),
                        )
                    })
                    .unwrap_or((None, None, None))
            } else if is_udp {
                // Local sending stream: iperf3's sender line shows zero
                // jitter/loss over the sent datagram count.
                (
                    Some(0.0),
                    Some(0),
                    Some((bytes / blksize.max(1) as u64) as i64),
                )
            } else {
                (None, None, None)
            };

            let local = crate::reporter::StreamSummary {
                stream_id: s.id,
                start: 0.0,
                end: test_duration,
                bytes,
                is_sender: s.is_sender,
                // TCP sender lines carry the omit-adjusted retransmit total
                // iperf3 prints (#184); receivers/UDP carry none.
                retransmits: s.sender_retransmits(is_udp),
                jitter,
                lost,
                total_packets: total,
                role_tag,
            };
            // The peer half — the opposite role of the same stream — from the
            // server's per-stream results entry. Tolerant of a missing entry
            // (an odd peer): the pair just collapses to the local line. The
            // peer's sender line shows its retransmits only when the peer
            // reported having them (#156 sender_has_retransmits).
            let peer_has_retr = server_results.is_some_and(|r| r.sender_has_retransmits == 1);
            let peer = server_results
                .and_then(|r| r.streams.iter().find(|x| x.id == s.id))
                .map(|x| {
                    peer_half_summary(
                        x,
                        s.is_sender,
                        is_udp,
                        peer_has_retr,
                        test_duration,
                        role_tag,
                    )
                });

            // Terminated mid-test (#170): the peer half never arrived.
            // iperf3 still prints BOTH halves with the missing one ZEROED
            // (live-captured: `0.00 Bytes 0.00 bits/sec receiver`), so
            // synthesize a zeroed opposite-role half rather than collapsing
            // the pair — a lone entry only remains for an odd peer that
            // exchanged results but skipped this stream id.
            let peer = peer.or_else(|| {
                server_results
                    .is_none()
                    .then(|| crate::reporter::StreamSummary {
                        stream_id: s.id,
                        start: 0.0,
                        end: test_duration,
                        bytes: 0,
                        is_sender: !s.is_sender,
                        retransmits: None,
                        jitter: is_udp.then_some(0.0),
                        lost: is_udp.then_some(0),
                        total_packets: is_udp.then_some(0),
                        role_tag,
                    })
            });

            // iperf3 orders each pair sender-first.
            match peer {
                Some(peer) if s.is_sender => summaries.extend([local, peer]),
                Some(peer) => summaries.extend([peer, local]),
                None => summaries.push(local),
            }
        }

        // iperf3 reprints the column header above the final summaries, with
        // the Retr column only when a line actually carries a retransmit total.
        let with_retr = summaries.iter().any(|s| s.retransmits.is_some());
        crate::reporter::print_final_header(self.protocol, self.bidir, with_retr);
        // Per-stream lines plus aggregate [SUM] row(s) for parallel streams
        // (issue #4), via the shared path the server also uses.
        crate::reporter::print_final_summaries(&summaries, self.format_char);
        self.print_server_output(server_results);
    }

    /// Assemble the typed iperf3-schema report input from the finished test.
    /// Shared by `-J` (build + pretty-print) and `--json-stream` (build + emit the
    /// `end` event; and at TestStart, the `start` event from a partial input where
    /// only the start fields are meaningful — see `emit_json_stream_start`).
    #[allow(clippy::too_many_arguments)]
    fn build_report_input(
        &self,
        streams: &[DataStream],
        cpu_start: Option<&CpuSnapshot>,
        remote_cpu: Option<&TestResultsJson>,
        blksize: usize,
        interval_data: &Arc<Mutex<crate::reporter::CollectedIntervals>>,
        start_meta: &StartMeta,
        test_duration: f64,
    ) -> crate::json_report::ReportInput {
        use crate::json_report::{
            CpuUtilization, ReportInput, StreamReport, TcpEndExtras, UdpStreamStats,
        };

        // Take the interval samples + per-stream extremes the reporter collected.
        // Its task has been joined by now (run_test awaits it), so this is final.
        let (collected_intervals, extremes) = match interval_data.lock() {
            Ok(mut g) => (
                std::mem::take(&mut g.intervals),
                std::mem::take(&mut g.extremes),
            ),
            Err(_) => (Vec::new(), Vec::new()),
        };

        let cpu_end = CpuSnapshot::now();
        let cpu_util = cpu_start
            .map(|start| cpu_end.utilization_since(start))
            .unwrap_or_default();
        let (remote_total, remote_user, remote_system) = remote_cpu
            .map(|r| (r.cpu_util_total, r.cpu_util_user, r.cpu_util_system))
            .unwrap_or((0.0, 0.0, 0.0));

        let is_udp = matches!(self.protocol, TransportProtocol::Udp);

        let stream_reports: Vec<StreamReport> = streams
            .iter()
            .map(|s| {
                let local_bytes = if s.is_sender {
                    s.counters.bytes_sent_net()
                } else {
                    s.counters.bytes_received_net()
                };
                // The peer's per-stream result is the opposite side of this stream.
                let server_stream =
                    remote_cpu.and_then(|r| r.streams.iter().find(|x| x.id == s.id));

                // UDP datagram stats: from our local receiver if we measured
                // them, else (any UDP sending stream) from the server's
                // results for this stream (#25, #182).
                let udp = if let Some(ref lock) = s.udp_recv_stats {
                    // Local receiver: post-omit stats (#31) — gross counters
                    // minus the boundary baselines.
                    lock.lock().ok().map(|st| UdpStreamStats {
                        jitter_secs: st.jitter,
                        lost_packets: st.cnt_error - st.omitted_cnt_error,
                        packets: st.packet_count - st.omitted_packet_count,
                        out_of_order: st.outoforder_packets - st.omitted_outoforder_packets,
                    })
                } else if is_udp && s.is_sender {
                    // A sending stream's datagram stats are measured at the
                    // peer's receiver and live only in the results it returned
                    // — attach them to the sender entry, in bidir exactly as
                    // in forward mode (#25, #182; iperf3 does the same).
                    // Peer's gross counts minus its omitted_* baselines (#31).
                    server_stream.map(|x| UdpStreamStats {
                        jitter_secs: x.jitter,
                        lost_packets: x.errors - x.omitted_errors,
                        packets: x.packets - x.omitted_packets,
                        out_of_order: 0,
                    })
                } else {
                    None
                };

                // to_canonical(): unwrap an IPv4-mapped IPv6 address to plain IPv4
                // (matches iperf3); a no-op for the client's usual canonical
                // addresses, correct if the client is bound to a dual-stack socket.
                let (local_host, local_port) = s
                    .local_addr
                    .map(|a| (a.ip().to_canonical().to_string(), a.port()))
                    .unwrap_or_else(|| (self.host.clone(), 0));
                let (remote_host, remote_port) = s
                    .peer_addr
                    .map(|a| (a.ip().to_canonical().to_string(), a.port()))
                    .unwrap_or_else(|| (self.host.clone(), self.port));

                // Sender-side TCP_INFO extremes + real retransmit total collected
                // across intervals (#36 PR2); only present for streams we sent.
                let ext = extremes
                    .iter()
                    .find(|e| e.stream_id == s.id && e.has_samples());
                let tcp_end = ext.map(|e| TcpEndExtras {
                    max_snd_cwnd: e.max_snd_cwnd,
                    max_snd_wnd: e.max_snd_wnd,
                    max_rtt: e.max_rtt,
                    min_rtt: e.min_rtt,
                    mean_rtt: e.mean_rtt(),
                    reorder: e.reorder,
                });
                let retransmits = if is_udp {
                    None
                } else {
                    // Real cumulative total when TCP_INFO gave us one (forward
                    // sender). Otherwise (reverse, or a stream we didn't send) use
                    // iperf3's defaults: 0 on a platform that supports retransmit
                    // info, -1 where it doesn't.
                    ext.and_then(|e| e.total_retransmits)
                        .map(|r| r as i64)
                        .or(Some(if crate::tcp_info::has_retransmit_info() {
                            0
                        } else {
                            -1
                        }))
                };

                StreamReport {
                    id: s.id,
                    local_host,
                    local_port,
                    remote_host,
                    remote_port,
                    is_sender: s.is_sender,
                    local_bytes,
                    remote_bytes: server_stream.map(|x| x.bytes),
                    retransmits,
                    tcp_end,
                    udp,
                }
            })
            .collect();

        let input = ReportInput {
            error: None,
            protocol: self.protocol,
            reverse: self.reverse,
            bidir: self.bidir,
            duration: self.duration as f64,
            elapsed: test_duration,
            num_streams: self.num_streams as i32,
            blksize: blksize as i64,
            omit: self.omit as i32,
            tos: self.tos,
            target_bitrate: self.bandwidth,
            bytes: self.bytes_to_send.unwrap_or(0),
            blocks: self.blocks_to_send.unwrap_or(0),
            connecting_host: self.host.clone(),
            connecting_port: self.port,
            is_server: false,
            accepted_host: String::new(),
            accepted_port: 0,
            version: format!("riperf3 {}", env!("CARGO_PKG_VERSION")),
            system_info: system_info(),
            cpu: CpuUtilization {
                host_total: cpu_util.host_total,
                host_user: cpu_util.host_user,
                host_system: cpu_util.host_system,
                remote_total,
                remote_user,
                remote_system,
            },
            // #37: the congestion algorithm actually in effect on the data socket,
            // read back via getsockopt(TCP_CONGESTION) at stream creation (the
            // kernel default when -C is unset). None for UDP / unsupported platforms.
            congestion_used: streams.first().and_then(|s| s.congestion_used.clone()),
            cookie: start_meta.cookie.clone(),
            tcp_mss_default: start_meta.tcp_mss_default,
            // -M/--set-mss request: emitted as start.tcp_mss (TCP only), which
            // suppresses tcp_mss_default. build() does the TCP/UDP gating.
            mss: self.mss.filter(|&m| m > 0).map(|m| m as u32),
            fq_rate: self.fq_rate.unwrap_or(0),
            // iperf3's start.sock_bufsize is the requested -w value (0 if unset);
            // sndbuf/rcvbuf_actual are the kernel's actual sizes on a data socket.
            // .max(0): the public builder accepts an i32 window; clamp so a
            // negative can't wrap to a huge u64 (the CLI path is already >= 0).
            sock_bufsize: self.window.map(|w| w.max(0) as u64).unwrap_or(0),
            sndbuf_actual: streams.first().and_then(|s| s.sndbuf_actual).unwrap_or(0),
            rcvbuf_actual: streams.first().and_then(|s| s.rcvbuf_actual).unwrap_or(0),
            interval: self.interval.unwrap_or(1.0),
            // riperf3's single --gsro flag drives both GSO and GRO.
            gso: i32::from(self.gsro),
            gro: i32::from(self.gsro),
            start_time_millis: start_meta.start_time_millis,
            extra_data: self.extra_data.clone(),
            // --get-server-output (#33): the server's returned output rides
            // the -J report tail — only when WE requested it (iperf3 gates on
            // test->get_server_output; an unrequested attachment is ignored).
            server_output_text: self
                .get_server_output
                .then(|| remote_cpu.and_then(|r| r.server_output_text.clone()))
                .flatten(),
            server_output_json: self
                .get_server_output
                .then(|| remote_cpu.and_then(|r| r.server_output_json.clone()))
                .flatten(),
            intervals: collected_intervals,
            streams: stream_reports,
        };

        input
    }

    /// `-J`: build and pretty-print the single batched report blob.
    #[allow(clippy::too_many_arguments)]
    fn print_results_json(
        &self,
        streams: &[DataStream],
        cpu_start: Option<&CpuSnapshot>,
        remote_cpu: Option<&TestResultsJson>,
        blksize: usize,
        interval_data: &Arc<Mutex<crate::reporter::CollectedIntervals>>,
        start_meta: &StartMeta,
        test_duration: f64,
        error: Option<&str>,
    ) {
        let mut input = self.build_report_input(
            streams,
            cpu_start,
            remote_cpu,
            blksize,
            interval_data,
            start_meta,
            test_duration,
        );
        // iperf3's iperf_json_finish attaches the run error to the blob (#170).
        input.error = error.map(str::to_owned);
        println!("{}", serde_json::to_string_pretty(&input.build()).unwrap());
    }

    /// `--json-stream`: emit the `start` event (#62). Called at TestStart, before
    /// any interval event. Only the `start` block is meaningful at this point; the
    /// rest of the report input is placeholder (no bytes/cpu/intervals collected
    /// yet) and is discarded.
    fn emit_json_stream_start(
        &self,
        streams: &[DataStream],
        cpu_start: Option<&CpuSnapshot>,
        blksize: usize,
        interval_data: &Arc<Mutex<crate::reporter::CollectedIntervals>>,
        start_meta: &StartMeta,
    ) {
        // The `start` event carries no summary window, so the elapsed value is
        // unused here; pass the nominal duration.
        let input = self.build_report_input(
            streams,
            cpu_start,
            None,
            blksize,
            interval_data,
            start_meta,
            self.duration as f64,
        );
        crate::reporter::emit_json_stream_line(&crate::json_report::json_stream_event(
            "start",
            &input.build().start,
        ));
    }

    /// `--json-stream`: emit the `end` event (#62) at DisplayResults. The interval
    /// events were already streamed live by the reporter.
    #[allow(clippy::too_many_arguments)]
    fn emit_json_stream_end(
        &self,
        streams: &[DataStream],
        cpu_start: Option<&CpuSnapshot>,
        remote_cpu: Option<&TestResultsJson>,
        blksize: usize,
        interval_data: &Arc<Mutex<crate::reporter::CollectedIntervals>>,
        start_meta: &StartMeta,
        test_duration: f64,
    ) {
        let input = self.build_report_input(
            streams,
            cpu_start,
            remote_cpu,
            blksize,
            interval_data,
            start_meta,
            test_duration,
        );
        let report = input.build();
        crate::reporter::emit_json_stream_line(&crate::json_report::json_stream_event(
            "end",
            &report.end,
        ));
        // --json-stream-full-output: the complete monolithic document also
        // prints after the stream, like iperf_json_finish keeping
        // print_full_json under the flag (iperf_api.c:5323) (#213).
        if self.json_stream_full_output {
            println!("{}", serde_json::to_string_pretty(&report).unwrap());
        }
    }
}

enum EndCondition {
    Duration(Duration),
    Bytes(u64),
    Blocks(u64),
}

/// Start-of-test metadata for the `-J` `start` block (#36 PR3), captured in
/// `run()` where the cookie / control-MSS / start wall-clock are known.
struct StartMeta {
    cookie: String,
    tcp_mss_default: u32,
    start_time_millis: u64,
}

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

pub struct ClientBuilder {
    host: Option<String>,
    port: Option<u16>,
    protocol: TransportProtocol,
    duration: u32,
    num_streams: u32,
    blksize: Option<usize>,
    reverse: bool,
    bidir: bool,
    omit: u32,
    no_delay: bool,
    mss: Option<i32>,
    window: Option<i32>,
    bandwidth: Option<u64>,
    burst: u32,
    pacing_timer: u32,
    tos: i32,
    congestion: Option<String>,
    udp_counters_64bit: bool,
    connect_timeout: Option<Duration>,
    title: Option<String>,
    extra_data: Option<String>,
    verbose: bool,
    json_output: bool,
    json_stream: bool,
    interrupt: Option<InterruptWatch>,
    json_stream_full_output: bool,
    bytes_to_send: Option<u64>,
    blocks_to_send: Option<u64>,
    repeating_payload: bool,
    zerocopy: bool,
    gsro: bool,
    sendmmsg: bool,
    dont_fragment: bool,
    cport: Option<u16>,
    get_server_output: bool,
    forceflush: bool,
    timestamps: Option<String>,
    bind_address: Option<String>,
    bind_dev: Option<String>,
    fq_rate: Option<u64>,
    flowlabel: Option<i32>,
    ip_version: Option<u8>,
    mptcp: bool,
    skip_rx_copy: bool,
    rcv_timeout: Option<u64>,
    snd_timeout: Option<u64>,
    file: Option<String>,
    dscp: Option<String>,
    format_char: char,
    interval: Option<f64>,
    cntl_ka: Option<String>,
    username: Option<String>,
    password: Option<String>,
    rsa_public_key_path: Option<String>,
    use_pkcs1_padding: bool,
}

impl Default for ClientBuilder {
    fn default() -> Self {
        Self {
            host: None,
            port: Some(DEFAULT_PORT),
            protocol: TransportProtocol::Tcp,
            duration: DEFAULT_DURATION,
            num_streams: DEFAULT_NUM_STREAMS,
            blksize: None,
            reverse: false,
            bidir: false,
            omit: DEFAULT_OMIT,
            no_delay: false,
            mss: None,
            window: None,
            bandwidth: None,
            burst: 0,
            pacing_timer: 0,
            tos: 0,
            congestion: None,
            udp_counters_64bit: false,
            connect_timeout: None,
            title: None,
            extra_data: None,
            verbose: false,
            json_output: false,
            json_stream: false,
            interrupt: None,
            json_stream_full_output: false,
            bytes_to_send: None,
            blocks_to_send: None,
            repeating_payload: false,
            zerocopy: false,
            gsro: false,
            sendmmsg: false,
            dont_fragment: false,
            cport: None,
            get_server_output: false,
            forceflush: false,
            timestamps: None,
            bind_address: None,
            bind_dev: None,
            fq_rate: None,
            flowlabel: None,
            ip_version: None,
            mptcp: false,
            skip_rx_copy: false,
            rcv_timeout: None,
            snd_timeout: None,
            file: None,
            dscp: None,
            format_char: 'a',
            interval: None,
            cntl_ka: None,
            username: None,
            password: None,
            rsa_public_key_path: None,
            use_pkcs1_padding: false,
        }
    }
}

impl ClientBuilder {
    pub fn new(host: &str) -> Self {
        Self::default().host(host)
    }

    /// `-c/--client <host>`: the server to connect to (hostname or IP literal).
    pub fn host(mut self, host: &str) -> Self {
        self.host = Some(host.to_string());
        self
    }

    /// `-p/--port`: server control port to connect to (default 5201); `None`
    /// resolves back to the default at `build()`.
    pub fn port(mut self, port: Option<u16>) -> Self {
        self.port = port;
        self
    }

    /// `-u/--udp`: transport protocol for the data streams (default TCP).
    pub fn protocol(mut self, protocol: TransportProtocol) -> Self {
        self.protocol = protocol;
        self
    }

    /// `-t/--time`: time in seconds to transmit for (default 10).
    pub fn duration(mut self, secs: u32) -> Self {
        self.duration = secs;
        self
    }

    /// `-P/--parallel`: number of parallel data streams (default 1).
    pub fn num_streams(mut self, n: u32) -> Self {
        self.num_streams = n;
        self
    }

    /// `-l/--length`: length of the read/write buffer in bytes (default 128 KB
    /// for TCP; unset UDP derives the datagram size from the control-socket MSS).
    pub fn blksize(mut self, size: usize) -> Self {
        self.blksize = Some(size);
        self
    }

    /// `-R/--reverse`: reverse mode — the server sends, the client receives.
    pub fn reverse(mut self, reverse: bool) -> Self {
        self.reverse = reverse;
        self
    }

    /// `--bidir`: bidirectional mode — client and server send and receive
    /// simultaneously.
    pub fn bidir(mut self, bidir: bool) -> Self {
        self.bidir = bidir;
        self
    }

    /// `-O/--omit`: omit the first `secs` seconds of the test (e.g. TCP
    /// slow-start) from the results (default 0).
    pub fn omit(mut self, secs: u32) -> Self {
        self.omit = secs;
        self
    }

    /// `-N/--no-delay`: set `TCP_NODELAY`, disabling Nagle's algorithm.
    pub fn no_delay(mut self, no_delay: bool) -> Self {
        self.no_delay = no_delay;
        self
    }

    /// `-M/--set-mss`: TCP maximum segment size (MTU - 40 bytes).
    pub fn mss(mut self, mss: i32) -> Self {
        self.mss = Some(mss);
        self
    }

    /// `-w/--window`: socket buffer size in bytes (indirectly sets the TCP
    /// window size).
    pub fn window(mut self, window: i32) -> Self {
        self.window = Some(window);
        self
    }

    /// `-b/--bitrate`: target bitrate in bits/sec; 0 = unlimited. Unset resolves
    /// at `build()` to the iperf3 default: unlimited for TCP, 1 Mbit/sec for UDP.
    pub fn bandwidth(mut self, bps: u64) -> Self {
        // `Some` even for 0: an explicit `-b 0` means unlimited and must be
        // distinguishable from "unset" (which resolves to the UDP default) (#17).
        self.bandwidth = Some(bps);
        self
    }

    /// `-b rate/burst` burst count: blocks sent per throttle green light
    /// (iperf3's multisend batch, 1..=1000; 0 = unset) (#160). Range-checked
    /// at `build()`.
    pub fn burst(mut self, blocks: u32) -> Self {
        self.burst = blocks;
        self
    }

    /// `--pacing-timer`: the `-b` throttle's wakeup quantum in microseconds
    /// (iperf3 default 1000). 0 falls back to the default.
    pub fn pacing_timer(mut self, us: u32) -> Self {
        self.pacing_timer = us;
        self
    }

    /// `-S/--tos`: IP type-of-service value (0-255). Symbolic DSCP names go
    /// through [`Self::dscp`] instead.
    pub fn tos(mut self, tos: i32) -> Self {
        self.tos = tos;
        self
    }

    /// `-C/--congestion`: TCP congestion control algorithm (e.g. `cubic`,
    /// `bbr`); Linux/FreeBSD only; silently unavailable elsewhere on unix, rejected at `build()` on non-unix.
    pub fn congestion(mut self, algo: &str) -> Self {
        self.congestion = Some(algo.to_string());
        self
    }

    /// `--udp-counters-64bit`: use 64-bit sequence counters in UDP test packets.
    pub fn udp_counters_64bit(mut self, enabled: bool) -> Self {
        self.udp_counters_64bit = enabled;
        self
    }

    /// `--connect-timeout`: timeout for establishing the control connection
    /// (the CLI flag takes milliseconds).
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = Some(timeout);
        self
    }

    /// Prefix every client text-output line with `<title>:  ` (`-T/--title`),
    /// matching iperf3. Applies only to plain-text output, not `-J`/`--json-stream`.
    ///
    /// Note: the prefix is tracked in a process-global for the duration of the
    /// run, so two `Client::run` calls executing concurrently in the same process
    /// are not isolated for `-T` (their titled lines can interleave). This does
    /// not affect the CLI (one run per process) or sequential library use.
    pub fn title(mut self, title: &str) -> Self {
        self.title = Some(title.to_string());
        self
    }

    /// `--extra-data`: extra data string to include in the JSON output.
    pub fn extra_data(mut self, data: &str) -> Self {
        self.extra_data = Some(data.to_string());
        self
    }

    /// `-V/--verbose`: enable verbose output.
    pub fn verbose(mut self, verbose: bool) -> Self {
        self.verbose = verbose;
        self
    }

    /// `-J/--json`: emit the results as iperf3-schema JSON on stdout instead
    /// of text.
    /// When combined with [`Self::json_stream`], stream mode wins (#220).
    pub fn json_output(mut self, enabled: bool) -> Self {
        self.json_output = enabled;
        self
    }

    /// `-n/--bytes`: end the test after this many bytes, instead of running
    /// for a set time (`-t`). 0 means "no byte limit" (iperf3 semantics) and
    /// is normalized to unset at build().
    pub fn bytes(mut self, bytes: u64) -> Self {
        self.bytes_to_send = Some(bytes);
        self
    }

    /// `-k/--blockcount`: end the test after this many blocks (packets),
    /// instead of `-t` or `-n`. 0 means "no block limit" (iperf3 semantics)
    /// and is normalized to unset at build().
    pub fn blocks(mut self, blocks: u64) -> Self {
        self.blocks_to_send = Some(blocks);
        self
    }

    /// `--json-stream`: stream line-delimited interval JSON during the test.
    /// Combined with [`Self::json_output`], stream mode WINS — iperf3's
    /// OPT_JSON_STREAM implies -J, so the hybrid is simply stream mode
    /// (full event stream incl. `end`; the monolithic document only with
    /// [`Self::json_stream_full_output`]) (#220).
    pub fn json_stream(mut self, enabled: bool) -> Self {
        self.json_stream = enabled;
        self
    }

    /// Wire an interrupt watch (#210): when the consumer sends a message
    /// (iperf3's "interrupt - the client has terminated by signal …"), a
    /// running test dumps its accumulated stats like iperf_got_sigend,
    /// notifies the peer via CLIENT_TERMINATE on the control socket, and
    /// `run()` returns normally with the local results — the caller owns the
    /// exit (iperf3 exits 0 on TERM/INT/HUP).
    pub fn interrupt(mut self, rx: tokio::sync::watch::Receiver<Option<String>>) -> Self {
        self.interrupt = Some(InterruptWatch(rx));
        self
    }

    /// With json-stream, also print the complete monolithic JSON document
    /// after the stream ends — iperf3's `--json-stream-full-output`, the
    /// third leg of its discard_json condition (#213).
    pub fn json_stream_full_output(mut self, enabled: bool) -> Self {
        self.json_stream_full_output = enabled;
        self
    }

    /// `--repeating-payload`: use a repeating pattern in the payload instead
    /// of zeros.
    pub fn repeating_payload(mut self, enabled: bool) -> Self {
        self.repeating_payload = enabled;
        self
    }

    /// `-Z/--zerocopy`: use a zero-copy (`sendfile`) method of sending data;
    /// Linux/macOS/FreeBSD only. On other unix — and on any platform whenever
    /// `-b` pacing or an `-n`/`-k` byte budget is in effect — it silently
    /// falls back to the normal copying sender; rejected at `build()` on
    /// non-unix.
    pub fn zerocopy(mut self, enabled: bool) -> Self {
        self.zerocopy = enabled;
        self
    }

    /// `--gsro`: enable UDP GSO/GRO (generic segmentation/receive offload);
    /// Linux only; a silent no-op elsewhere on unix, rejected at `build()` on
    /// non-unix.
    pub fn gsro(mut self, enabled: bool) -> Self {
        self.gsro = enabled;
        self
    }

    /// `--sendmmsg`: batched UDP sends via `sendmmsg(2)` (experimental,
    /// Linux/FreeBSD/NetBSD). riperf3 extension with no iperf3 equivalent.
    pub fn sendmmsg(mut self, enabled: bool) -> Self {
        self.sendmmsg = enabled;
        self
    }

    /// `--dont-fragment`: set the IPv4 Don't Fragment flag on UDP packets.
    pub fn dont_fragment(mut self, enabled: bool) -> Self {
        self.dont_fragment = enabled;
        self
    }

    /// `--cport`: bind to a specific local client port (default: ephemeral).
    pub fn cport(mut self, port: u16) -> Self {
        self.cport = Some(port);
        self
    }

    /// `--get-server-output`: retrieve the server-side output and include it
    /// in the client's results.
    pub fn get_server_output(mut self, enabled: bool) -> Self {
        self.get_server_output = enabled;
        self
    }

    /// `--forceflush`: force flushing output at every interval.
    pub fn forceflush(mut self, enabled: bool) -> Self {
        self.forceflush = enabled;
        self
    }

    /// `--timestamps`: prefix each output line with a timestamp in the given
    /// `strftime` format (the CLI defaults to `"%c "` when no format is given).
    pub fn timestamps(mut self, fmt: &str) -> Self {
        self.timestamps = Some(fmt.to_string());
        self
    }

    /// `-B/--bind`: bind to a specific local source address (interface binding
    /// is [`Self::bind_dev`]).
    pub fn bind_address(mut self, addr: &str) -> Self {
        self.bind_address = Some(addr.to_string());
        self
    }

    /// `--bind-dev`: bind data sockets to a network device. Linux
    /// (`SO_BINDTODEVICE`) and macOS (`IP_BOUND_IF`/`IPV6_BOUND_IF`) only;
    /// rejected at `build()` everywhere else (#149) — matching iperf3, whose
    /// client-side IP_BOUND_IF fallback covers exactly these two.
    pub fn bind_dev(mut self, dev: &str) -> Self {
        self.bind_dev = Some(dev.to_string());
        self
    }

    /// `--fq-rate`: fair-queuing based socket pacing rate in bits/sec
    /// (Linux only).
    pub fn fq_rate(mut self, rate: u64) -> Self {
        self.fq_rate = Some(rate);
        self
    }

    /// `-L/--flowlabel`: IPv6 flow label (Linux only).
    pub fn flowlabel(mut self, label: i32) -> Self {
        self.flowlabel = Some(label);
        self
    }

    /// `-4`/`-6`: only use IPv4 (`4`) or IPv6 (`6`) when connecting. Leave
    /// unset to use whichever family the host resolves to.
    pub fn ip_version(mut self, version: u8) -> Self {
        debug_assert!(
            matches!(version, 4 | 6),
            "ip_version must be 4 or 6, got {version}"
        );
        self.ip_version = Some(version);
        self
    }

    /// `-m/--mptcp`: use MPTCP rather than plain TCP.
    pub fn mptcp(mut self, enabled: bool) -> Self {
        self.mptcp = enabled;
        self
    }

    /// `--skip-rx-copy`: discard received data in the kernel with `MSG_TRUNC`,
    /// skipping the copy to userspace.
    pub fn skip_rx_copy(mut self, enabled: bool) -> Self {
        self.skip_rx_copy = enabled;
        self
    }

    /// `--rcv-timeout`: idle-receive timeout in ms. Sets `SO_RCVTIMEO` on the
    /// data socket; note tokio sockets are nonblocking, where the kernel
    /// timeout does not fire on reads — parity with iperf3's flag surface,
    /// effective behavior under review.
    pub fn rcv_timeout(mut self, ms: u64) -> Self {
        self.rcv_timeout = Some(ms);
        self
    }

    /// `--snd-timeout`: timeout for unacknowledged TCP data, in milliseconds
    /// (`TCP_USER_TIMEOUT`, Linux only).
    pub fn snd_timeout(mut self, ms: u64) -> Self {
        self.snd_timeout = Some(ms);
        self
    }

    /// `-F/--file`: sending streams read the payload from this file instead of
    /// generated data; receiving streams write received data to it.
    pub fn file(mut self, path: &str) -> Self {
        self.file = Some(path.to_string());
        self
    }

    /// `--dscp`: IP DSCP value, numeric (0-63) or symbolic (e.g. `CS5`);
    /// overrides [`Self::tos`] at `build()`.
    pub fn dscp(mut self, val: &str) -> Self {
        self.dscp = Some(val.to_string());
        self
    }

    /// `-f/--format`: report units — `k`/`m`/`g`/`t` for bits, uppercase for
    /// bytes; the default `'a'` picks adaptively.
    pub fn format_char(mut self, c: char) -> Self {
        self.format_char = c;
        self
    }

    /// `-i/--interval`: seconds between periodic throughput reports (default 1).
    pub fn interval(mut self, secs: f64) -> Self {
        self.interval = Some(secs);
        self
    }

    /// `--cntl-ka`: enable TCP keepalive on the control connection; `spec` is
    /// `idle/intv/cnt`.
    pub fn cntl_ka(mut self, spec: &str) -> Self {
        self.cntl_ka = Some(spec.to_string());
        self
    }

    /// `--username`: username for authentication (used with a password and
    /// [`Self::rsa_public_key_path`]).
    pub fn username(mut self, name: &str) -> Self {
        self.username = Some(name.to_string());
        self
    }

    /// Password for authentication. iperf3 has no flag for this; the CLI reads
    /// the `RIPERF3_PASSWORD`/`IPERF3_PASSWORD` environment variables or prompts.
    pub fn password(mut self, pass: &str) -> Self {
        self.password = Some(pass.to_string());
        self
    }

    /// `--rsa-public-key-path`: path to the RSA public key used to encrypt the
    /// authentication credentials.
    pub fn rsa_public_key_path(mut self, path: &str) -> Self {
        self.rsa_public_key_path = Some(path.to_string());
        self
    }

    /// `--use-pkcs1-padding`: encrypt credentials with PKCS#1 v1.5 padding
    /// instead of OAEP (for pre-3.17 iperf3 servers). The CLI rejects this flag
    /// for clients, matching iperf3 (#100); only embedders can set it here.
    pub fn use_pkcs1_padding(mut self, enabled: bool) -> Self {
        self.use_pkcs1_padding = enabled;
        self
    }

    // String-accepting variants — parse KMG suffixes (e.g., "1M", "512K", "10G")
    // so callers don't need to import parse_kmg/parse_bitrate.

    /// Like [`Self::bytes`], accepting a KMG-suffixed size string
    /// (`-n 100M`; binary, 1024-based).
    pub fn bytes_str(self, s: &str) -> std::result::Result<Self, ConfigError> {
        Ok(self.bytes(parse_kmg(s)?))
    }

    /// Like [`Self::blocks`], accepting a KMG-suffixed count string
    /// (`-k 10K`; binary, 1024-based).
    pub fn blocks_str(self, s: &str) -> std::result::Result<Self, ConfigError> {
        Ok(self.blocks(parse_kmg(s)?))
    }

    /// Like [`Self::blksize`], accepting a KMG-suffixed size string
    /// (`-l 128K`; binary, 1024-based).
    pub fn blksize_str(self, s: &str) -> std::result::Result<Self, ConfigError> {
        Ok(self.blksize(parse_kmg(s)? as usize))
    }

    /// Like [`Self::window`], accepting a KMG-suffixed size string
    /// (`-w 4M`; binary, 1024-based).
    pub fn window_str(self, s: &str) -> std::result::Result<Self, ConfigError> {
        Ok(self.window(parse_kmg(s)? as i32))
    }

    /// Like [`Self::bandwidth`], accepting an iperf3 rate string
    /// (`-b 10M[/burst]`; decimal, 1000-based). A `/burst` count is applied
    /// per [`Self::burst`] (#160).
    pub fn bandwidth_str(self, s: &str) -> std::result::Result<Self, ConfigError> {
        let (rate, burst) = parse_bitrate(s)?;
        Ok(self.bandwidth(rate).burst(burst))
    }

    /// Like [`Self::tos`], accepting iperf3's `-S` string forms: decimal,
    /// `0x` hex, or leading-`0` octal (strtol base 0), range 0-255 (#167).
    pub fn tos_str(self, s: &str) -> std::result::Result<Self, ConfigError> {
        Ok(self.tos(crate::utils::parse_tos(s)?))
    }

    /// Like [`Self::pacing_timer`], accepting a KMG-suffixed string
    /// (`--pacing-timer 1K`; binary, 1024-based, like iperf3's `unit_atoi`) (#160).
    pub fn pacing_timer_str(self, s: &str) -> std::result::Result<Self, ConfigError> {
        let us = parse_kmg(s)?;
        // The wire TestParams field is i32; larger would wrap negative.
        if us > i32::MAX as u64 {
            return Err(ConfigError::InvalidValue("pacing_timer", s.to_string()));
        }
        Ok(self.pacing_timer(us as u32))
    }

    /// Like [`Self::fq_rate`], accepting an iperf3 rate string
    /// (`--fq-rate 1G`; decimal, 1000-based).
    pub fn fq_rate_str(self, s: &str) -> std::result::Result<Self, ConfigError> {
        // --fq-rate is a rate: decimal (1000-based) suffixes, like iperf3 (#56).
        Ok(self.fq_rate(crate::utils::parse_rate(s)?))
    }

    pub fn build(self) -> std::result::Result<Client, ConfigError> {
        let host = self.host.ok_or(ConfigError::MissingField("host"))?;

        // Reject a -B literal whose family contradicts -4/-6 at config time,
        // mirroring the server-side check (#12); a bind hostname is validated
        // against the target family at connect time instead (#15).
        if let (Some(v), Some(addr)) = (self.ip_version, self.bind_address.as_deref()) {
            let addr = addr.split('%').next().unwrap_or(addr);
            if let Ok(ip) = addr.parse::<std::net::IpAddr>() {
                if (v == 4 && ip.is_ipv6()) || (v == 6 && ip.is_ipv4()) {
                    return Err(ConfigError::InvalidValue(
                        "bind_address",
                        format!("-{v} conflicts with bind address {addr}"),
                    ));
                }
            }
        }

        // iperf3 caps the warm-up at MAX_OMIT_TIME (600 s, iperf.h) with
        // IEOMIT (#31; review r1 — the cap is 600, not 60).
        if self.omit > 600 {
            return Err(ConfigError::InvalidValue(
                "omit",
                format!(
                    "bogus value for --omit (maximum = 600 seconds): {}",
                    self.omit
                ),
            ));
        }

        // iperf3 rejects -i outside {0} ∪ [MIN_INTERVAL, MAX_INTERVAL] with
        // IEINTERVAL (iperf_api.c:1261; 0.1/60 in iperf.h). Load-bearing for
        // -O: the reporter owns the omit boundary, so an out-of-range
        // interval silently disabling it would silently disable omit
        // semantics too (#31, review r3).
        if let Some(i) = self.interval {
            if i != 0.0 && !(0.1..=60.0).contains(&i) {
                return Err(ConfigError::InvalidValue(
                    "interval",
                    format!("invalid report interval (min = 0.1, max = 60 seconds): {i}"),
                ));
            }
        }

        // Reject flags that require OS support not available on this platform.
        // Matches iperf3 behavior: error at build/parse time, not at runtime.
        #[cfg(not(unix))]
        {
            if self.zerocopy {
                return Err(ConfigError::Unsupported(
                    "this OS does not support sendfile".into(),
                ));
            }
            if self.congestion.is_some() {
                return Err(ConfigError::Unsupported(
                    "TCP congestion control is not supported on this platform".into(),
                ));
            }
            if self.gsro {
                return Err(ConfigError::Unsupported(
                    "UDP GSO/GRO is not supported on this platform".into(),
                ));
            }
        }

        // --bind-dev needs SO_BINDTODEVICE (Linux) or IP_BOUND_IF (macOS). The
        // old gate only covered not(unix), so FreeBSD/NetBSD silently
        // no-opped through net.rs's fallback — no binding, no error (#149).
        // iperf3 without CAN_BIND_TO_DEVICE doesn't recognize the option.
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        if self.bind_dev.is_some() {
            return Err(ConfigError::Unsupported(
                "--bind-dev is not supported on this platform".into(),
            ));
        }

        // sendmmsg's real implementation is Linux/FreeBSD/NetBSD only; elsewhere
        // (incl. macOS, which is `unix` but unsupported) it would silently fall
        // back to the per-packet sender, so reject it at build time instead (#18).
        #[cfg(not(any(target_os = "linux", target_os = "freebsd", target_os = "netbsd")))]
        if self.sendmmsg {
            return Err(ConfigError::Unsupported(
                "sendmmsg is only supported on Linux, FreeBSD, and NetBSD".into(),
            ));
        }

        let default_blksize = match self.protocol {
            TransportProtocol::Tcp => DEFAULT_TCP_BLKSIZE,
            TransportProtocol::Udp => DEFAULT_UDP_BLKSIZE,
        };

        // If --dscp is set, convert to TOS and override
        let tos = if let Some(ref dscp) = self.dscp {
            parse_dscp(dscp)?
        } else {
            self.tos
        };
        // IEBADTOS parity for the i32 setter path (the string path validates
        // in parse_tos; --dscp resolves to 0-252 by construction) (#167).
        if !(0..=255).contains(&tos) {
            return Err(ConfigError::InvalidValue(
                "tos",
                format!("bad TOS value (must be between 0 and 255 inclusive): {tos}"),
            ));
        }

        // IEBURST parity for the u32 setter path (#160).
        if self.burst > crate::utils::MAX_BURST {
            return Err(ConfigError::InvalidValue(
                "burst count",
                format!(
                    "invalid burst count (maximum = {}): {}",
                    crate::utils::MAX_BURST,
                    self.burst
                ),
            ));
        }

        // -l 0 means "unset", like iperf3 (blksize 0 picks up the protocol
        // default before validation; for UDP the dynamic-MSS resolution
        // applies). A nonzero value is bounds-checked per protocol: TCP
        // 1..=MAX_BLOCKSIZE (IEBLOCKSIZE), UDP MIN..=MAX_UDP_BLKSIZE
        // (IEUDPBLOCKSIZE) (#188).
        let blksize_req = self.blksize.filter(|&b| b != 0);
        if let Some(b) = blksize_req {
            match self.protocol {
                TransportProtocol::Tcp if b > crate::utils::MAX_BLOCKSIZE => {
                    return Err(ConfigError::InvalidValue(
                        "len",
                        format!(
                            "block size too large (maximum = {} bytes): {b}",
                            crate::utils::MAX_BLOCKSIZE
                        ),
                    ));
                }
                TransportProtocol::Udp
                    if !(crate::utils::MIN_UDP_BLKSIZE..=MAX_UDP_BLKSIZE).contains(&b) =>
                {
                    return Err(ConfigError::InvalidValue(
                        "len",
                        format!(
                            "block size invalid (minimum = {} bytes, maximum = {} bytes): {b}",
                            crate::utils::MIN_UDP_BLKSIZE,
                            MAX_UDP_BLKSIZE
                        ),
                    ));
                }
                _ => {}
            }
        }

        Ok(Client {
            host,
            port: self.port.unwrap_or(DEFAULT_PORT),
            protocol: self.protocol,
            duration: self.duration,
            num_streams: self.num_streams,
            blksize: blksize_req.unwrap_or(default_blksize),
            blksize_explicit: blksize_req.is_some(),
            reverse: self.reverse,
            bidir: self.bidir,
            omit: self.omit,
            no_delay: self.no_delay,
            mss: self.mss,
            window: self.window,
            // Resolve the rate default now (UDP unset → 1 Mbit/s, like iperf3);
            // an explicit -b (incl. 0 = unlimited) is honored. TCP default is
            // unlimited (0). After this, bandwidth==0 unambiguously = unlimited.
            bandwidth: self.bandwidth.unwrap_or(match self.protocol {
                TransportProtocol::Udp => DEFAULT_UDP_RATE,
                TransportProtocol::Tcp => 0,
            }),
            burst: self.burst,
            // 0 = unset → iperf3's default quantum, like its pacing_timer
            // option parsing (it never sends 0).
            pacing_timer: if self.pacing_timer == 0 {
                crate::utils::DEFAULT_PACING_TIMER_US
            } else {
                self.pacing_timer
            },
            tos,
            congestion: self.congestion,
            udp_counters_64bit: self.udp_counters_64bit,
            connect_timeout: self.connect_timeout,
            title: self.title,
            extra_data: self.extra_data,
            verbose: self.verbose,
            json_output: self.json_output,
            json_stream: self.json_stream,
            interrupt: self.interrupt.clone(),
            json_stream_full_output: self.json_stream_full_output,
            // 0 means "no limit" in iperf3 (`-n 0`/`-k 0` run a plain duration
            // test — its end-condition checks gate on the value), so normalize
            // to unset here rather than ending the test instantly (#140).
            bytes_to_send: self.bytes_to_send.filter(|&b| b != 0),
            blocks_to_send: self.blocks_to_send.filter(|&b| b != 0),
            repeating_payload: self.repeating_payload,
            zerocopy: self.zerocopy,
            gsro: self.gsro,
            sendmmsg: self.sendmmsg,
            dont_fragment: self.dont_fragment,
            cport: self.cport,
            get_server_output: self.get_server_output,
            forceflush: self.forceflush,
            timestamps: self.timestamps,
            bind_address: self.bind_address,
            bind_dev: self.bind_dev,
            fq_rate: self.fq_rate,
            flowlabel: self.flowlabel,
            ip_version: self.ip_version,
            mptcp: self.mptcp,
            skip_rx_copy: self.skip_rx_copy,
            rcv_timeout: self.rcv_timeout,
            snd_timeout: self.snd_timeout,
            file: self.file,
            format_char: self.format_char,
            interval: self.interval,
            cntl_ka: self.cntl_ka,
            username: self.username,
            password: self.password,
            rsa_public_key_path: self.rsa_public_key_path,
            use_pkcs1_padding: self.use_pkcs1_padding,
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    // #147 (review r1): the discriminating leak test. The e2e mock below can't
    // pin the fix — run()'s DoneOnDrop guard already stops the SENDERS on any
    // exit, pre-fix included. The real pre-fix leak was the REPORTER task:
    // `done` was never set on the ServerTerminate early-return, so it stayed
    // parked holding the collector Arc (forever under -i 0's year-long
    // ticker). Calling run_test directly bypasses DoneOnDrop, and the
    // collector's strong count observes the reporter's clone: pre-fix this
    // asserts 2 (parked reporter), post-fix 1 (joined before propagating).
    #[tokio::test]
    async fn abort_path_joins_the_reporter() {
        use crate::protocol::{self, TestState};
        use std::sync::atomic::AtomicBool;
        use std::sync::{Arc, Mutex};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let srv = tokio::spawn(async move {
            let (mut ctrl, _) = listener.accept().await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            protocol::send_state(&mut ctrl, TestState::ServerTerminate)
                .await
                .unwrap();
            // Keep the control socket open past the client's assertions.
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        });

        // json_output(true) makes the reporter take the collector clone.
        let client = crate::ClientBuilder::new("127.0.0.1")
            .duration(10)
            .json_output(true)
            .build()
            .unwrap();
        let mut ctrl = tokio::net::TcpStream::connect(addr).await.unwrap();
        let done = Arc::new(AtomicBool::new(false));
        let collector = Arc::new(Mutex::new(crate::reporter::CollectedIntervals::default()));

        let res = client
            .run_test(
                &mut ctrl,
                &[],
                &done,
                131072,
                collector.clone(),
                None,
                &mut None,
            )
            .await;
        // Since #170 run_test reports the termination as an outcome (the
        // caller renders the partial summary then errors with IESERVERTERM).
        assert!(
            matches!(res, Ok((_, Some(ControlEvent::Terminated)))),
            "ServerTerminate must surface as the terminated outcome: {res:?}"
        );
        assert_eq!(
            Arc::strong_count(&collector),
            1,
            "#147: the reporter must be JOINED before the abort propagates \
             (a parked reporter still holds the collector Arc)"
        );
        srv.abort();
    }

    // #156 (review r2): build_results runs at ExchangeResults — strictly after
    // the sender task has dropped (closed) its socket — so the retransmit
    // total must be captured while the socket was alive. A kernel read from
    // the dead fd fails and ships the -1 sentinel beside
    // sender_has_retransmits=1, which iperf3 peers render as a bogus Retr
    // count (u64::MAX on 3.12). `raw_fd: None` models the dead-fd state
    // deterministically: under parallel tests a real closed fd can be
    // recycled by another test's socket, making get_tcp_info spuriously
    // succeed; the pinned property is identical — the exchange value must
    // not depend on an exchange-time fd read.
    #[tokio::test]
    async fn exchange_retransmits_survive_sender_socket_close() {
        use std::sync::atomic::{AtomicBool, AtomicI64};
        use std::sync::Arc;
        use tokio::io::AsyncReadExt;

        if !crate::tcp_info::has_retransmit_info() {
            return; // flag is never 1 here; there is no contract to pin
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let drain = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 65536];
            while sock.read(&mut buf).await.unwrap_or(0) > 0 {}
        });

        let sock = tokio::net::TcpStream::connect(addr).await.unwrap();
        let counters = Arc::new(crate::stream::StreamCounters::new());
        let done = Arc::new(AtomicBool::new(false));
        // 1 MiB budget. A sender at budget exhaustion IDLES waiting for a
        // refill or `done` (#31) — it no longer self-terminates — so drive it
        // like the real run does: wait for the budget to be consumed, then
        // set `done`. The exit path still snapshots the retransmit total
        // before the socket drops.
        let budget = Arc::new(AtomicI64::new(1 << 20));
        let sender = tokio::spawn(crate::stream::run_tcp_sender(
            sock,
            counters.clone(),
            vec![0u8; 131072],
            done.clone(),
            None,
            0,
            1000,
            0,
            Some(budget.clone()),
        ));
        while budget.load(std::sync::atomic::Ordering::Relaxed) > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        done.store(true, std::sync::atomic::Ordering::Relaxed);
        sender.await.unwrap().unwrap();
        drain.await.unwrap();

        let client = crate::ClientBuilder::new("127.0.0.1").build().unwrap();
        let ds = crate::stream::DataStream {
            id: 1,
            is_sender: true,
            counters,
            udp_recv_stats: None,
            task: tokio::spawn(async { Ok(()) }),
            raw_fd: None,
            local_addr: None,
            peer_addr: None,
            sndbuf_actual: None,
            rcvbuf_actual: None,
            congestion_used: None,
        };
        let results = client.build_results(std::slice::from_ref(&ds), None, 1460, 1.0);
        assert_eq!(results.sender_has_retransmits, 1);
        assert!(
            results.streams[0].retransmits >= 0,
            "#156: a TCP sender whose socket has closed must still report \
             its real end-of-test retransmit total (got {})",
            results.streams[0].retransmits
        );
        ds.task.abort();
    }

    use super::*;

    // #31 (review r3 blocker 1): iperf3's -n/-k end check uses test-level
    // counters (iperf_client_api.c:771-772) — bytes_sent is zeroed at the
    // omit boundary by iperf_reset_stats (iperf_api.c:3675) but
    // test->bytes_received never is, so the receive side counts GROSS,
    // warm-up included. Counting net on the receive side hangs a reverse
    // -n -O run whenever a mistimed boundary baseline swallows warm-up bytes.
    #[tokio::test]
    async fn byte_limit_counts_received_gross_and_sent_net() {
        use crate::stream::{DataStream, StreamCounters};

        let sender = Arc::new(StreamCounters::new());
        sender.record_sent(1_000);
        sender.snapshot_omit();
        sender.record_sent(300); // post-omit net: 300
        let receiver = Arc::new(StreamCounters::new());
        receiver.record_received(1_000);
        receiver.snapshot_omit(); // the boundary must NOT hide warm-up receive
        receiver.record_received(300);

        let mk = |is_sender: bool, counters: Arc<StreamCounters>| DataStream {
            id: 1,
            is_sender,
            counters,
            udp_recv_stats: None,
            task: tokio::spawn(async { Ok(()) }),
            raw_fd: None,
            local_addr: None,
            peer_addr: None,
            sndbuf_actual: None,
            rcvbuf_actual: None,
            congestion_used: None,
        };
        let streams = [mk(true, sender), mk(false, receiver)];
        assert_eq!(
            transferred_bytes(&streams),
            1_300,
            "received counts gross (1300), sent counts net (300)"
        );
        for s in &streams {
            s.task.abort();
        }
    }

    // #171: with -O, the exchanged per-stream retransmit total must cover
    // the post-omit window only — iperf3's iperf_reset_stats records
    // stream_prev_total_retrans at the boundary (iperf_api.c:3687-3692) and
    // stream_retrans accumulates from there. The reporter's boundary block
    // stores the same baseline into StreamCounters; the exchange subtracts.
    #[tokio::test]
    async fn exchange_retransmits_subtract_the_omit_baseline() {
        use crate::stream::{DataStream, StreamCounters};

        if !crate::tcp_info::has_retransmit_info() {
            return;
        }

        let counters = Arc::new(StreamCounters::new());
        counters.set_omit_retransmits(5); // boundary baseline (warm-up retransmits)
        counters.set_final_retransmits(8); // connection-lifetime total at exit
        let ds = DataStream {
            id: 1,
            is_sender: true,
            counters,
            udp_recv_stats: None,
            task: tokio::spawn(async { Ok(()) }),
            raw_fd: None,
            local_addr: None,
            peer_addr: None,
            sndbuf_actual: None,
            rcvbuf_actual: None,
            congestion_used: None,
        };
        let client = crate::ClientBuilder::new("127.0.0.1").build().unwrap();
        let results = client.build_results(std::slice::from_ref(&ds), None, 1460, 1.0);
        assert_eq!(
            results.streams[0].retransmits, 3,
            "#171: warm-up retransmits (5) must be subtracted from the \
             lifetime total (8)"
        );
        ds.task.abort();
    }

    // iperf3 rejects -i outside {0} ∪ [0.1, 60] with IEINTERVAL
    // (iperf_api.c:1261, MIN_INTERVAL/MAX_INTERVAL in iperf.h). With -O the
    // reporter is load-bearing (it owns the omit boundary), so an invalid
    // interval silently disabling it must be impossible (review r3 nit).
    #[test]
    fn client_builder_rejects_out_of_range_interval() {
        for bad in [-1.0, 0.05, 60.1] {
            assert!(
                ClientBuilder::new("h").interval(bad).build().is_err(),
                "interval {bad} must be rejected"
            );
        }
        for ok in [0.0, 0.1, 1.0, 60.0] {
            assert!(
                ClientBuilder::new("h").interval(ok).build().is_ok(),
                "interval {ok} must be accepted"
            );
        }
    }

    // Per-setter builder tests migrated in-crate from `tests/integration.rs`
    // when `Client`'s fields became `pub(crate)` (#43): an external test crate
    // can no longer read `c.protocol`, `c.duration`, etc.
    mod builder_setter_tests {
        use super::*;
        use std::time::Duration;

        #[test]
        fn client_builder_protocol() {
            let c = ClientBuilder::new("h")
                .protocol(TransportProtocol::Udp)
                .build()
                .unwrap();
            assert_eq!(c.protocol, TransportProtocol::Udp);
        }

        #[test]
        fn client_builder_duration() {
            let c = ClientBuilder::new("h").duration(30).build().unwrap();
            assert_eq!(c.duration, 30);
        }

        #[test]
        fn client_builder_num_streams() {
            let c = ClientBuilder::new("h").num_streams(8).build().unwrap();
            assert_eq!(c.num_streams, 8);
        }

        #[test]
        fn client_builder_blksize() {
            let c = ClientBuilder::new("h").blksize(65536).build().unwrap();
            assert_eq!(c.blksize, 65536);
        }

        #[test]
        fn client_builder_blksize_defaults() {
            let tcp = ClientBuilder::new("h").build().unwrap();
            assert_eq!(tcp.blksize, 128 * 1024);

            let udp = ClientBuilder::new("h")
                .protocol(TransportProtocol::Udp)
                .build()
                .unwrap();
            assert_eq!(udp.blksize, 1460);
        }

        #[test]
        fn client_builder_reverse() {
            let c = ClientBuilder::new("h").reverse(true).build().unwrap();
            assert!(c.reverse);
        }

        #[test]
        fn omit_cap_matches_iperf3_max_omit_time() {
            // iperf3's MAX_OMIT_TIME is 600 (iperf.h); -O 600 accepted, 601
            // rejected with IEOMIT's wording (r1 blocker 4: was capped at 60).
            assert!(ClientBuilder::new("h").omit(600).build().is_ok());
            let err = ClientBuilder::new("h").omit(601).build().unwrap_err();
            assert!(
                format!("{err}").contains("maximum = 600 seconds"),
                "IEOMIT wording expected: {err}"
            );
        }

        #[test]
        fn client_builder_bidir() {
            let c = ClientBuilder::new("h").bidir(true).build().unwrap();
            assert!(c.bidir);
        }

        #[test]
        fn client_builder_omit() {
            let c = ClientBuilder::new("h").omit(3).build().unwrap();
            assert_eq!(c.omit, 3);
        }

        #[test]
        fn client_builder_no_delay() {
            let c = ClientBuilder::new("h").no_delay(true).build().unwrap();
            assert!(c.no_delay);
        }

        #[test]
        fn client_builder_mss() {
            let c = ClientBuilder::new("h").mss(1400).build().unwrap();
            assert_eq!(c.mss, Some(1400));
        }

        #[test]
        fn client_builder_window() {
            let c = ClientBuilder::new("h").window(524288).build().unwrap();
            assert_eq!(c.window, Some(524288));
        }

        #[test]
        fn build_blksize_zero_is_default() {
            // -l 0 means "unset", like iperf3: blksize 0 resolves to the
            // protocol default pre-validation; for UDP the dynamic-MSS path
            // stays live (blksize_explicit = false) (#188).
            let c = ClientBuilder::new("h").blksize(0).build().unwrap();
            assert_eq!(c.blksize, DEFAULT_TCP_BLKSIZE);
            assert!(!c.blksize_explicit);
            let c = ClientBuilder::new("h")
                .protocol(TransportProtocol::Udp)
                .blksize(0)
                .build()
                .unwrap();
            assert_eq!(c.blksize, DEFAULT_UDP_BLKSIZE);
            assert!(!c.blksize_explicit);
        }

        #[test]
        fn build_blksize_bounds_match_iperf3() {
            // TCP: 1..=MAX_BLOCKSIZE (IEBLOCKSIZE); UDP: MIN..=MAX_UDP_BLKSIZE
            // (IEUDPBLOCKSIZE) (#188).
            let tcp = |b| ClientBuilder::new("h").blksize(b).build();
            let udp = |b| {
                ClientBuilder::new("h")
                    .protocol(TransportProtocol::Udp)
                    .blksize(b)
                    .build()
            };
            assert!(tcp(MAX_BLOCKSIZE).is_ok());
            assert!(tcp(MAX_BLOCKSIZE + 1).is_err());
            assert!(udp(MIN_UDP_BLKSIZE).is_ok());
            assert!(udp(MIN_UDP_BLKSIZE - 1).is_err());
            assert!(udp(MAX_UDP_BLKSIZE).is_ok());
            assert!(udp(MAX_UDP_BLKSIZE + 1).is_err());
        }

        #[test]
        fn build_tos_range_checked() {
            // IEBADTOS parity for the i32 setter path (#167).
            assert!(ClientBuilder::new("h").tos(255).build().is_ok());
            assert!(ClientBuilder::new("h").tos(256).build().is_err());
            assert!(ClientBuilder::new("h").tos(-1).build().is_err());
        }

        #[test]
        fn tos_str_parses_strtol_base0() {
            // -S accepts decimal/hex/octal like iperf3's strtol base 0 (#167).
            let c = ClientBuilder::new("h")
                .tos_str("0x20")
                .unwrap()
                .build()
                .unwrap();
            assert_eq!(c.tos, 0x20);
            assert!(ClientBuilder::new("h").tos_str("256").is_err());
        }

        #[test]
        fn bandwidth_str_applies_burst() {
            // The /burst count is no longer discarded (#160).
            let c = ClientBuilder::new("h")
                .bandwidth_str("100M/10")
                .unwrap()
                .build()
                .unwrap();
            assert_eq!(c.bandwidth, 100_000_000);
            assert_eq!(c.burst, 10);
            // IEBURST parity on the setter path too.
            assert!(ClientBuilder::new("h").burst(1001).build().is_err());
        }

        #[test]
        fn pacing_timer_str_enforces_i32_wire_cap() {
            // The wire TestParams field is i32; larger would wrap negative
            // (review r1 of #32; coverage restored per #193 review r1 n2).
            assert!(ClientBuilder::new("h").pacing_timer_str("3G").is_err());
            assert!(ClientBuilder::new("h")
                .pacing_timer_str("2147483647")
                .is_ok());
            assert!(ClientBuilder::new("h")
                .pacing_timer_str("2147483648")
                .is_err());
        }

        #[test]
        fn pacing_timer_str_accepts_kmg() {
            // iperf3 parses --pacing-timer with unit_atoi (1024-based) (#160).
            let c = ClientBuilder::new("h")
                .pacing_timer_str("1K")
                .unwrap()
                .build()
                .unwrap();
            assert_eq!(c.pacing_timer, 1024);
        }

        #[test]
        fn client_builder_bandwidth() {
            let c = ClientBuilder::new("h")
                .bandwidth(1_000_000)
                .build()
                .unwrap();
            assert_eq!(c.bandwidth, 1_000_000);
        }

        #[test]
        fn client_builder_tos() {
            let c = ClientBuilder::new("h").tos(0x10).build().unwrap();
            assert_eq!(c.tos, 0x10);
        }

        // Congestion is a Linux/FreeBSD feature (net.rs); gate to match (#76).
        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        #[test]
        fn client_builder_congestion() {
            let c = ClientBuilder::new("h").congestion("bbr").build().unwrap();
            assert_eq!(c.congestion, Some("bbr".to_string()));
        }

        #[test]
        fn client_builder_udp_64bit() {
            let c = ClientBuilder::new("h")
                .udp_counters_64bit(true)
                .build()
                .unwrap();
            assert!(c.udp_counters_64bit);
        }

        #[test]
        fn client_builder_connect_timeout() {
            let c = ClientBuilder::new("h")
                .connect_timeout(Duration::from_millis(500))
                .build()
                .unwrap();
            assert_eq!(c.connect_timeout, Some(Duration::from_millis(500)));
        }

        #[test]
        fn client_builder_title() {
            let c = ClientBuilder::new("h").title("my test").build().unwrap();
            assert_eq!(c.title, Some("my test".to_string()));
        }

        #[test]
        fn client_builder_extra_data() {
            let c = ClientBuilder::new("h").extra_data("x").build().unwrap();
            assert_eq!(c.extra_data, Some("x".to_string()));
        }

        #[test]
        fn client_builder_verbose() {
            let c = ClientBuilder::new("h").verbose(true).build().unwrap();
            assert!(c.verbose);
        }

        #[test]
        fn client_builder_json_output() {
            let c = ClientBuilder::new("h").json_output(true).build().unwrap();
            assert!(c.json_output);
        }

        #[test]
        fn client_builder_bytes() {
            let c = ClientBuilder::new("h").bytes(1_000_000).build().unwrap();
            assert_eq!(c.bytes_to_send, Some(1_000_000));
        }

        #[test]
        fn client_builder_blocks() {
            let c = ClientBuilder::new("h").blocks(100).build().unwrap();
            assert_eq!(c.blocks_to_send, Some(100));
        }

        #[test]
        fn client_builder_format_char() {
            // -f format is wired to Client.format_char and used in the reporter.
            let c = ClientBuilder::new("h").format_char('k').build().unwrap();
            assert_eq!(c.format_char, 'k');
        }

        #[test]
        fn client_builder_mptcp() {
            let c = ClientBuilder::new("h").mptcp(true).build().unwrap();
            assert!(c.mptcp);
        }

        #[test]
        fn client_builder_dscp_maps_to_tos() {
            // --dscp folds into the TOS byte: EF (46) << 2 == 184. The end-to-end
            // run lives in tests/integration.rs; here we pin the mapping precisely.
            let c = ClientBuilder::new("h").dscp("ef").build().unwrap();
            assert_eq!(c.tos, 46 << 2);
        }
    }

    mod client_builder_tests {
        use super::*;

        #[test]
        fn test_client_builder_default() {
            let b = ClientBuilder::default();
            assert_eq!(b.host, None);
            assert_eq!(b.port, Some(DEFAULT_PORT));
        }

        #[test]
        fn test_client_builder_new() {
            let b = ClientBuilder::new("localhost");
            assert_eq!(b.host, Some("localhost".to_string()));
            assert_eq!(b.port, Some(DEFAULT_PORT));
        }

        #[test]
        fn test_client_builder_host() {
            let b = ClientBuilder::new("localhost").host("otherhost");
            assert_eq!(b.host, Some("otherhost".to_string()));
        }

        #[test]
        fn test_client_builder_port() {
            let b = ClientBuilder::new("localhost").port(Some(1234));
            assert_eq!(b.port, Some(1234));
        }

        #[test]
        fn test_client_builder_build() {
            let r = ClientBuilder::default().build();
            assert!(r.is_err());
            assert_eq!(r.unwrap_err(), ConfigError::MissingField("host"));

            let c = ClientBuilder::new("localhost").build().unwrap();
            assert_eq!(c.host, "localhost");
            assert_eq!(c.port, DEFAULT_PORT);

            let c = ClientBuilder::new("localhost")
                .host("otherhost")
                .port(Some(1234))
                .build()
                .unwrap();
            assert_eq!(c.host, "otherhost");
            assert_eq!(c.port, 1234);
        }

        #[test]
        fn test_client_builder_all_fields() {
            let c = ClientBuilder::new("10.0.0.1")
                .protocol(TransportProtocol::Udp)
                .duration(30)
                .num_streams(4)
                .blksize(1460)
                .reverse(true)
                .bidir(false)
                .no_delay(true)
                .bandwidth(100_000_000)
                .tos(0x10)
                .verbose(true)
                .build()
                .unwrap();

            assert_eq!(c.protocol, TransportProtocol::Udp);
            assert_eq!(c.duration, 30);
            assert_eq!(c.num_streams, 4);
            assert_eq!(c.blksize, 1460);
            assert!(c.reverse);
            assert!(!c.bidir);
            assert!(c.no_delay);
            assert_eq!(c.bandwidth, 100_000_000);
            assert_eq!(c.tos, 0x10);
        }

        // -- UDP -b 0 = unlimited (issue #17) --

        #[test]
        fn udp_unset_bandwidth_defaults_to_1m() {
            // No -b on UDP resolves to the 1 Mbit/s default (iperf3 parity),
            // now resolved at build time rather than in the sender.
            let c = ClientBuilder::new("h")
                .protocol(TransportProtocol::Udp)
                .build()
                .unwrap();
            assert_eq!(c.bandwidth, DEFAULT_UDP_RATE);
        }

        #[test]
        fn udp_explicit_zero_bandwidth_is_unlimited() {
            // -b 0 means unlimited (0), NOT the 1 Mbit/s default (#17).
            let c = ClientBuilder::new("h")
                .protocol(TransportProtocol::Udp)
                .bandwidth(0)
                .build()
                .unwrap();
            assert_eq!(c.bandwidth, 0);
        }

        #[test]
        fn tcp_unset_bandwidth_is_unlimited() {
            // TCP default stays unlimited (0).
            let c = ClientBuilder::new("h").build().unwrap();
            assert_eq!(c.protocol, TransportProtocol::Tcp);
            assert_eq!(c.bandwidth, 0);
        }

        #[test]
        fn udp_build_params_carries_bandwidth_including_zero() {
            // The negotiated rate must reach the server (in `len`/`bandwidth`)
            // so reverse-mode -b 0 is unlimited server-side, not throttled.
            let c = ClientBuilder::new("h")
                .protocol(TransportProtocol::Udp)
                .bandwidth(0)
                .build()
                .unwrap();
            assert_eq!(c.build_params(1460).bandwidth, Some(0));
        }

        // #32: iperf3 ALWAYS sends pacing_timer in the param exchange (default
        // 1000 µs), so the server's reverse/bidir sender paces on the same
        // quantum. riperf3 left it unset.
        #[test]
        fn build_params_sends_burst_only_when_set() {
            // iperf3 gates the param on nonzero (`if (test->settings->burst)`,
            // iperf_api.c:2461) — absent otherwise, so the wire JSON is
            // byte-identical for every burst-less invocation (#160 review r2 n4).
            let c = ClientBuilder::new("h")
                .bandwidth_str("100M/10")
                .unwrap()
                .build()
                .unwrap();
            assert_eq!(c.build_params(1460).burst, Some(10));
            let c = ClientBuilder::new("h").build().unwrap();
            assert_eq!(c.build_params(1460).burst, None);
        }

        #[test]
        fn build_params_always_sends_pacing_timer() {
            let c = ClientBuilder::new("h").build().unwrap();
            assert_eq!(c.build_params(1460).pacing_timer, Some(1000));
            let c = ClientBuilder::new("h").pacing_timer(500).build().unwrap();
            assert_eq!(c.build_params(1460).pacing_timer, Some(500));
        }

        // -- end-block peer halves (issue #25 generalized by #184) --

        #[test]
        fn forward_udp_surfaces_server_receiver_loss() {
            // Forward UDP: the client is the sender, so the receiver's loss lives
            // only in the server's results. riperf3 must surface it as a receiver
            // line, like iperf3 — otherwise forward looks artificially loss-free
            // even when the link drops packets (issue #25).
            let x = protocol::StreamResultJson {
                id: 1,
                bytes: 2_000_000,
                retransmits: -1,
                jitter: 0.000_03,
                errors: 4258,
                omitted_errors: 0,
                packets: 267_190,
                omitted_packets: 0,
                start_time: 0.0,
                end_time: 5.0,
            };

            let recv = peer_half_summary(&x, true, true, false, 5.0, None);
            assert!(!recv.is_sender, "server is the receiver in forward mode");
            assert_eq!(recv.lost, Some(4258));
            assert_eq!(recv.total_packets, Some(267_190));
            assert_eq!(recv.jitter, Some(0.000_03));

            // Renders as a receiver line carrying the loss iperf3 would print.
            let line = crate::reporter::format_summary_line(&recv, 'a');
            assert!(line.contains("receiver"), "{line}");
            assert!(line.contains("4258/267190"), "{line}");

            // TCP forward: no datagram-loss columns, just a receiver byte line.
            let tcp = peer_half_summary(&x, true, false, false, 5.0, None);
            assert_eq!(tcp.lost, None);
            assert_eq!(tcp.total_packets, None);
            assert_eq!(tcp.jitter, None);
        }

        #[test]
        fn peer_sender_half_carries_sent_total_not_measured_stats() {
            // Reverse/bidir: the peer SENT this stream — its pair line is a
            // sender line with zero jitter/loss over the sent count (#184),
            // exactly iperf3's sender-line convention.
            let x = protocol::StreamResultJson {
                id: 3,
                bytes: 250_000,
                retransmits: -1,
                jitter: 0.5, // a peer sender reports no meaningful jitter
                errors: 0,
                omitted_errors: 0,
                packets: 30,
                omitted_packets: 5,
                start_time: 0.0,
                end_time: 5.0,
            };
            let snd = peer_half_summary(&x, false, true, false, 5.0, Some("RX-C"));
            assert!(snd.is_sender, "peer half of a local receiver is the sender");
            assert_eq!(snd.jitter, Some(0.0), "sender line shows zero jitter");
            assert_eq!(snd.lost, Some(0), "sender line shows zero loss");
            assert_eq!(snd.total_packets, Some(25), "post-omit sent count (#31)");
            let line = crate::reporter::format_summary_line(&snd, 'a');
            assert!(line.contains("sender") && line.contains("RX-C"), "{line}");
        }

        // -- client -B vs -4/-6 build-time validation (issue #15) --

        #[test]
        fn bind_address_family_conflict_rejected_at_build() {
            // A -B literal contradicting -4/-6 is rejected at config time,
            // mirroring the server (#12); matching families build fine (#15).
            assert!(ClientBuilder::new("h")
                .ip_version(6)
                .bind_address("10.0.0.1")
                .build()
                .is_err());
            assert!(ClientBuilder::new("h")
                .ip_version(4)
                .bind_address("::1")
                .build()
                .is_err());
            assert!(ClientBuilder::new("h")
                .ip_version(4)
                .bind_address("10.0.0.1")
                .build()
                .is_ok());
        }
    }
}

// ---------------------------------------------------------------------------
// Client::run return-value error path (migrated in-crate from tests/integration.rs, #67)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod client_run_return_value {
    use crate::ClientBuilder;
    use crate::RiperfError;

    /// Error path: a server that ends the session via `IperfDone` without an
    /// `ExchangeResults` round now yields `Protocol("missing server results...")`
    /// instead of the previous `Ok(())`. Uses a mock TCP server because the real
    /// riperf3 server always performs `ExchangeResults`.
    #[tokio::test]
    async fn run_errors_when_server_skips_results_exchange() {
        use crate::protocol::{self, TestState};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server_task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut cookie = [0u8; 37];
            tokio::io::AsyncReadExt::read_exact(&mut stream, &mut cookie)
                .await
                .unwrap();
            // Skip ExchangeResults / DisplayResults entirely.
            protocol::send_state(&mut stream, TestState::IperfDone)
                .await
                .unwrap();
        });

        let client = ClientBuilder::new("127.0.0.1")
            .port(Some(addr.port()))
            .duration(1)
            .build()
            .unwrap();
        let err = client
            .run()
            .await
            .expect_err("expected missing-results error");
        match err {
            RiperfError::Protocol(msg) => assert!(
                msg.contains("missing server results"),
                "unexpected protocol message: {msg}"
            ),
            other => panic!("expected RiperfError::Protocol, got {other:?}"),
        }

        let _ = server_task.await;
    }

    /// End-to-end abort sanity: a mid-test `ServerTerminate` aborts `run()`
    /// and the data flow stops promptly. NOTE this does NOT discriminate the
    /// #147 fix — run()'s DoneOnDrop guard stops the senders on any exit; the
    /// real pre-fix leak (a parked reporter task) is pinned by
    /// `abort_path_joins_the_reporter` in the in-module tests.
    /// #170 T1: the control connection DYING mid-test (duration mode) must
    /// surface as ControlSocketClosed promptly — iperf3's select observes the
    /// EOF immediately and errexits with IECTRLCLOSE. Pre-fix the recv_state
    /// arm swallowed the error, "completed" the test at the full -t, and the
    /// failure surfaced (late) as a broken-pipe Io from the TestEnd write.
    #[tokio::test]
    async fn control_death_mid_test_is_control_socket_closed() {
        use crate::protocol::{self, TestState};
        use tokio::io::AsyncReadExt;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server_task = tokio::spawn(async move {
            let (mut ctrl, _) = listener.accept().await.unwrap();
            let mut cookie = [0u8; 37];
            ctrl.read_exact(&mut cookie).await.unwrap();
            protocol::send_state(&mut ctrl, TestState::ParamExchange)
                .await
                .unwrap();
            let _params = protocol::recv_params(&mut ctrl).await.unwrap();
            protocol::send_state(&mut ctrl, TestState::CreateStreams)
                .await
                .unwrap();
            let (data, _) = listener.accept().await.unwrap();
            protocol::send_state(&mut ctrl, TestState::TestStart)
                .await
                .unwrap();
            protocol::send_state(&mut ctrl, TestState::TestRunning)
                .await
                .unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            drop(ctrl); // control socket dies mid-test
                        // Hold the data socket a beat so the death is unambiguous.
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            drop(data);
        });

        let client = ClientBuilder::new("127.0.0.1")
            .port(Some(addr.port()))
            .duration(10)
            .build()
            .unwrap();
        let err = tokio::time::timeout(std::time::Duration::from_secs(5), client.run())
            .await
            .expect("must fail promptly, not run out the full -t")
            .expect_err("control death is an error");
        assert!(
            matches!(err, RiperfError::ControlSocketClosed),
            "iperf3's IECTRLCLOSE class, got {err:?}"
        );
        let _ = server_task.await;
    }

    /// #170 T3: -n/--bytes mode had NO control watch at all — a dead server
    /// stalled the byte-limit poll forever. Pre-fix this test times out.
    #[tokio::test]
    async fn bytes_mode_watches_the_control_socket() {
        use crate::protocol::{self, TestState};
        use tokio::io::AsyncReadExt;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server_task = tokio::spawn(async move {
            let (mut ctrl, _) = listener.accept().await.unwrap();
            let mut cookie = [0u8; 37];
            ctrl.read_exact(&mut cookie).await.unwrap();
            protocol::send_state(&mut ctrl, TestState::ParamExchange)
                .await
                .unwrap();
            let _params = protocol::recv_params(&mut ctrl).await.unwrap();
            protocol::send_state(&mut ctrl, TestState::CreateStreams)
                .await
                .unwrap();
            let (data, _) = listener.accept().await.unwrap();
            protocol::send_state(&mut ctrl, TestState::TestStart)
                .await
                .unwrap();
            protocol::send_state(&mut ctrl, TestState::TestRunning)
                .await
                .unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            drop(ctrl);
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            drop(data); // never read: the byte budget can't complete
        });

        let client = ClientBuilder::new("127.0.0.1")
            .port(Some(addr.port()))
            .bytes(1024 * 1024 * 1024) // far beyond what the mock drains
            .build()
            .unwrap();
        let err = tokio::time::timeout(std::time::Duration::from_secs(8), client.run())
            .await
            .expect("-n mode must observe control death (pre-fix: hangs)")
            .expect_err("control death is an error");
        assert!(
            matches!(err, RiperfError::ControlSocketClosed),
            "got {err:?}"
        );
        let _ = server_task.await;
    }

    /// #170 T2: ServerTerminate mid-test still renders a summary from the
    /// partial local data — iperf3 flips to DISPLAY_RESULTS before erroring
    /// with IESERVERTERM ("the server has terminated").
    #[tokio::test]
    async fn server_terminate_renders_partial_summary() {
        use crate::protocol::{self, TestState};
        use tokio::io::AsyncReadExt;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server_task = tokio::spawn(async move {
            let (mut ctrl, _) = listener.accept().await.unwrap();
            let mut cookie = [0u8; 37];
            ctrl.read_exact(&mut cookie).await.unwrap();
            protocol::send_state(&mut ctrl, TestState::ParamExchange)
                .await
                .unwrap();
            let _params = protocol::recv_params(&mut ctrl).await.unwrap();
            protocol::send_state(&mut ctrl, TestState::CreateStreams)
                .await
                .unwrap();
            let (data, _) = listener.accept().await.unwrap();
            protocol::send_state(&mut ctrl, TestState::TestStart)
                .await
                .unwrap();
            protocol::send_state(&mut ctrl, TestState::TestRunning)
                .await
                .unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            protocol::send_state(&mut ctrl, TestState::ServerTerminate)
                .await
                .unwrap();
            // Hold both sockets open; the client returns on its own.
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            drop((ctrl, data));
        });

        // The capture guard tees every titled() report line (process-global;
        // contains()-tolerant assertions below).
        let capture = crate::macros::OutputCaptureGuard::start();
        let client = ClientBuilder::new("127.0.0.1")
            .port(Some(addr.port()))
            .duration(10)
            .build()
            .unwrap();
        let err = client.run().await.expect_err("expected ServerTerminated");
        let printed = capture.take();
        assert!(
            matches!(err, RiperfError::ServerTerminated),
            "iperf3's IESERVERTERM class, got {err:?}"
        );
        assert!(
            printed.contains("sender"),
            "a partial summary must render from local data (iperf3 flips to \
             DISPLAY_RESULTS); captured: {printed:?}"
        );
        assert!(
            printed.contains("receiver"),
            "the missing peer half renders ZEROED, like iperf3's client \
             (review r1 n1) — never collapsed away: {printed:?}"
        );
        let _ = server_task.await;
    }

    #[tokio::test]
    async fn server_terminate_stops_senders_and_reporter() {
        use crate::protocol::{self, TestState};
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::sync::Arc;
        use tokio::io::AsyncReadExt;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let drained = Arc::new(AtomicU64::new(0));
        let drained_srv = drained.clone();
        let (err_tx, err_rx) = tokio::sync::oneshot::channel::<()>();

        let server_task = tokio::spawn(async move {
            let (mut ctrl, _) = listener.accept().await.unwrap();
            let mut cookie = [0u8; 37];
            ctrl.read_exact(&mut cookie).await.unwrap();
            protocol::send_state(&mut ctrl, TestState::ParamExchange)
                .await
                .unwrap();
            let _params = protocol::recv_params(&mut ctrl).await.unwrap();
            protocol::send_state(&mut ctrl, TestState::CreateStreams)
                .await
                .unwrap();
            let (mut data, _) = listener.accept().await.unwrap();
            let mut dcookie = [0u8; 37];
            data.read_exact(&mut dcookie).await.unwrap();
            protocol::send_state(&mut ctrl, TestState::TestStart)
                .await
                .unwrap();
            protocol::send_state(&mut ctrl, TestState::TestRunning)
                .await
                .unwrap();
            // Let the test run briefly, then terminate mid-test.
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            protocol::send_state(&mut ctrl, TestState::ServerTerminate)
                .await
                .unwrap();
            // Wait until run() has returned, then measure post-abort flow.
            let _ = err_rx.await;
            let mut buf = vec![0u8; 64 * 1024];
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(800);
            loop {
                tokio::select! {
                    r = data.read(&mut buf) => match r {
                        Ok(0) | Err(_) => break,
                        Ok(n) => { drained_srv.fetch_add(n as u64, Ordering::Relaxed); }
                    },
                    _ = tokio::time::sleep_until(deadline) => break,
                }
            }
        });

        let client = ClientBuilder::new("127.0.0.1")
            .port(Some(addr.port()))
            .duration(10)
            .build()
            .unwrap();
        let err = client.run().await.expect_err("expected ServerTerminated");
        assert!(
            matches!(err, RiperfError::ServerTerminated),
            "IESERVERTERM class since #170, got {err:?}"
        );
        // Kernel socket buffers legitimately hold a few MB in flight on
        // loopback; the LEAK signature is continued line-rate production
        // (hundreds of MB over the 800 ms drain window). 64 MB cleanly
        // separates the two: pre-fix this reads GBs, post-fix single-digit MB.
        let _ = err_tx.send(());
        let _ = server_task.await;
        let post = drained.load(Ordering::Relaxed);
        assert!(
            post <= 64 * 1024 * 1024,
            "senders still producing after run() returned (#147 leak): {post} bytes post-abort"
        );
    }
}
