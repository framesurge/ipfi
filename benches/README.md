# IPFI Benchmarks

This directory contains some work-in-progress benchmarks of IPFI's performance against other RPC systems. Right now, these are under heavy construction, and are probably ludicrously wrong. Since we benchmark actual servers running locally, there can be quite a bit of variation between measurements, though generally somewhere between 1000-10000 iterations removes most of this variation.

## Single Procedure Call

This benchmark spins up a server for each of the benchmarked systems and executes a single procedure call against them, measuring the time taken for the client to connect to the server, and the time for a response to be received and validated. In this simple benchmark, IPFI's synchronous API marginally outperforms its asynchronous one, since `tokio` is best suited to higher-load conditions (with connections taking longer to process), and, for a single request, its power is not shown at all. Note that the time taken to actually make a procedure call is actually improved with the asynchronous API, however. The primary speed difference between IPFI and gRPC in this benchmark is the issue that gRPC uses HTTP, and therefore simply needs to transfer vastly more bytes than IPFI's hand-optimised protocol, leading to substantial speed increases (note that 55.8% faster indicates that IPFI is 2.26x faster!).

### Raw Results

```
--- ipfi_blocking vs ipfi_async ---
Metric 'call_time': ipfi_blocking (117.3μs) is 20.2% faster than ipfi_async (14
7.1μs).
Metric 'connect_time': ipfi_blocking (30.1μs) is 37.0% faster than ipfi_async (
47.8μs).
Metric 'total_time': ipfi_blocking (147.9μs) is 24.3% faster than ipfi_async (195.3μs).

--- ipfi_blocking vs grpc ---
Metric 'call_time': ipfi_blocking (117.3μs) is 60.4% faster than grpc (296.5μs).
Metric 'connect_time': ipfi_blocking (30.1μs) is 78.7% faster than grpc (141.7μs).
Metric 'total_time': ipfi_blocking (147.9μs) is 66.3% faster than grpc (438.6μs).

--- ipfi_async vs grpc ---
Metric 'call_time': ipfi_async (147.1μs) is 50.4% faster than grpc (296.5μs).
Metric 'connect_time': ipfi_async (47.8μs) is 66.3% faster than grpc (141.7μs).
Metric 'total_time': ipfi_async (195.3μs) is 55.5% faster than grpc (438.6μs)
```

In this benchmark, **IPFI is 2.97x faster than gRPC** (with IPFI's synchronous API).
