# Interplanetary Function Interface (IPFI)

> ⚠ IPFI is currently in early beta, and some features may change as we round things off! The library is usable, but there may be a few issues.

IPFI is a Rust-based inter-process communication tool to allow programs running over the wackiest interfaces under the sun to effectively talk to each other and get stuff done collaboratively. It was designed for a build server for [a Rust web framework](https://github.com/framesurge/perseus) that needs to work with code from potentially any programming language.

IPFI provides a few things:

- Transfer of arbitrary data without any attention to shared memory, serializing and deserializing data over *the wire*
- Management of callbacks on both sides of an interface, allowing dead-simple procedure calls between programs
- Event passing with arbitrary data attachment
- Full asynchronicity and thread-safety, letting you send and receive multiple message simultaneously

But our core feature is *the wire*, which can be literally whatever you want. IPFI communicates using byte arrays fed one byte at a time into some arbitrary source. If you can pass full arrays through stdio, that's great! If you're limited to sending one byte at a time through Morse code, no problem, IPFI can handle that. This restriction was introduced because IPFI was originally intended to work in WebAssembly, where the only native types are integers, and there is no concept of arrays at the function boundary.

## Can IPFI do X?

If it's not in the above list, probably not. IPFI is designed to be incredibly simple, fast, and easy to reason about. If you need to plug two programs into each other get them to talk to each other, you've already got a complicated-enough system to not want to worry about some extra weird interface program. Having to use platform-dependent IPC message queues and shaky FFI is not ideal at all, and IPFI is trivial to reason about: you send something, it gets copied, you ask for a procedure to run, it runs on the platform that defined it.

## License

See [`LICENSE`](./LICENSE).
