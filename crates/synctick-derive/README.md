# synctick-derive

Wire and StableHash derive macros for Synctick.

Derive macros for deterministic command encoding and state hashing.
Use the `Wire` and `StableHash` re-exports from `synctick`; ordinary consumers do
not need a direct dependency on this implementation crate.

```rust
use synctick::{StableHash, Wire};

#[derive(Wire, StableHash)]
struct Move {
    x: i32,
    y: i32,
}
```

See the [integration guide](https://github.com/nielsgroen/synctick/blob/main/docs/SESSION_FRAMEWORK.md)
and [examples](https://github.com/nielsgroen/synctick/tree/main/examples).

Licensed under the [Apache License, Version 2.0](LICENSE). See [NOTICE](NOTICE).
