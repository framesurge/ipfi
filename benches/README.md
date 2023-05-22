# IPFI Benchmarks

This directory contains some work-in-progress benchmarks of IPFI's performance against other RPC systems. Right now, these are under heavy construction, and are probably ludicrously wrong. Since we benchmark actual servers running locally, there can be quite a bit of variation between measurements, though generally somewhere between 1000-10000 iterations removes most of this variation.

## gRPC (`tonic`) Benchmark

This uses code heavily from [Tonic's introductory tutorial](https://github.com/hyperium/tonic/blob/master/examples/helloworld-tutorial.md). Here, we benchmark a single remote procedure call that provides a name and receives a greeting to that name (e.g. `John Doe` -> `Hello, John Doe!`), using for gRPC a `tonic` server, and a manually constructed TCP server with a `rayon` thread pool for IPFI. This is really not a fair comparison to IPFI, as `tonic` is backed by `tokio`, a full async runtime, and we are using a simple threadpool for IPFI, yet IPFI comes out the victor by quite the margin!

|              | Connect time  | Call time     | Total time    |
|:-------------|:--------------|:--------------|:--------------|
| IPFI         | 29.5μs        | 124.8μs       | 154.8μs       |
| gRPC         | 140.2μs       | 296.3μs       | 437.0μs       |
| % Difference | 374.7% faster | 137.5% faster | 182.3% faster |
