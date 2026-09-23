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

## Checkpoint lobbies

`managed::host` and `managed::connect` provide an opt-in, checkpoint-backed
session protocol alongside the existing recording/replay API. Implement
`managed::CheckpointSimulation` to export/restore bounded game checkpoints and
apply transactional game-owned lobby configuration. Games own seat assignment;
Synctick owns transport participants, membership, organizer authority, verified
synchronization, and pause/resume epochs.

Use `SessionControl::lobby()` for the connected roster/readiness and
`organize(OrganizerAction::Start(...))` to start or resume. Dedicated lobbies give
organization to the first member and transfer it in connection order. A member
leaving pauses the session. Clients reconnect with a fresh transport connection
ID and their remembered `Identity`; IDs are installation identifiers, not accounts.

Live sessions remain deterministic command replicas: each admitted command forms
the next tick, replicated in reliable order and hash-checked before publication.
These managed sessions are command-driven, not the fixed-rate physics/replay
sessions provided by the original API. Connection/configuration changes distribute
an immutable checkpoint and require matching acknowledgments before resume.
Queued inputs carry the old epoch and cannot cross that boundary. Game factories
and callbacks run only on the worker; snapshots are published for UI/save use.

Managed protocol v2 is separate from the legacy recording transport. Checkpoint
packets are bounded to 256 KiB. Oversized games must explicitly add a chunked
checkpoint transfer before adopting this API; oversized payloads report errors.
