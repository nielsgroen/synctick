# Architecture

Synctick owns session mechanics. Each game owns deterministic rules and state.
The examples are ordinary consumers; framework crates do not depend on them.

## Ownership

`synctick` provides the `Game`/`Simulation` contracts and host, dedicated-server,
client, and offline replay entry points. Renet channels, startup transfer,
worker construction, and internal crossbeam plumbing stay behind those APIs.
`Wire` validation traverses complete payloads before materialization; collection
helpers enforce byte, allocation, and nesting budgets. `StableHash` separately
streams canonical state representations without allocating or applying payload
budgets. Derives are implemented in `synctick-derive` and re-exported by `synctick`.

`synctick-bevy` creates a separate simulation App on the worker thread. Games
register systems on `SimStep`; the adapter supplies tick and ordered-input
resources. A presentation App submits commands and reads game-defined snapshots.
The plugin/controller own attachment and lifecycle; a guard retained outside
`App::run` joins the worker and exposes terminal errors after runner teardown.

## Session data flow

The authority sequences typed commands into ticks. Remote origins are bound to
connected participants; local host commands also carry framework identity.
Gameplay legality is resolved by the shared simulation, so rejected actions have
the same effect on authority, replicas, and replay. Malformed protocol data is a
framework error. The authority does not wait for an acknowledgement on every tick.

Before live play, participants receive recorded initialization and ordered
history, replay incrementally, and verify tick/hash agreement through a readiness
barrier. Compatibility and payload validation precede simulation advancement.
Live hash verification detects state disagreement; session status is coalesced,
while terminal worker results are retained separately.

Recordings contain format/protocol compatibility, game ID/version, fixed tick
duration, game initialization, and an ordered command log. Loading adopts saved
initialization and timing. Continuation records into a new file; existing files
are never silently overwritten. Replay uses the same rules but does not publish
historical presentation effects.

## Advancement and publication

The driver advances a tick, validates its result, publishes valid live state,
and finishes the tick. Cleanup discards transient effects and inputs during both
live play and replay. Publication and cleanup must not change authoritative state.
Verified startup publishes once before exposing Live. Games choose snapshot
shapes and delivery mechanisms; cameras and presentation clocks remain external.

See the [integration guide](SESSION_FRAMEWORK.md) for API details and the
[determinism contract](DETERMINISM.md) for ordering and hashing responsibilities.
