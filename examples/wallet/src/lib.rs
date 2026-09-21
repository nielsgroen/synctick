//! A small deterministic game demonstrating the public session framework API.
//!
//! Each participant owns a wallet. Commands transfer the sum of a list of
//! amounts, with deterministic rejection for invalid ownership, funds, or
//! arithmetic. Initialization and commands derive [`synctick::Wire`].
//!
//! [`WalletGame`] adapts the rules to the framework. [`Snapshots`] exposes
//! immutable live state to presentation code; it is not part of the state hash.
//! Host, dedicated server, client, and replay all execute the same rules.
//!
//! ```no_run
//! use synctick::{ServerConfig, host};
//! use synctick_example_wallet::{Initialization, WalletGame};
//!
//! let game = WalletGame::default();
//! let snapshots = game.snapshots();
//! let mut session = host(game, ServerConfig::new(Initialization {
//!     seed: 42,
//!     balances: vec![100, 200],
//! }))?;
//! // A presentation loop can read snapshots.latest() and submit typed commands.
//! let result = session.shutdown();
//! assert!(result.is_ok());
//! # Ok::<(), synctick::SessionError>(())
//! ```
#![deny(missing_docs)]

mod model;
mod simulation;

pub use model::{Initialization, State, Transfer};
pub use simulation::{Snapshots, WalletGame, WalletSimulation};
