//! Session adapter and publication; wallet rules live in the model.
use crate::{Initialization, State, Transfer, model::Rejection};
use arc_swap::ArcSwapOption;
use std::{sync::Arc, time::Duration};
use synctick::{Game, ParticipantId, SessionError, SessionResult, Simulation, Tick};

/// Read-only access to the latest complete live state.
///
/// Clones share publication, but readers cannot overwrite it or mutate the
/// simulation. Snapshots may skip intermediate ticks when the reader is slower.
#[derive(Clone, Default)]
pub struct Snapshots {
    state: Arc<ArcSwapOption<State>>,
}

impl Snapshots {
    /// Return the last published state, or `None` before live startup.
    ///
    /// Offline replay does not publish. An earlier snapshot remains valid after
    /// later ticks and after session shutdown.
    #[must_use]
    pub fn latest(&self) -> Option<Arc<State>> {
        self.state.load_full()
    }

    fn publish(&self, state: &State) {
        self.state.store(Some(Arc::new(state.clone())));
    }
}

/// Game definition passed to the framework's host, server, client, or replay API.
///
/// Create one definition per session. Clones share the snapshot stream and are
/// intended for observing the same session, not running independent simulations.
#[derive(Clone, Default)]
pub struct WalletGame {
    snapshots: Snapshots,
}

impl WalletGame {
    /// Obtain a reader before moving this game into a framework entry point.
    #[must_use]
    pub fn snapshots(&self) -> Snapshots {
        self.snapshots.clone()
    }
}

/// Worker-owned simulation constructed by [`WalletGame`].
///
/// Host commands debit wallet zero. Remote participant `n > 0` debits wallet
/// `n`; remote participant zero owns no wallet. Rules and hashing are identical
/// during live ticks and replay. The framework invokes snapshot publication separately from advancement.
pub struct WalletSimulation {
    state: State,
    snapshots: Snapshots,
}

impl Game for WalletGame {
    type Command = Transfer;
    type Initialization = Initialization;
    type Simulation = WalletSimulation;
    const ID: [u8; 16] = *b"wallet-game-v001";
    const VERSION: u32 = 2;

    fn create(
        &self,
        initialization: Initialization,
        _: Duration,
    ) -> SessionResult<WalletSimulation> {
        if initialization.balances.is_empty() {
            return Err(SessionError::Simulation(
                "at least one wallet is required".into(),
            ));
        }
        Ok(WalletSimulation {
            state: State {
                seed: initialization.seed,
                balances: initialization.balances,
                ..State::default()
            },
            snapshots: self.snapshots.clone(),
        })
    }
}

impl Simulation<Transfer> for WalletSimulation {
    fn current_tick(&self) -> u64 {
        self.state.tick
    }

    fn state_hash(&mut self) -> u64 {
        self.state.state_hash()
    }

    fn advance(&mut self, tick: Tick<Transfer>) -> SessionResult {
        self.state.tick = tick.number;
        for input in tick.inputs {
            let result = match input.participant {
                ParticipantId::Host => self.state.transfer(0, &input.command),
                ParticipantId::Remote(0) => Err(Rejection::UnknownSender),
                ParticipantId::Remote(id) => self.state.transfer(id, &input.command),
            };
            if result.is_err() {
                self.state.rejected = self.state.rejected.checked_add(1).ok_or_else(|| {
                    SessionError::Simulation("rejection counter exhausted".into())
                })?;
            }
        }
        Ok(())
    }

    fn publish(&mut self) -> SessionResult {
        self.snapshots.publish(&self.state);
        Ok(())
    }
}
