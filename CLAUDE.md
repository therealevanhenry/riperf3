# riperf3

Rust implementation of iperf3 — a network performance testing tool. Goal is feature parity and CLI compatibility with ESNet's iperf3.

## Build & Test

```bash
cargo build --release        # binary at target/release/riperf3
cargo test                   # unit tests
cargo clippy --all-targets -- -D warnings   # lint (also: cargo make lint)
```

## Project Structure

Cargo workspace with two crates:
- `riperf3/` — core library (client, server, error types, utils)
- `riperf3-cli/` — CLI binary wrapping the library

Single binary, two modes:
- Server: `riperf3 -s` (listens on port 5201 by default)
- Client: `riperf3 -c <host>`

Key dependencies: tokio (async runtime), clap (CLI parsing), thiserror, log4rs.

## Status

Early stage. The `Client::run()` and `Server::run()` methods are stubs. Many CLI arguments are defined but not yet wired up (see TODOs in `riperf3-cli/src/cli.rs`). The project builds and passes its unit tests but does not yet perform actual network transfers.

## Reference Implementation

ESNet's iperf3 at `../iperf/` is the compatibility and performance target.

```bash
cd ../iperf && ./configure && make -j$(nproc)   # binary at src/iperf3
```

Always build both riperf3 and iperf3 from source. Never use distro packages — this ensures complete control over the build environment and guarantees reproducibility.

## Sandbox Testing

Two QEMU/KVM sandbox VMs are available on Gandalf for isolated build and interchange testing. Each VM has 8GB RAM and 8 vCPUs. These are lightweight Debian 12 cloud images provisioned with Rust, build-essential, autotools, and rsync.

### Quick Reference

| Sandbox | IPv4 | IPv6 | SSH |
|---|---|---|---|
| sandbox-server-1 | 172.20.0.20 | fd00:20::20 | `ssh sandbox-server-1` |
| sandbox-client-1 | 172.20.0.21 | fd00:20::21 | `ssh sandbox-client-1` |

Jumbo frames (MTU 9000) are auto-configured on all interfaces (host bridge, taps, and guest NICs via cloud-init). Baseline iperf3 throughput: ~70 Gbps normal, ~63 Gbps reverse, ~72 Gbps bidirectional aggregate.

### Lifecycle (fish shell on the host)

```fish
sandbox list               # show status
sandbox start <name>       # boot VM, wait for SSH
sandbox stop <name>        # graceful shutdown
sandbox reset <name>       # destroy + recreate from golden image
```

### Interchange Testing Workflow

Edit code on the host, then build and test in sandboxes:

1. Start sandboxes:
   ```bash
   sandbox start sandbox-server-1
   sandbox start sandbox-client-1
   ```

2. Sync source to both VMs:
   ```bash
   rsync -az ~/workspace/therealevanhenry/riperf3/ sandbox-server-1:~/riperf3/
   rsync -az ~/workspace/therealevanhenry/riperf3/ sandbox-client-1:~/riperf3/
   rsync -az ~/workspace/therealevanhenry/iperf/ sandbox-server-1:~/iperf/
   rsync -az ~/workspace/therealevanhenry/iperf/ sandbox-client-1:~/iperf/
   ```

3. Build on both:
   ```bash
   ssh sandbox-server-1 'cd ~/iperf && ./configure && make -j$(nproc)'
   ssh sandbox-server-1 'cd ~/riperf3 && cargo build --release'
   ssh sandbox-client-1 'cd ~/iperf && ./configure && make -j$(nproc)'
   ssh sandbox-client-1 'cd ~/riperf3 && cargo build --release'
   ```

4. Run interchange tests (server is 172.20.0.20):
   - **Baseline:** `iperf3 -s` on server, `iperf3 -c 172.20.0.20 -t 10 -J` on client
   - **Interchange A:** `riperf3 -s` on server, `iperf3 -c 172.20.0.20 -J` on client
   - **Interchange B:** `iperf3 -s` on server, `riperf3 -c 172.20.0.20` on client
   - **Native:** `riperf3 -s` on server, `riperf3 -c 172.20.0.20` on client

5. Compare throughput/CPU, identify optimization targets.

6. Clean up: `sandbox stop sandbox-server-1 && sandbox stop sandbox-client-1`

### Slash Commands

- `/sandbox-provision` — provisions both sandboxes from scratch (cloud-init wait, sync source, build iperf3 + riperf3, run tests)
- `/sandbox-benchmark` — runs the iperf3 benchmark suite (normal, reverse, bidirectional) and reports results

For detailed VM infrastructure docs, see `~/virtual-machines/CLAUDE.md`.
