# synctick

Deterministic server-sequenced multiplayer sessions, recording, and replay for Rust games.

Implement `Game` and `Simulation`, then start a host, dedicated server, remote
client, or replay through the public API. Synctick owns tick sequencing, verified
startup, participant identity, bounded command submission, recording, and shutdown.
Games own deterministic rules, initialization, state selection, and entity ordering.

```toml
[dependencies]
synctick = "0.1.0"
```

`Wire` and `StableHash` derives are re-exported by this crate.

See the [integration guide](https://github.com/nielsgroen/synctick/blob/main/docs/SESSION_FRAMEWORK.md)
and [examples](https://github.com/nielsgroen/synctick/tree/main/examples).

Licensed under the [Apache License, Version 2.0](LICENSE). See [NOTICE](NOTICE).
