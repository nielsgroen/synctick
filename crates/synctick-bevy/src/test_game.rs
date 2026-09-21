//! Shared deterministic fixture for adapter and lifecycle tests.
use crate::{BevyGame, TickInputs, TickNumber};
use bevy_app::App;
use bevy_ecs::world::World;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use synctick::SessionResult;

#[derive(Clone, Default)]
pub struct Example {
    pub published: Arc<AtomicU64>,
    pub published_inputs: Arc<AtomicU64>,
    pub cleaned_inputs: Arc<AtomicU64>,
}
impl BevyGame for Example {
    type Command = ();
    type Initialization = ();
    const ID: [u8; 16] = *b"bevy-test-game01";
    const VERSION: u32 = 1;
    fn build(&self, (): (), _: &mut App) -> SessionResult {
        Ok(())
    }
    fn state_hash(&self, world: &mut World) -> u64 {
        world.resource::<TickNumber>().0
    }
    fn publish(&self, world: &mut World) -> SessionResult {
        self.published
            .store(world.resource::<TickNumber>().0 + 1, Ordering::Release);
        self.published_inputs.store(
            u64::try_from(world.resource::<TickInputs<()>>().0.len()).unwrap(),
            Ordering::Release,
        );
        Ok(())
    }
    fn finish_tick(&self, world: &mut World) {
        self.cleaned_inputs.store(
            u64::try_from(world.resource::<TickInputs<()>>().0.len()).unwrap(),
            Ordering::Release,
        );
    }
}
