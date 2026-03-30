Provision both sandbox VMs for riperf3 interchange testing.

NOTE: The `sandbox` command is a fish shell function. Always invoke it via `fish -c 'sandbox ...'`.

1. Check sandbox status with `fish -c 'sandbox list'`. If not running, start them:
   ```
   fish -c 'sandbox start sandbox-server-1'
   fish -c 'sandbox start sandbox-client-1'
   ```

2. Wait for cloud-init to complete on both:
   ```
   ssh sandbox-server-1 'cloud-init status --wait'
   ssh sandbox-client-1 'cloud-init status --wait'
   ```

3. Sync source code to both VMs:
   ```
   rsync -az ~/workspace/therealevanhenry/riperf3/ sandbox-server-1:~/riperf3/ --exclude target
   rsync -az ~/workspace/therealevanhenry/riperf3/ sandbox-client-1:~/riperf3/ --exclude target
   rsync -az ~/workspace/therealevanhenry/iperf/ sandbox-server-1:~/iperf/ --exclude .git
   rsync -az ~/workspace/therealevanhenry/iperf/ sandbox-client-1:~/iperf/ --exclude .git
   ```

4. Build iperf3 from source on both:
   ```
   ssh sandbox-server-1 'cd ~/iperf && ./configure --quiet && make -j$(nproc) --quiet'
   ssh sandbox-client-1 'cd ~/iperf && ./configure --quiet && make -j$(nproc) --quiet'
   ```

5. Build riperf3 on both:
   ```
   ssh sandbox-server-1 'cd ~/riperf3 && cargo build --release'
   ssh sandbox-client-1 'cd ~/riperf3 && cargo build --release'
   ```

6. Run riperf3 unit tests on one VM:
   ```
   ssh sandbox-server-1 'cd ~/riperf3 && cargo test'
   ```

7. Report what was built (iperf3 version, riperf3 build status, test results).
