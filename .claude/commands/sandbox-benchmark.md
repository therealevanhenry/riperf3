Run the iperf3 benchmark suite between sandbox VMs and report results.

Both sandboxes must be running and provisioned (use /sandbox-provision first if needed).

Run three iperf3 tests between sandbox-server-1 (172.20.0.20) and sandbox-client-1, using the source-built iperf3 at ~/iperf/src/iperf3. Each test runs for 5 seconds with JSON output (`-J`).

1. **Normal mode** (client sends to server):
   - Start server: `ssh sandbox-server-1 'cd ~/iperf && ./src/iperf3 -s -1 -D'`
   - Wait 1 second, then run client: `ssh sandbox-client-1 'cd ~/iperf && ./src/iperf3 -c 172.20.0.20 -t 5 -J'`

2. **Reverse mode** (server sends to client):
   - Start server: `ssh sandbox-server-1 'cd ~/iperf && ./src/iperf3 -s -1 -D'`
   - Wait 1 second, then run client: `ssh sandbox-client-1 'cd ~/iperf && ./src/iperf3 -c 172.20.0.20 -t 5 -R -J'`

3. **Bidirectional mode**:
   - Start server: `ssh sandbox-server-1 'cd ~/iperf && ./src/iperf3 -s -1 -D'`
   - Wait 1 second, then run client: `ssh sandbox-client-1 'cd ~/iperf && ./src/iperf3 -c 172.20.0.20 -t 5 --bidir -J'`

Parse the JSON output from each test and report a summary table with:
- Throughput (Gbps) for sender and receiver
- Retransmits
- CPU utilization (host and remote)
- Protocol confirmation (should be TCP)

Expected baseline with jumbo frames (MTU 9000): ~70 Gbps normal, ~63 Gbps reverse, ~72 Gbps bidir aggregate.
