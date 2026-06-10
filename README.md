# riperf3

A ground-up, idiomatic Rust implementation of [iperf3](https://github.com/esnet/iperf), the standard network performance measurement tool — not a C port or a binding, but a faithful reimplementation in safe, async Rust.

riperf3 speaks iperf3's exact wire protocol: a riperf3 client interoperates with an iperf3 server, and vice versa, across every mode. Fidelity is the guiding principle — riperf3 matches iperf3 flag for flag and quirk for quirk rather than reinventing the interface. Where iperf3 accepts an option, riperf3 implements it to behave the same way; it does not reject, rename, or work around iperf3's semantics. The goal is a drop-in you can swap in without your scripts, dashboards, or muscle memory noticing.

## Highlights

- **Wire-protocol compatible** with iperf3. Passes interchange tests in both directions across all modes.
- **Comprehensive flag support** — 60+ flags covering TCP, UDP, parallel streams, reverse/bidir, zerocopy, GSO/GRO, RSA authentication, IPv6, MPTCP, and more.
- **Safe Rust** — `unsafe` is used only for platform-specific kernel syscalls (`setsockopt`/`getsockopt`) with no safe wrapper. No unsafe in any application logic or public API. See the [audit table](https://github.com/therealevanhenry/riperf3/blob/main/riperf3/src/lib.rs) for the full inventory.
- **Single static binary** with no runtime dependencies.
- **Idiomatic Rust** — not a C port. Uses tokio for async I/O, serde for JSON, clap for CLI parsing, nix for safe Unix syscalls.
- **430+ tests** — unit, integration, and full client-server loopback with interchange verification.

## Quick Start

```bash
# Build
cargo build --release

# Server
./target/release/riperf3 -s

# Client (from another machine)
./target/release/riperf3 -c <server-host>
```

### Common Options

```bash
riperf3 -c <host> -t 30            # 30-second test
riperf3 -c <host> -P 4             # 4 parallel streams
riperf3 -c <host> -R               # reverse mode (server sends)
riperf3 -c <host> --bidir          # bidirectional
riperf3 -c <host> -u -b 10G        # UDP at 10 Gbps
riperf3 -c <host> -J               # JSON output
riperf3 -c <host> -Z               # zero-copy (sendfile)
riperf3 -c <host> -C bbr           # BBR congestion control
riperf3 -c <host> -6               # IPv6
riperf3 -c <host> -F /path/to/file # file transfer
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

Verified across TCP (normal, reverse, bidir, parallel, zerocopy, BBR, file mode), UDP (10G, 50G), IPv6, and RSA authentication.

## Performance

On a two-VM QEMU/KVM sandbox (virtio-net, MTU 9000, 8 vCPU), a 30-run-per-cell
campaign puts riperf3 **at or above iperf3** throughput across the board:

- **TCP** — near-parity single-stream (~75 Gbps; one marginal cell aside); a few percent ahead at `-P 8`.
- **UDP** — significantly faster in every cell (~+10–17%), and holds steady across `-P` instead of collapsing.
- **Wire-compatible** with iperf3 (current and 3.12) in both directions.

[**BENCHMARKS.md**](https://github.com/therealevanhenry/riperf3/blob/main/BENCHMARKS.md)
has the authoritative numbers — per-cell 95% confidence intervals, significance
tests, the compatibility matrix, and full methodology.

## Platform Support

riperf3 builds on Linux, macOS, FreeBSD, and Windows. Linux is the reference platform (full feature set; the required CI gate runs the full suite plus a real-iperf3 interop matrix). FreeBSD and Windows run the full native test suite as **required** CI checks that gate every merge. macOS runs the full native suite in CI as an informational check — tested on every change, promoted to required once consistently clean. Platform-specific features use safe Rust wrappers where available, with graceful degradation or a clear error message where a flag is unavailable.

| Feature | Linux | macOS | FreeBSD | Windows |
|---|:---:|:---:|:---:|:---:|
| TCP/UDP core | yes | yes | yes | yes |
| `-w` window size | yes | yes | yes | yes |
| `-N` no-delay | yes | yes | yes | yes |
| `-M` MSS | yes | yes | yes | yes |
| `-S` TOS | yes | yes | yes | yes |
| `-A` CPU affinity | yes | | yes | yes |
| `--dont-fragment` | yes | yes | yes | yes |
| `-Z` zerocopy (sendfile) | yes | yes | yes | |
| `-C` congestion control | yes | | yes | |
| `-D` daemon | yes | | yes | |
| `--bind-dev` | yes | yes | | |
| `--rcv-timeout` | yes | yes | yes | |
| `--cntl-ka` keepalive | yes | yes | yes | |
| TCP_INFO stats | yes | yes | yes | |
| `--snd-timeout` | yes | | | |
| `--fq-rate` pacing | yes | | | |
| `--flowlabel` IPv6 | yes | | | |
| `--gsro` UDP GSO/GRO | yes | | | |
| `--sendmmsg` batched UDP | yes | | yes | |

All platform-specific flags match iperf3's support matrix exactly for flags shared with iperf3. `--sendmmsg` is a riperf3-exclusive experimental optimization. Blank cells indicate the feature is unavailable on that platform in both riperf3 and iperf3. On Windows, unsupported flags return a clear error at startup; on Unix platforms a blank cell is a silent no-op at the socket layer, except `-D` and `--sendmmsg`, which error at startup wherever unsupported.

## CLI Reference

```
Usage: riperf3 [OPTIONS] <--server|--client <host>>

General:
  -s, --server                       Run in server mode
  -c, --client <host>                Run in client mode
  -p, --port <PORT>                  Server port (default: 5201)
  -f, --format <k|m|g|t>            Report format (default: Mbits)
  -i, --interval <secs>             Seconds between periodic reports
  -V, --verbose                      Verbose output
  -J, --json                         JSON output
      --json-stream                  Line-delimited JSON output
  -v, --version                      Print version
  -I, --pidfile <file>               Write PID to file
      --logfile <file>               Redirect output to log file
      --forceflush                   Flush output at every interval
      --timestamps [<format>]        Timestamp each output line

Server:
  -1, --one-off                      Handle one client then exit
  -D, --daemon                       Daemonize
      --idle-timeout <secs>          Restart idle server after N seconds
      --server-bitrate-limit <rate>  Server's total bitrate limit
      --server-max-duration <secs>   Max test duration on server

Test parameters:
  -u, --udp                          Use UDP instead of TCP
  -t, --time <secs>                  Test duration (default: 10)
  -n, --bytes <N[KMG]>              Bytes to transmit (instead of -t)
  -k, --blockcount <N[KMG]>         Blocks to transmit (instead of -t)
  -l, --length <N[KMG]>             Buffer size (default: 128K TCP; UDP tracks MSS, else 1460)
  -P, --parallel <N>                 Parallel streams
  -R, --reverse                      Server sends, client receives
      --bidir                        Bidirectional test
  -b, --bitrate <rate[/burst]>      Target bitrate (default: unlimited TCP, 1M UDP)
  -O, --omit <secs>                  Omit first N seconds

TCP options:
  -w, --window <N[KMG]>             Socket buffer size
  -C, --congestion <algo>            Congestion control algorithm (e.g., bbr)
  -M, --set-mss <N>                  Maximum segment size
  -N, --no-delay                     Set TCP_NODELAY
  -Z, --zerocopy                     Zero-copy sends via sendfile()
      --cntl-ka <idle/intv/cnt>     Control connection TCP keepalive

UDP options:
      --gsro                         Enable UDP GSO/GRO
      --sendmmsg                     Batched UDP sends via sendmmsg (experimental)
      --udp-counters-64bit           Use 64-bit UDP counters
      --repeating-payload            Repeating pattern payload
      --dont-fragment                Set IPv4 Don't Fragment

Network:
  -4, --version4                     IPv4 only
  -6, --version6                     IPv6 only
  -B, --bind <host>                  Bind to address
      --bind-dev <dev>               Bind to device (SO_BINDTODEVICE)
      --cport <port>                 Bind to specific client port
      --fq-rate <rate>               Fair-queue socket pacing (bits/sec)
  -L, --flowlabel <N>               IPv6 flow label
  -S, --tos <N>                      IP type of service
      --dscp <val>                   DSCP value (0-63 or symbolic)
  -m, --mptcp                        Use MPTCP
      --connect-timeout <ms>         Control connection timeout
      --rcv-timeout <ms>             Receive idle timeout
      --snd-timeout <ms>             Unacknowledged TCP data timeout
      --skip-rx-copy                 Discard received data (MSG_TRUNC)

File mode:
  -F, --file <name>                  Transmit/receive a file

CPU affinity:
  -A, --affinity <n[,m]>            Pin to CPU core(s)

Authentication (RSA):
      --username <name>              Username for auth
      --rsa-public-key-path <file>   RSA public key (client)
      --rsa-private-key-path <file>  RSA private key (server)
      --authorized-users-path <file> Authorized users CSV (server)
      --time-skew-threshold <secs>   Auth timestamp tolerance
      --use-pkcs1-padding            Use PKCS#1 instead of OAEP

Misc:
  -T, --title <str>                  Prefix output lines
      --extra-data <str>             Extra data in JSON output
      --get-server-output            Get results from server
  -d, --debug [<level>]              Debug level 1-4
```

## Project Structure

```
riperf3/            Core library
  src/
    client.rs       Client protocol state machine
    server.rs       Server protocol state machine
    protocol.rs     Wire protocol: cookie, state machine, JSON framing
    net.rs          TCP/UDP socket helpers, socket options (nix + socket2)
    stream.rs       Data stream I/O, counters, rate limiting, zerocopy
    reporter.rs     Human-readable and JSON output formatting
    auth.rs         RSA authentication (OAEP/PKCS#1, credential validation)
    units.rs        Byte/bit unit formatting
    tcp_info.rs     TCP_INFO (Linux/FreeBSD) / TCP_CONNECTION_INFO (macOS)
    cpu.rs          CPU utilization via getrusage
    error.rs        Error types
    utils.rs        Constants, KMG parser, DSCP parser
  tests/
    integration.rs  Client-server loopback tests

riperf3-cli/        CLI binary
  src/
    cli.rs          clap argument definitions + wiring tests
    main.rs         CLI-to-library wiring, CPU affinity, pidfile/logfile
```

## Building and Testing

```bash
cargo build --release                          # optimized binary
cargo test --workspace                         # unit + integration tests
cargo clippy --all-targets -- -D warnings      # lint
```

## Status

Feature-complete for the core iperf3 flag set, with full interchange compatibility verified against real iperf3 (current and 3.12) in both directions across all modes. Linux (full suite + interop matrix), FreeBSD, and Windows (native suites) are required CI checks gating every merge; macOS runs the full native suite as informational CI. Platform-specific flags match iperf3's support matrix (see [Platform Support](#platform-support)).

See [CHANGELOG.md](CHANGELOG.md) for the release notes and current known issues — including a handful of options that are accepted but not yet fully effective.

Not yet implemented:
- SCTP transport
- `libiperf`-compatible FFI library

Experimental:
- `--sendmmsg` — batched UDP sends via `sendmmsg(2)`. Uses safe Rust only (nix wrapper). Available on Linux and FreeBSD (the send path is also written for NetBSD, but the crate doesn't yet build there — [#78](https://github.com/therealevanhenry/riperf3/issues/78)). Not part of iperf3 — a riperf3-exclusive optimization exploring safe Rust performance at the kernel boundary.

## License

Dual-licensed under [MIT](LICENSE-MIT.txt) and [Apache 2.0](LICENSE-APACHE.txt). Choose whichever you prefer.

## Contributing

Contributions welcome. Please open an issue or reach out to [@therealevanhenry](https://github.com/therealevanhenry).
