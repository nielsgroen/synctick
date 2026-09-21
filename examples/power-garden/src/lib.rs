//! Power Garden: a deterministic circuit puzzle using the public Bevy session API.
//!
//! The model owns integer-only rules, the simulation adapter owns its worker
//! world, and the executable owns menus and presentation. No transport internals
//! are required. Construct a `GameAdapter(PowerGarden::default())` to use the
//! framework's host, dedicated-server, client, or replay entry points.
#![deny(missing_docs)]

mod model;
mod simulation;

pub use model::{Board, Initialization, InitializationError, Kind, Rotate, Tile};
pub use simulation::{PowerGarden, RotationEffect, Snapshot, Snapshots};
