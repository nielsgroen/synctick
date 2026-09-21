//! Deterministic test model: no engine or game simulation dependencies.
use crate::{protocol::TickAdvance, session::SessionResult, simulation::SessionSimulation};
#[derive(Default)]
pub struct TestSimulation {
    pub tick: u64,
    pub observed_commands: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>,
    pub commands: u64,
    pub starts: usize,
    pub live_ticks: usize,
}
impl SessionSimulation for TestSimulation {
    fn header(&self) -> crate::replay::ReplayHeader {
        crate::replay::ReplayHeader::current()
    }
    fn current_tick(&self) -> u64 {
        self.tick
    }
    fn state_hash(&mut self) -> u64 {
        self.tick.wrapping_mul(31).wrapping_add(self.commands)
    }
    fn advance(&mut self, advance: TickAdvance) -> SessionResult {
        self.tick = advance.tick;
        self.commands += u64::try_from(advance.inputs.len()).unwrap();
        if let Some(observed) = &self.observed_commands {
            observed.store(self.commands, std::sync::atomic::Ordering::Release);
        }
        Ok(())
    }
    fn publish(&mut self) -> SessionResult {
        if self.starts == 0 {
            self.starts = 1;
        } else {
            self.live_ticks += 1;
        }
        Ok(())
    }
}
