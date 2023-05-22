# IPFI Benchmarks

This directory contains some work-in-progress benchmarks of IPFI's performance against other RPC systems. Right now, these are under heavy construction, and are probably ludicrously wrong. Since we benchmark actual servers running locally, there can be quite a bit of variation between measurements, though generally somewhere between 1000-10000 iterations removes most of this variation.

## gRPC (`tonic`) Benchmark

This uses code heavily from [Tonic's introductory tutorial](https://github.com/hyperium/tonic/blob/master/examples/helloworld-tutorial.md). Here, we benchmark a single remote procedure call that provides a name and receives a greeting to that name (e.g. `John Doe` -> `Hello, John Doe!`), using for gRPC a `tonic` server, and a manually constructed TCP server with a `rayon` thread pool for IPFI. This is really not a fair comparison to IPFI, as `tonic` is backed by `tokio`, a full async runtime, and we are using a simple threadpool for IPFI, yet IPFI comes out the victor by quite the margin!

|              | Connect time  | Call time     | Total time    |
|:-------------|:--------------|:--------------|:--------------|
| IPFI         | 29.1μs        | 120.9μs       | 150.5μs       |
| gRPC         | 139.5μs       | 294.0μs       | 434.0μs       |
| % Difference | 378.9% faster | 143.2% faster | 188.3% faster |
