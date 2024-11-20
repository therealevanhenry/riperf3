# riperf3
A Rust implementation of [iperf3](https://github.com/esnet/iperf).

## Overview

**riperf3** is a high-performance network testing tool written in Rust, designed to replicate and enhance the functionality of the widely-used [iperf3](https://github.com/esnet/iperf) tool. Leveraging Rust's safety and concurrency features, `riperf3` aims to provide a reliable and efficient alternative for measuring network bandwidth and performance.

## Project Goals

`riperf3` is designed with the following objectives:

1. **Feature Parity:** Strive to match the feature set of `iperf3`, ensuring users have access to familiar functionalities.
2. **Rust Best Practices:** Implement `riperf3` following Rust's development best practices, emphasizing safety, concurrency, and performance.
3. **Command-Line Interface:** Maintain the same CLI syntax as `iperf3` to facilitate ease-of-use for existing `iperf3` users.
4. **Interoperability:**
    - **Integration:** Support full integration with `iperf3` clients and servers, regardless of their implementation language.
    - **FFI Support:** Provide a Foreign Function Interface (FFI) library that matches the API of `libiperf`, allowing `riperf3` to serve as a complete drop-in replacement.

## (Planned) Features

- **Client and Server Modes:** Operate in both client and server modes to initiate and receive network tests.
- **UDP and TCP Support:** Perform tests using either UDP or TCP protocols.
- **Bandwidth Configuration:** Customize bandwidth settings for UDP tests.
- **Parallel Streams:** Run multiple parallel client streams to simulate concurrent connections.
- **Single Trial Mode:** Execute the server in a single trial mode for specific testing scenarios.
- **Debugging Options:** Control verbosity with adjustable debug levels to aid in troubleshooting and performance tuning.
- **Cross-Platform Compatibility:** Run seamlessly on major operating systems including Linux, macOS, and Windows.

## Collaboration

This is a nascent project which is in need of contributors. Please reach out to [@therealevanhenry](https://github.com/therealevanhenry) if you wish to help out.

## License

`riperf3` is [dual-licensed](LICENSE.txt) under the [MIT License](LICENSE-MIT.txt) and the [Apache License 2.0](LICENSE-APACHE.txt). You may choose either license to govern your use of the software.

For more details, please refer to the respective license files included in this repository.
