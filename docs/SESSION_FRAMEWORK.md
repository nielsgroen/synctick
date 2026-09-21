# Building a game with Synctick

`synctick` provides deterministic server-sequenced sessions without engine or
game dependencies. `synctick-bevy` adds a separate simulation App and a
presentation-side session plugin. Renet is an implementation detail.

## Plain Rust integration

Implement `Game` with a stable 16-byte `ID`, a compatibility `VERSION`, command
and initialization types implementing `codec::Wire`, and a simulation factory.
The factory runs on the session worker; the resulting simulation need not be
Send. It must start at tick zero and initialize solely from the saved data.

Implement `Simulation<Command>`: report the current tick, hash all deterministic
state, and apply `Tick<Command>` in its given order. The framework verifies tick
continuity and the resulting tick. Input entries carry `ParticipantId::Host` or
`ParticipantId::Remote(id)`. Remote origins are taken from connections, never
from command payloads. IDs identify session participants, not authenticated users.
Games define ownership; preserve those numeric IDs when continuing a save.
Gameplay rejection is deterministic game logic, not a transport error.
`advance(tick)` runs identical gameplay for live play and replay; execution phase
is private to the driver. The driver validates the resulting tick, calls
`publish()` only for valid live ticks, then calls `finish_tick()` in both modes.
Publication must not change deterministic state. Cleanup may discard per-tick
inputs and presentation effects, but must not change deterministic state or fail.
It also runs after returned advance or publication errors; panics terminate the
worker without guaranteed cleanup. Rejected tick continuity enters no hooks.
After verified startup the driver calls `publish()` before exposing Live, without
an advance or cleanup call. Publish complete state there, not historical effects.
Publication errors are terminal and do not roll back the completed tick.
This replaces the previous `advance(tick, phase)` and `on_live_start()` API without
changing command bytes, recorded formats, or game compatibility versions.

Choose an entry point:

- `host(game, ServerConfig::new(initialization))`: local participant, optional peers.
- `dedicated_server(game, config)`: no local participant; command submission is disabled.
- `connect(game, ClientConfig::new(id, address))`: initialize from the server's save.
- `replay(game, path, control)`: synchronous offline replay with a final tick/hash.

Server configuration covers port, required remote count, fixed tick duration,
load/record paths, desync policy and local command queue capacity. Defaults are
30 Hz, port 5000, no required peers and a 256-command queue. Supported fixed tick
durations are 1 microsecond through 1 second. Verification runs approximately
once per simulated second, with a ten-interval history/deadline. Queues accept
1 through 65,536 entries; the transport supports at most 1,024 remote clients.
These constraints are validated before creating a recording or socket.

Keep the returned `SessionHandle`. Clone `commands()` for typed `submit(&command)`.
Submission reports `NotLive`, `Full`, `Stopped`, or `Codec`; success means local
queue admission, not gameplay acceptance. A dedicated server has no command
receiver and reports `Stopped` on submission. Submission and network servicing
are bounded; a tick that exceeds its byte budget fails explicitly before being
recorded or broadcast. Consumers should pace large batches rather than retry in
a busy loop. There is no command receipt/acceptance-response protocol in v1.

Call `poll()` to observe worker completion without blocking. `status()` returns
coalesced presentation status; terminal typed errors are retained independently.
`cancel()` is cooperative, `join()` waits, and `shutdown()` cancels and joins.
Both join methods return a retained `Arc<SessionResult<()>>`. Dropping the handle
also cancels and joins. Clone `control()` to connect application signal handling.
Library diagnostics use `log`; applications choose the logging subscriber.

## Payload codec

Use `#[derive(synctick::Wire)]` on nonempty named or tuple structs to
generate all three passes directly from the fields, including generic bounds.
The derive is re-exported by `synctick`; games need no macro-crate dependency.
Fields are encoded in declaration order without tags or padding, preserving the
former `impl_wire_struct!` layout. Reordering fields changes the wire format.
Empty structs are rejected; use a unit field for an explicit one-byte marker.
Enums support unit, tuple, and named-field variants. Every variant must declare
an explicit unique `#[wire(tag = N)]` byte tag (0..=255). The tag comes before the
variant's fields, which retain declaration order. Reordering variants preserves
the format; changing tags or payload layouts does not. Rust discriminants are
rejected; unknown wire tags fail validation and decoding. For custom types, implement `Wire::encode`, allocation-free `Wire::validate`, and `Wire::decode`
with the same field order. `MIN_SIZE` is a positive lower bound on one encoded
value. Helpers cover integer scalars, unit, UTF-8 strings, and nested vectors.
Commands and initialization have a 64 KiB encoded budget; decoder helpers also
limit cumulative allocations to 4 MiB and collection nesting to 32 levels.
The complete payload is validated before decoding/materializing it. Collection
lengths are checked against remaining bytes, allocation is fallible, and trailing
bytes are rejected. Custom codec implementations are trusted Rust code and must
follow this contract; arbitrary Serde deserialization is not a safe substitute.
The canonical tick budget is 256 KiB with conservative framing accounting.

`examples/wallet` implements wallets with a saved seed and starting balances.
Its transfer command has a recipient and vector of amounts. Sender identity
selects the debited wallet; insufficient funds, nonexistent wallets and overflow
are deterministic rejections. Its integration test covers host, dedicated server,
client, replay, rejection parity, and continuation through public APIs only.
Its `model` module owns transfer rules, while `simulation` adapts those rules to
the framework. Call `game.snapshots()` before moving the game into a session;
`snapshots.latest()` returns an immutable live snapshot, or `None` before live
startup. Offline replay does not publish snapshots.

Run two terminals, then stop each with Enter:

```
cargo run -p synctick-example-wallet -- host /tmp/wallet.save
cargo run -p synctick-example-wallet -- client
cargo run -p synctick-example-wallet -- replay /tmp/wallet.save
```

To resume live play, run `cargo run -p synctick-example-wallet -- host --load /tmp/wallet.save`
and start the client again. Supply a new positional recording path to record
the continued session as well.

Use `server` instead of `host` for a dedicated authority. Recording destinations
must not exist. The example client connects as participant 1 to localhost:5000.

## Bevy integration

Implement `BevyGame` and wrap it in `GameAdapter`. The worker creates an App,
installs `TickNumber`, `TickDuration`, `TickInputs<Command>` and `SimStep`, then
calls `build`. Build deterministic state directly; normal Startup/Update schedules
are not run. Register game systems on `SimStep`. The adapter supplies the same
canonical tick and inputs for authority, replica and replay.

`state_hash` must include tick, game state, allocators and seeded RNG state.
`publish` is called before exposing Live and after live ticks, before trackers
are cleared. Replay skips publication. The optional `BevyGame::finish_tick`
hook discards game-owned presentation effects in both modes before the adapter
clears `TickInputs` and world trackers. Publication and cleanup must not mutate
deterministic state. Full snapshots and their delivery mechanism belong to the game.

Create `(plugin, guard) = SessionPlugin::new(handle)`, add the plugin to the
presentation App, and retain the guard outside `App::run`. The plugin supplies
`SessionCommands<Command>`, `SessionState`, and `SessionCancellation`, polls the
worker in PreUpdate, and cancels on AppExit. The guard joins even if Bevy's runner
retains the world. Call `guard.shutdown()` after `App::run` to inspect the retained
typed result, including recording flush failures. Rendering and cameras remain game-owned.

### Simulation system ordering and ambiguity detection

Order systems explicitly when their relative order affects gameplay. Bevy prevents
conflicting accesses from running simultaneously, but does not choose a gameplay
order for systems registered without an ordering dependency.

For example, these two systems both write the same resource:

```rust
use bevy::prelude::*;
use synctick_bevy::SimStep;

#[derive(Resource, Default)]
struct Wallet {
    balance: u64,
    upgraded: bool,
}

fn collect_income(mut wallet: ResMut<Wallet>) {
    wallet.balance += 10;
}

fn buy_upgrade(mut wallet: ResMut<Wallet>) {
    if !wallet.upgraded && wallet.balance >= 10 {
        wallet.balance -= 10;
        wallet.upgraded = true;
    }
}

fn register_systems(app: &mut App) {
    app.init_resource::<Wallet>();
    app.add_systems(SimStep, (collect_income, buy_upgrade).chain());
}
```

Starting at zero money, income followed by purchase buys the upgrade and leaves
zero money. Purchase followed by income leaves ten money and no upgrade. Without
`.chain()`, the tuple alone leaves that order unspecified. Use `.chain()`,
`.before()`, `.after()`, or ordered system sets to express the intended dependency;
independent systems can still run in parallel.

Bevy's schedule ambiguity detection identifies systems with "conflicting access
but indeterminate order": overlapping resource/component access where at least
one system writes, without a resolved ordering dependency. See
[ScheduleBuildSettings](https://docs.rs/bevy_ecs/0.18.1/bevy_ecs/schedule/struct.ScheduleBuildSettings.html).
`ambiguity_detection` can warn (`LogLevel::Warn`) or fail schedule construction
(`LogLevel::Error`); Bevy defaults to `LogLevel::Ignore`.

The framework currently does not enable this check. A future framework guardrail
would reject ambiguous `SimStep` schedules during initialization and report the
conflicting systems. That behavior is a recommendation, not a current guarantee.

Detection is conservative: two systems that only add to the same balance may
commute while still producing a conflict report. Deliberate exceptions can use
Bevy's `.ambiguous_with()` after establishing that order cannot change the result;
the exemption suppresses detection, it does not impose order. Avoid blanket
suppression for simulation schedules.

This check is not a complete determinism test. It cannot establish stable entity
iteration order, detect wall-clock-dependent gameplay, or discover dependencies
outside Bevy's declared resource/component accesses. Games still own deterministic
rules, stable ordering, and complete state hashing.

## Stable state hashing

Use `synctick::StableHash` on authoritative state types and call `stable_hash`
from the simulation's `state_hash` hook. The derive includes every field, including
nested structures and collections; adding an authoritative field automatically
includes it. It does not inspect a Bevy World or select resources for the game.

```rust
use synctick::{StableHash, stable_hash};

#[derive(StableHash)]
struct State {
    tick: u64,
    seed: u64,
    balances: Vec<u64>,
    rejected: u64,
}

impl State {
    fn state_hash(&self) -> u64 {
        stable_hash(self)
    }
}
```

Keep presentation effects and recomputable caches outside the derived state.
Power Garden consumes and validates recorded `Initialization` through
`TryFrom<Initialization> for PuzzleState`, moving dimensions and tiles into runtime
state. It hashes `(tick, &state)`; its `Board` keeps connectivity separately. There is no skip
attribute. Custom implementations and borrowed state projections remain possible.

ECS games must select components and sort by stable logical IDs. For example,
a game could derive `StableHash` on its `Body` component and compose its
fingerprint with its tick and logical-ID allocator like this:

```rust,ignore
let tick = world.resource::<TickNumber>().0;
let next_id = world.resource::<NextBodyId>().0;
let mut bodies: Vec<_> = world
    .query::<(&BodyId, &Body)>()
    .iter(world)
    .map(|(id, body)| (id.0, *body))
    .collect();
bodies.sort_by_key(|(id, _)| *id);
stable_hash(&(tick, next_id, bodies.as_slice()))
```

Every enum variant needs a unique explicit byte tag, either
`#[stable_hash(tag = N)]` or an existing `#[wire(tag = N)]`, never both.
Variant order may change without changing hashes; field order may not.
Hash-only types do not need `Wire` or an encoder.

The representation is fixed: little-endian fixed-width integers; one-byte 0/1
booleans; exact float bits including signed zero and NaN payloads; u64 UTF-8 byte
lengths for strings and u64 element counts for sequences. Arrays, slices, and
vectors agree. Structs and tuples concatenate fields; unit contributes nothing.
Options use a 0/1 byte tag and then the present value. References delegate.
Pointer-sized integers, unordered maps/sets, and Bevy entity IDs are unsupported.

`StateHasher::new()`, `field(&value)`, and `finish()` support incremental composition.
`write_bytes(&bytes)` is an unframed low-level operation: variable-length raw data
requires an explicit length. Built-in hashing streams without allocating or
applying wire-payload limits; custom implementations must uphold the same stable
representation discipline. ECS selection/sorting can still allocate.

The algorithm is FNV-1a 64-bit with standard offset basis and prime. It is a
noncryptographic fingerprint for a known schema, not a schema identifier or a
proof that states are identical. Changing its representation, tags, field order,
or algorithm requires a game compatibility-version bump. Both examples use game version 2; their version-1 saves and peers are rejected.
Framework format and protocol versions are unchanged, and there is no migration.

## Compatibility and limits

The save header separately identifies the framework protocol/format, game ID,
game compatibility version, fixed tick duration, and encoded initialization.
Bump the game version for incompatible rules, codecs, initialization or hashes.
Loading adopts saved initialization/timing, ignoring fresh-session values.
Framework-v1 deliberately rejects Planet protocol-5 saves and peers; no migration
is provided. Recordings remain input logs with bounded incremental replay,
exclusive creation, periodic flushing and explicit malformed-record failures.
Flush drains userspace buffers; it does not promise power-loss durability.

Required participants replay and verify before Start. Startup replacement works;
late joining, reconnecting, lobbies, account authentication, prediction, rollback,
alternative transports and render-clock smoothing remain outside v1. Complete
startup saves still occupy memory. Games remain responsible for deterministic
math, system ordering and hashing across supported platforms.

## Graphical example and session menus

`synctick-example-power-garden` implements Power Garden, a cooperative circuit puzzle.
Run `cargo run -p synctick-example-power-garden` for the Solo/Host/Join menu.
The game owns its UI, snapshots, and integer-only board rules. See
[the example README](../examples/power-garden/README.md) for controls,
recording, loading, and headless commands.

For a GUI menu, install `SessionPlugin::<Command>::idle()` and retain its guard.
The plugin provides `SessionController<Command>`; call `attach(handle)` after
creating a session through a core entry point. An occupied slot rejects incoming
handles, which are dropped and shut down. Call `detach()` to cancel, join, and
release the old session before starting another. Terminal results remain
available through the controller and guard until the next attachment.

Status is Stopped before the first attachment. Command and cancellation resources
exist only while a handle is attached; attachment/detachment synchronizes them in
the next PreUpdate. Menu state should gate input immediately during transitions.
The existing `SessionPlugin::new(handle)` entry point remains supported.

The adapter implementation is separated into `simulation.rs` for the worker App
and `session.rs` for presentation resources, attachment, and teardown. `lib.rs`
retains the public exports.
