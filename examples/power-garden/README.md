# Power Garden

A cooperative circuit puzzle and a graphical example of `synctick-bevy`.

```sh
cargo run -p synctick-example-power-garden
```

Choose **Solo**, **Host**, or **Join** in the menu. A host waits for one remote
player; Join defaults to `127.0.0.1:5000` as participant 1. Click wires to rotate
them clockwise. Neighboring ports must face each other. Connect the central
source to all four flowers to complete the puzzle.

Host and Solo can load a recording or record to a new file. To continue a save
while recording, choose a different, nonexistent destination. Errors appear in
the GUI. Back to menu cancels and joins the session; another session can then be
started without reopening the window.

Fields support typing, Backspace, Tab, and Ctrl+A to clear. Paths can be absolute
or relative to the launch directory. Join accepts numeric socket addresses,
including bracketed IPv6 addresses.

Headless tools:

```sh
cargo run -p synctick-example-power-garden -- server --port 5000 --record garden.save
cargo run -p synctick-example-power-garden -- replay garden.save
```

The server stops on Enter or EOF. `server --load garden.save --record next.save`
continues an existing recording. Offline replay prints the final tick and hash;
it does not create a window.

## Structure

- `model/tile`: tile roles, stable tags, and port rotation.
- `model/initialization`: recorded setup, default puzzle, and validation.
- `model/board`: validated conversion, authoritative state, commands, and hashing.
- `model/connectivity`: reciprocal-port traversal shared by validation and play.
- `simulation`: separate Bevy worker App, schedule, hashing, and immutable snapshots.
- `menu`: setup forms, typed command submission, and session attachment.
- `presentation`: board geometry, status, hover feedback, and cosmetic flashes.

Any participant can rotate any wire. Invalid and post-completion moves are
no-ops. Cosmetic rotation effects are excluded from the hash and discarded by
`finish_tick()` in live play and replay. Publication is coalesced: an intermediate
flash may be skipped, but each snapshot contains the entire current board.

The example uses only public framework APIs and no downloaded assets. Window
teardown retains a `SessionGuard` outside `App::run` and reports terminal errors,
including recording flush failures.

Game compatibility version 2 uses framework `StableHash` fingerprints. Version-1
recordings and peers are rejected; there is no save migration.
