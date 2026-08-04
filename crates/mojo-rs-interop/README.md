# mojo-rs-interop

Interoperability test clients.

**Status: Scaffolded** (structure only; no behavior yet).

Mixed-language pairings required by the bindings court:

```text
C++ client -> C++ oracle server
C++ client -> Rust native server
Rust native client -> C++ oracle server
Rust native client -> Rust native server
```

The two mixed-language directions are mandatory.
