//! The simulation contract consumed by live sessions and replay.
use crate::{
    protocol::TickAdvance,
    session::{SessionError, SessionResult},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickPhase {
    Replay,
    Live,
}

/// Engine-owned state. All methods run on the session's calling thread.
pub trait SessionSimulation {
    fn header(&self) -> crate::replay::ReplayHeader;
    fn initialize(&mut self, _header: &crate::replay::ReplayHeader) -> SessionResult {
        Ok(())
    }
    fn validate_command(&self, _payload: &[u8]) -> SessionResult {
        Ok(())
    }
    fn hash_interval(&self) -> u64 {
        // Round to nearest tick: the existing 33,333,333 ns duration yields 30.
        (1_000_000_000 + self.header().tick_nanos / 2) / self.header().tick_nanos
    }

    fn current_tick(&self) -> u64;
    fn state_hash(&mut self) -> u64;
    /// # Errors
    /// Returns simulation failures; successful execution must end at advance.tick.
    fn advance(&mut self, advance: TickAdvance) -> SessionResult;
    /// # Errors
    /// Returns terminal publication failures. Must not mutate deterministic state.
    fn publish(&mut self) -> SessionResult;
    fn finish_tick(&mut self) {}
}

/// # Errors
/// Rejects discontinuities before executing and incorrect adapter ticks afterward.
pub fn advance_tick(
    sim: &mut impl SessionSimulation,
    advance: TickAdvance,
    phase: TickPhase,
) -> SessionResult {
    validate_tick(sim, advance.tick)?;
    let expected = advance.tick;
    let result = sim.advance(advance).and_then(|()| {
        if sim.current_tick() != expected {
            return Err(SessionError::Simulation(format!(
                "adapter ended at tick {}, expected {expected}",
                sim.current_tick()
            )));
        }
        if phase == TickPhase::Live {
            sim.publish()?;
        }
        Ok(())
    });
    sim.finish_tick();
    result
}

pub fn validate_tick(sim: &impl SessionSimulation, tick: u64) -> SessionResult {
    let current = sim.current_tick();
    if current.checked_add(1) != Some(tick) {
        return Err(SessionError::Protocol(format!(
            "tick gap: {tick} after {current}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Failure {
        Advance,
        WrongTick,
        Publish,
    }

    #[derive(Default)]
    struct Lifecycle {
        tick: u64,
        calls: Vec<&'static str>,
        pending_effect: bool,
        failure: Option<Failure>,
    }
    impl SessionSimulation for Lifecycle {
        fn header(&self) -> crate::replay::ReplayHeader {
            crate::replay::ReplayHeader::current()
        }
        fn current_tick(&self) -> u64 {
            self.tick
        }
        fn state_hash(&mut self) -> u64 {
            self.tick
        }
        fn advance(&mut self, tick: TickAdvance) -> SessionResult {
            assert!(!self.pending_effect, "previous tick was not cleaned up");
            self.calls.push("advance");
            self.pending_effect = true;
            if self.failure == Some(Failure::Advance) {
                return Err(SessionError::Simulation("advance failed".into()));
            }
            if self.failure != Some(Failure::WrongTick) {
                self.tick = tick.tick;
            }
            Ok(())
        }
        fn publish(&mut self) -> SessionResult {
            assert!(self.pending_effect, "publication ran after cleanup");
            self.calls.push("publish");
            if self.failure == Some(Failure::Publish) {
                return Err(SessionError::Simulation("publish failed".into()));
            }
            Ok(())
        }
        fn finish_tick(&mut self) {
            self.calls.push("finish");
            self.pending_effect = false;
        }
    }

    #[test]
    fn driver_owns_publication_and_cleanup_order() {
        let mut sim = Lifecycle::default();
        for (tick, phase) in [
            (1, TickPhase::Replay),
            (2, TickPhase::Replay),
            (3, TickPhase::Live),
        ] {
            advance_tick(
                &mut sim,
                TickAdvance {
                    tick,
                    inputs: vec![],
                },
                phase,
            )
            .unwrap();
        }
        assert_eq!(
            sim.calls,
            [
                "advance", "finish", "advance", "finish", "advance", "publish", "finish"
            ]
        );
        assert!(!sim.pending_effect);
    }

    #[test]
    fn returned_errors_cleanup_without_publishing_invalid_state() {
        for phase in [TickPhase::Live, TickPhase::Replay] {
            for mut sim in [
                Lifecycle {
                    failure: Some(Failure::Advance),
                    ..Lifecycle::default()
                },
                Lifecycle {
                    failure: Some(Failure::WrongTick),
                    ..Lifecycle::default()
                },
            ] {
                assert!(
                    advance_tick(
                        &mut sim,
                        TickAdvance {
                            tick: 1,
                            inputs: vec![]
                        },
                        phase
                    )
                    .is_err()
                );
                assert_eq!(sim.calls, ["advance", "finish"]);
                assert!(!sim.pending_effect);
            }
        }
        let mut sim = Lifecycle {
            failure: Some(Failure::Publish),
            ..Lifecycle::default()
        };
        assert!(
            advance_tick(
                &mut sim,
                TickAdvance {
                    tick: 1,
                    inputs: vec![]
                },
                TickPhase::Live
            )
            .is_err()
        );
        assert_eq!(sim.calls, ["advance", "publish", "finish"]);
        assert_eq!(
            sim.tick, 1,
            "publication failure does not roll back gameplay"
        );
        assert!(!sim.pending_effect);

        sim.calls.clear();
        assert!(
            advance_tick(
                &mut sim,
                TickAdvance {
                    tick: 3,
                    inputs: vec![]
                },
                TickPhase::Live
            )
            .is_err()
        );
        assert!(
            sim.calls.is_empty(),
            "a rejected tick never enters the lifecycle"
        );
    }
    struct Broken {
        tick: u64,
        called: bool,
        fail: bool,
    }
    impl SessionSimulation for Broken {
        fn header(&self) -> crate::replay::ReplayHeader {
            crate::replay::ReplayHeader::current()
        }
        fn current_tick(&self) -> u64 {
            self.tick
        }
        fn state_hash(&mut self) -> u64 {
            self.tick
        }
        fn advance(&mut self, _: TickAdvance) -> SessionResult {
            self.called = true;
            if self.fail {
                return Err(SessionError::Simulation("intentional".into()));
            }
            Ok(()) // Deliberately violates the postcondition.
        }
        fn publish(&mut self) -> SessionResult {
            Ok(())
        }
    }
    #[test]
    fn adapter_failure_and_tick_contract_are_checked() {
        for phase in [TickPhase::Live, TickPhase::Replay] {
            let mut sim = Broken {
                tick: 0,
                called: false,
                fail: false,
            };
            assert!(matches!(
                advance_tick(
                    &mut sim,
                    TickAdvance {
                        tick: 2,
                        inputs: vec![]
                    },
                    phase
                ),
                Err(SessionError::Protocol(_))
            ));
            assert!(!sim.called);
            assert!(matches!(
                advance_tick(
                    &mut sim,
                    TickAdvance {
                        tick: 1,
                        inputs: vec![]
                    },
                    phase
                ),
                Err(SessionError::Simulation(_))
            ));
            assert!(sim.called);
            sim.fail = true;
            assert!(
                matches!(advance_tick(&mut sim, TickAdvance { tick: 1, inputs: vec![] }, phase), Err(SessionError::Simulation(message)) if message == "intentional")
            );
            sim.tick = u64::MAX;
            sim.called = false;
            assert!(
                advance_tick(
                    &mut sim,
                    TickAdvance {
                        tick: 0,
                        inputs: vec![]
                    },
                    phase
                )
                .is_err()
            );
            assert!(!sim.called);
        }
    }
}
