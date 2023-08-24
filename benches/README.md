# IPFI Benchmarks

This directory contains some work-in-progress benchmarks of IPFI's performance against other RPC systems. Right now, these are under heavy construction, and are probably ludicrously wrong. Since we benchmark actual servers running locally, there can be quite a bit of variation between measurements, though generally somewhere between 1000-10000 iterations removes most of this variation.

## Single Procedure Call

This benchmark spins up a server for each of the benchmarked systems and executes a single procedure call against them, measuring the time taken for the client to connect to the server, and the time for a response to be received and validated. In this simple benchmark, IPFI's synchronous API marginally outperforms its asynchronous one, since `tokio` is best suited to higher-load conditions (with connections taking longer to process), and, for a single request, its power is not shown at all. Note that the time taken to actually make a procedure call is actually improved with the asynchronous API, however. The primary speed difference between IPFI and gRPC in this benchmark is the issue that gRPC uses HTTP, and therefore simply needs to transfer vastly more bytes than IPFI's hand-optimised protocol, leading to substantial speed increases.

### Raw Results

The following were observed after 100k runs of the benchmarks with a custom harness.

```
--- ipfi_blocking vs ipfi_async ---
Metric 'call_time': ipfi_blocking (134.6μs) is 22.8% (1.2x) faster than ipfi_async (165.3μs).
Metric 'connect_time': ipfi_blocking (31.0μs) is 54.0% (1.5x) faster than ipfi_async (47.8μs).
Metric 'total_time': ipfi_blocking (166.1μs) is 28.5% (1.3x) faster than ipfi_async (213.6μs).

--- ipfi_blocking vs grpc ---
Metric 'call_time': ipfi_blocking (134.6μs) is 119.7% (2.2x) faster than grpc (295.8μs).
Metric 'connect_time': ipfi_blocking (31.0μs) is 355.0% (4.6x) faster than grpc (141.2μs).
Metric 'total_time': ipfi_blocking (166.1μs) is 163.3% (2.6x) faster than grpc (437.4μs).

--- ipfi_async vs grpc ---
Metric 'call_time': ipfi_async (165.3μs) is 78.9% (1.8x) faster than grpc (295.8μs).
Metric 'connect_time': ipfi_async (47.8μs) is 195.6% (3.0x) faster than grpc (141.2μs).
Metric 'total_time': ipfi_async (213.6μs) is 104.8% (2.0x) faster than grpc (437.4μs).
```

In this benchmark, **IPFI is 2.63x faster than gRPC** (with IPFI's synchronous API).
