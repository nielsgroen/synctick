//! Bevy integration for deterministic sessions.
//!
//! The worker owns a separate simulation App and deterministic schedule.
//! Presentation uses typed command/status resources and an owning session guard.
//! Install an idle plugin for menus, then attach through `SessionController`.

mod session;
mod simulation;

pub use session::{
    SessionCancellation, SessionCommands, SessionController, SessionGuard, SessionPlugin,
    SessionState,
};
pub use simulation::{
    BevyGame, BevySimulation, CheckpointGame, GameAdapter, SimStep, TickDuration, TickInputs,
    TickNumber,
};

#[cfg(test)]
mod test_game;
