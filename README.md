# riperf3

A ground-up Rust implementation of [iperf3](https://github.com/esnet/iperf), the standard network performance measurement tool. Wire-compatible with ESNet's iperf3 — a riperf3 client talks to an iperf3 server and vice versa.

## Highlights

- **Wire-protocol compatible** with iperf3. Passes interchange tests in both directions at full line rate.
- **TCP and UDP** throughput testing with parallel streams, reverse mode, and bidirectional mode.
- **Single static binary** with no runtime dependencies.
- **Idiomatic Rust** — not a C port. Uses tokio for async I/O, serde for JSON, clap for CLI parsing.

## Quick Start

```bash
# Build
cargo build --release

# Server
./target/release/riperf3 -s

# Client (from another machine)
./target/release/riperf3 -c <server-host>

# Common options
./target/release/riperf3 -c <host> -t 30          # 30-second test
./target/release/riperf3 -c <host> -P 4           # 4 parallel streams
./target/release/riperf3 -c <host> -R             # reverse mode (server sends)
./target/release/riperf3 -c <host> --bidir        # bidirectional
./target/release/riperf3 -c <host> -u -b 1G       # UDP at 1 Gbps
./target/release/riperf3 -c <host> -J             # JSON output
```

## Interchange Compatibility

riperf3 is wire-compatible with iperf3. You can freely mix clients and servers:

```bash
# riperf3 server, iperf3 client
riperf3 -s
iperf3 -c <server>

# iperf3 server, riperf3 client
iperf3 -s
riperf3 -c <server>
```

Verified across TCP normal/reverse/bidir, parallel streams, and UDP modes.

## CLI Reference

```
Usage: riperf3 [OPTIONS] <--server|--client <host>>

Options:
  -s, --server                  Run in server mode
  -c, --client <host>           Run in client mode
  -p, --port <PORT>             Server port (default: 5201)
  -u, --udp                     Use UDP instead of TCP
  -t, --time <secs>             Test duration (default: 10s)
  -n, --bytes <N[KMG]>          Bytes to transmit (instead of -t)
  -k, --blockcount <N[KMG]>     Blocks to transmit (instead of -t)
  -l, --length <N[KMG]>         Read/write buffer size (default: 128K TCP, 1460 UDP)
  -P, --parallel <N>            Parallel streams (default: 1)
  -R, --reverse                 Server sends, client receives
      --bidir                   Bidirectional test
  -b, --bitrate <N[KMG][/#]>   Target bitrate (default: unlimited TCP, 1M UDP)
  -w, --window <N[KMG]>        Socket buffer size
  -N, --no-delay                Set TCP_NODELAY
  -M, --set-mss <N>            TCP maximum segment size
  -C, --congestion <algo>       TCP congestion control algorithm
  -S, --tos <N>                 IP type of service
  -O, --omit <N>                Omit first N seconds
  -J, --json                    JSON output
  -1, --one-off                 Handle one client then exit (server)
  -T, --title <str>             Output line prefix
  -V, --verbose                 Verbose output
  -v, --version                 Print version
  -h, --help                    Print help
```

## Project Structure

Cargo workspace with two crates:

```
riperf3/          Core library
  src/
    protocol.rs   Wire protocol: cookie, state machine, JSON framing
    net.rs        Async TCP/UDP helpers (socket2 for pre-configuration)
    stream.rs     Data stream I/O, counters, UDP header, rate limiting
    client.rs     Client protocol state machine
    server.rs     Server protocol state machine
    reporter.rs   Human-readable and JSON output formatting
    units.rs      Byte/bit unit formatting (adaptive, fixed)
    tcp_info.rs   Linux TCP_INFO via getsockopt
    cpu.rs        CPU utilization via getrusage
    error.rs      Error types
    utils.rs      Constants, KMG parser
  tests/
    integration.rs  Full client-server loopback tests

riperf3-cli/      CLI binary
  src/
    cli.rs        clap argument definitions
    main.rs       CLI-to-library wiring
```

## Building and Testing

```bash
cargo build --release          # optimized binary at target/release/riperf3
cargo test                     # unit + integration tests
cargo clippy --all-targets -- -D warnings   # lint
```

## Status

Early but functional. TCP and UDP throughput testing works end-to-end with full iperf3 interchange compatibility. Key areas for future work:

- [ ] Interval reporting during test (currently only final summary)
- [ ] Zero-copy send mode (`-Z`)
- [ ] Daemon mode (`-D`)
- [ ] SCTP support
- [ ] Authentication (RSA)
- [ ] `libiperf` FFI compatibility library

## License

Dual-licensed under [MIT](LICENSE-MIT.txt) and [Apache 2.0](LICENSE-APACHE.txt). Choose whichever you prefer.

## Contributing

Contributions welcome. Please open an issue or reach out to [@therealevanhenry](https://github.com/therealevanhenry).
