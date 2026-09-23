# synctick-bevy

Bevy integration for Synctick deterministic multiplayer sessions.

Implement `BevyGame` and use `GameAdapter` with Synctick session entry points.
A separate worker App runs the deterministic `SimStep` schedule; the presentation
App uses `SessionPlugin`, typed commands, status, and an owning session guard.
This release integrates with Bevy 0.18.

```toml
[dependencies]
synctick = "0.1.0"
synctick-bevy = "0.1.0"
```

See the [integration guide](https://github.com/nielsgroen/synctick/blob/main/docs/SESSION_FRAMEWORK.md)
and [examples](https://github.com/nielsgroen/synctick/tree/main/examples).

Licensed under the [Apache License, Version 2.0](LICENSE). See [NOTICE](NOTICE).

For checkpoint lobbies, implement `CheckpointGame` alongside `BevyGame` and use
`GameAdapter` with `synctick::managed`. The adapter invokes checkpoint, restore,
and lobby configuration hooks on its worker-owned World. Restore must include
`TickNumber` and all future-affecting state; configuration errors must leave the
World unchanged. The existing `SessionPlugin`/`SessionController` lifecycle also
owns managed sessions, including their `Lobby` and `Paused` statuses.
