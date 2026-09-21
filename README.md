# Synctick

A deterministic, server-sequenced session framework for Rust games. A server
orders commands into fixed ticks; authority, clients, and replay run the same
game rules. Synctick handles verified startup, participant identity, bounded
payloads, desync checking, recording, replay, and worker lifecycle.

## Crates

- `synctick`: engine-independent session APIs, codecs, and stable state hashing.
- `synctick-bevy`: a separate simulation App, deterministic schedule, presentation
  resources, session attachment, and an owning teardown guard.
- `synctick-derive`: `Wire` and `StableHash` derives, re-exported by `synctick`.
- `synctick-example-wallet`: plain Rust rules with participant-sensitive transfers.
- `synctick-example-power-garden`: a cooperative circuit puzzle with a Bevy GUI.

## Try the examples

```sh
# Graphical Solo / Host / Join menu
cargo run -p synctick-example-power-garden

# Wallet example: run host and client in separate terminals
cargo run -p synctick-example-wallet -- host /tmp/wallet.save
cargo run -p synctick-example-wallet -- client
# After stopping the host and client with Enter:
cargo run -p synctick-example-wallet -- replay /tmp/wallet.save
```

Recording destinations must not already exist. See the
[Power Garden guide](examples/power-garden/README.md) for GUI controls, continued
sessions, and headless server/replay commands. Graphical builds require Bevy's
platform dependencies; CI documents the Linux packages.

## Integrate a game

Implement `Game` and `Simulation`, or `BevyGame` through `GameAdapter`. Games own
initialization, gameplay validation, authoritative state, and presentation.
Derive `Wire` for recorded data and `StableHash` for authoritative state types.
Use `host`, `dedicated_server`, `connect`, or `replay` through the public APIs.

- [Integration guide](docs/SESSION_FRAMEWORK.md): APIs, lifecycle, examples, and limits.
- [Architecture](docs/ARCHITECTURE.md): ownership and data flow.
- [Determinism](docs/DETERMINISM.md): ordering, state hashes, and verification limits.

Renet is the sole transport. Late joining, reconnecting, prediction, rollback,
and runtime tick-rate changes are outside the current scope. Session participant
IDs are not authenticated accounts. The three library crates are configured for crates.io; examples remain unpublished.
See the [release checklist](docs/RELEASING.md).

## Development

```sh
cargo test --workspace --locked
cargo test --manifest-path tests/fixtures/renamed-dependency/Cargo.toml --locked
cargo fmt --all --check
cargo fmt --manifest-path tests/fixtures/renamed-dependency/Cargo.toml --check
cargo clippy --workspace --all-targets --locked -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked
```

CI uses one Ubuntu job for tests, formatting, and Clippy. New commits cancel
superseded runs. Rustdoc, Rust 1.92, and package verification are opt-in release
checks: select **Actions → CI → Run workflow → release_checks** before publishing.
Successful local tests do not establish mixed-platform session determinism.

## Provenance and compatibility

Extracted as a current-code snapshot from
[nielsgroen/planet-game](https://github.com/nielsgroen/planet-game), source commit
[`c1f786ca5c180a574ff3699ce98dae3218543db2`](https://github.com/nielsgroen/planet-game/commit/c1f786ca5c180a574ff3699ce98dae3218543db2).
Historical development remains in that repository; it is not imported here.

Package renaming does not change protocol or save formats, game IDs, or hashing.
Both examples retain game compatibility version 2; version-1 recordings remain
incompatible.

## License

Licensed under the [Apache License, Version 2.0](LICENSE).
See [NOTICE](NOTICE) for attribution.
