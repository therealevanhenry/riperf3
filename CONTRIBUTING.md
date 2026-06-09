# Contributing to riperf3

Thanks for your interest! riperf3 is a faithful, wire-compatible iperf3 drop-in — the ethos is **fidelity first**: where iperf3 accepts an option, riperf3 implements it to behave the same way (quirks included); it never rejects, renames, or works around iperf3's semantics. Behavioral claims in PRs should cite the iperf3 source (file/function) or a live run against a real iperf3 binary.

## Workflow

1. **Open or pick an issue first** for anything non-trivial, so scope is agreed before code.
2. Branch from `main` (it is protected: PRs only, required CI, linear history — squash merges).
3. **Tests before fixes**: write a failing test that captures the defect or missing behavior, commit it (red), then make it pass (green). Where the behavior genuinely can't be exercised in CI (e.g. routing effects invisible on loopback), say so in the PR and name the external verification you ran.
4. Keep PRs tightly scoped — one issue (or one tight cluster) per PR.

## Before pushing

```bash
cargo fmt --check
cargo clippy --workspace --all-targets   # CI denies warnings
cargo test --workspace                   # runs every integration binary, not just --test integration
```

If you touched `#[cfg(...)]`-gated or platform code, cross-check every supported target compiles:

```bash
for t in x86_64-unknown-linux-gnu x86_64-unknown-linux-musl x86_64-pc-windows-msvc \
         x86_64-apple-darwin aarch64-apple-darwin x86_64-unknown-freebsd x86_64-unknown-netbsd; do
  rustup target add "$t" >/dev/null 2>&1
  cargo check --workspace --all-targets --target "$t" || break
done
```

## CI gates

Required: Linux full suite, FreeBSD native suite, lint (fmt + clippy -D warnings), SemVer (cargo-semver-checks), cross-platform `cargo check`s, and a **real-iperf3 interop matrix** (current + 3.12). macOS/Windows native suites run informationally. The lib crate is API-only (no clap); CLI↔builder wiring is tested by comparing built `Client`s, not by lib test backdoors.

## Releases

SemVer (0.y.z: y = breaking, z = additive/fixes), `cargo-semver-checks` gated, changelog written at release prep, `riperf3` (lib) publishes before `riperf3-cli`.

## Security

See [SECURITY.md](SECURITY.md) — please do not open public issues for vulnerabilities.
