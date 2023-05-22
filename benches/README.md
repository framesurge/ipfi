# IPFI Benchmarks

This directory contains some work-in-progress benchmarks of IPFI's performance against other RPC systems. Right now, these are under heavy construction, and are probably ludicrously wrong.

## gRPC (`tonic`) Benchmark

This uses code heavily from [Tonic's introductory tutorial](https://github.com/hyperium/tonic/blob/master/examples/helloworld-tutorial.md). Here, we benchmark a single remote procedure call that provides a name and receives a greeting to that name (e.g. `John Doe` -> `Hello, John Doe!`), using for gRPC a `tonic` server, and a manually constructed TCP server with a `rayon` thread pool for IPFI. This is really not a fair comparison to IPFI, as `tonic` is backed by `tokio`, a full async runtime, and we are using a simple threadpool for IPFI, yet IPFI comes out the victor by quite the margin!

|              | Connect time  | Call time    | Total time   |
|:-------------|:--------------|:-------------|:-------------|
| IPFI         | 62.9μs        | 379.4μs      | 442.7μs      |
| gRPC         | 227.8μs       | 505.3μs      | 733.5μs      |
| % Difference | 262.2% faster | 33.2% faster | 65.7% faster |
