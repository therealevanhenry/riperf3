Start both sandbox VMs and wait for them to be SSH-ready.

Run as a single script:

```bash
fish -c 'sandbox start sandbox-server-1' &
fish -c 'sandbox start sandbox-client-1' &
wait
```

Then verify both are reachable:

```bash
ssh sandbox-server-1 'hostname' && ssh sandbox-client-1 'hostname'
```
