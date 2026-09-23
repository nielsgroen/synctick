//! Engine-independent server-sequenced deterministic sessions.
//!
//! Implement [`Game`] and [`Simulation`], then use [`host`], [`dedicated_server`],
//! [`connect`] or [`replay`]. Transport channels and replay envelopes are private.
extern crate self as synctick;

mod api;
pub mod codec;
pub mod hashing;
pub mod managed;
mod pacer;
mod protocol;
mod replay;
mod run_client;
mod run_server;
mod session;
mod simulation;
mod startup;
#[cfg(test)]
mod test_simulation;

pub use api::{
    ClientConfig, CommandSender, Game, Input, ParticipantId, ReplayOutcome, ServerConfig,
    SessionHandle, Simulation, SubmitError, Tick, connect, dedicated_server, host, replay,
};
pub use run_server::DesyncPolicy;
pub use session::{LoadingPhase, SessionControl, SessionError, SessionResult, SessionStatus};

pub use codec::Wire;

pub use hashing::{StableHash, StateHasher, stable_hash};
