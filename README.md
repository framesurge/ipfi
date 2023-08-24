# Interplanetary Function Interface (IPFI)

> ⚠ IPFI is currently in early beta, and some features may change as we round things off! The library is usable, but there may be a few issues.

IPFI is a Rust-based remote procedure call system to allow programs running over the wackiest interfaces under the sun to effectively talk to each other and get stuff done collaboratively. It was designed for a build server for [a Rust web framework](https://github.com/framesurge/perseus) that needs to work with code from potentially any programming language, but provides an effective interface for everything from one-byte-at-a-time communication of complex types between programming languages to production-grade servers.

IPFI provides a few things:

- Transfer of arbitrary data without any attention to shared memory, serializing and deserializing data over *the wire*
- Management of callbacks on both sides of an interface, allowing dead-simple procedure calls between programs
- Full support for streaming procedure data
- Full asynchronicity and thread-safety, letting you send and receive multiple message simultaneously
- A strong attention to security and concurrent performance with multiple layers of DoS prevention mechanisms built-in
- A minimal code footprint with feature flags and very few dependencies

Our core feature is *the wire*, which can be literally whatever you want. IPFI communicates using byte arrays fed one byte at a time into some arbitrary source. If you can pass full arrays through stdio, that's great! If you're limited to sending one byte at a time through Morse code, no problem, IPFI can handle that. This restriction was introduced because IPFI was originally intended to work in WebAssembly, where the only native types are integers, and there is no concept of arrays at the function boundary (not without proposals that were not yet standardized when it was conceived of, that is).

## Can IPFI do X?

If it's not in the above list, probably not. IPFI is designed to be incredibly simple, fast, and easy to reason about. If you need to plug two programs into each other get them to talk to each other, you've already got a complicated-enough system to not want to worry about some extra weird interface program. Having to use platform-dependent IPC message queues and shaky FFI is not ideal at all, and IPFI is trivial to reason about: you send something, it gets copied, you ask for a procedure to run, it runs on the platform that defined it. Better still, IPFI automatically handles what happens if the connection between two systems blows up: all the handles you were using to wait for responses will cleanly terminate, and any left-over internal messages will be cleaned up. This allows you to have one *interface* that can connect to many other programs, each with their own *wire*. Unsurprisingly, this can be used to create a server system, that, in our testing, is *significantly* faster than gRPC! (See preliminary benchmarks, which are probably riddled with errors, [here]().)

## Is IPFI a transport protocol?

No! IPFI is *technically* an application-layer protocol, meaning it operates at about the same level as HTTP, but it's unique in that it works with anything that can be read from and written to. That means you can run IPFI over TCP, HTTP, carrier pigeon (literally), or anything else you can conceive of. As long as you have some underlying transport that can guarantee that the bytes IPFI sends will get to the other end, and in the correct order, IPFI will do literally everything else required to implement very complex streaming RPC systems. In future, we will introduce support for native multiplexing, making IPFI suitable for protocols like QUIC (which powers HTTP/3): the groundwork for this has already been laid, and it's now a matter of altering only a few more sections of the codebase.

*Oh, and stay tuned for an example of using IPFI to communicate between two systems using literally Morse code!*

## License

See [`LICENSE`](./LICENSE).
