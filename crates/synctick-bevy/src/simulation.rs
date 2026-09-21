//! Worker-owned Bevy simulation and deterministic schedule.
use bevy_app::App;
use bevy_ecs::{prelude::*, schedule::ScheduleLabel};
use std::time::Duration;
use synctick::{Game, Input, SessionResult, Simulation, Tick};

/// Framework-assigned tick currently being simulated; zero before the first tick.
#[derive(Resource, Debug, Default, Clone, Copy)]
pub struct TickNumber(pub u64);
/// Fixed simulation interval recorded in the session metadata.
#[derive(Resource, Debug, Clone, Copy)]
pub struct TickDuration(pub Duration);
/// Participant-tagged commands in authoritative order for the current tick.
///
/// Inputs remain available through publication and the game's cleanup hook.
#[derive(Resource)]
pub struct TickInputs<C: Send + Sync + 'static>(pub Vec<Input<C>>);
impl<C: Send + Sync + 'static> Default for TickInputs<C> {
    fn default() -> Self {
        Self(Vec::new())
    }
}
/// Deterministic gameplay schedule, run once per live or replayed tick.
#[derive(ScheduleLabel, Debug, Clone, PartialEq, Eq, Hash)]
pub struct SimStep;

/// A game's Bevy simulation construction and presentation hooks.
///
/// `build` must
/// fully initialize deterministic state; ordinary Startup/Update schedules are
/// not run on the worker. Register gameplay systems on [`SimStep`].
pub trait BevyGame: Clone + Send + Sync + 'static {
    type Command: synctick::codec::Wire + Send + Sync + 'static;
    type Initialization: synctick::codec::Wire + Send + 'static;
    const ID: [u8; 16];
    const VERSION: u32;
    /// # Errors
    /// Reject unsupported initialization or failed world construction.
    fn build(&self, initialization: Self::Initialization, app: &mut App) -> SessionResult;
    /// Include `TickNumber` and every future-affecting game resource/component.
    fn state_hash(&self, world: &mut World) -> u64;
    /// Publish complete state before Live and after valid live ticks, never during replay.
    /// Must not change deterministic state. Inputs and transient effects remain
    /// available until `finish_tick`; initial publication has no pending effects.
    /// # Errors
    /// Return terminal presentation-publication failures.
    fn publish(&self, _world: &mut World) -> SessionResult {
        Ok(())
    }
    /// Discard per-tick presentation effects after publication, or directly after
    /// a replay tick. Runs before the adapter clears inputs and world trackers.
    /// Must not change deterministic state or fail. Also runs after returned tick
    /// or publication errors; panics terminate the worker without guaranteed cleanup.
    fn finish_tick(&self, _world: &mut World) {}
}

/// Adapt a [`BevyGame`] to the engine-independent [`Game`] session API.
#[derive(Clone)]
pub struct GameAdapter<G>(pub G);
/// Worker-owned simulation constructed by [`GameAdapter`].
pub struct BevySimulation<G> {
    app: App,
    game: G,
}
impl<G: BevyGame> Game for GameAdapter<G> {
    type Command = G::Command;
    type Initialization = G::Initialization;
    type Simulation = BevySimulation<G>;
    const ID: [u8; 16] = G::ID;
    const VERSION: u32 = G::VERSION;
    fn create(
        &self,
        initialization: G::Initialization,
        tick_duration: Duration,
    ) -> SessionResult<Self::Simulation> {
        let mut app = App::new();
        app.init_resource::<TickNumber>();
        app.init_resource::<TickInputs<G::Command>>();
        app.insert_resource(TickDuration(tick_duration));
        app.init_schedule(SimStep);
        self.0.build(initialization, &mut app)?;
        Ok(BevySimulation {
            app,
            game: self.0.clone(),
        })
    }
}
impl<G: BevyGame> Simulation<G::Command> for BevySimulation<G> {
    fn current_tick(&self) -> u64 {
        self.app.world().resource::<TickNumber>().0
    }
    fn state_hash(&mut self) -> u64 {
        self.game.state_hash(self.app.world_mut())
    }
    fn advance(&mut self, tick: Tick<G::Command>) -> SessionResult {
        let world = self.app.world_mut();
        world.resource_mut::<TickNumber>().0 = tick.number;
        world.resource_mut::<TickInputs<G::Command>>().0 = tick.inputs;
        world.run_schedule(SimStep);
        Ok(())
    }
    fn finish_tick(&mut self) {
        let world = self.app.world_mut();
        self.game.finish_tick(world);
        world.resource_mut::<TickInputs<G::Command>>().0.clear();
        world.clear_trackers();
    }
    fn publish(&mut self) -> SessionResult {
        self.game.publish(self.app.world_mut())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_game::Example;
    use std::sync::atomic::Ordering;
    #[test]
    fn factory_and_publication_share_the_same_tick_contract() {
        let game = Example::default();
        let observed = game.published.clone();
        let mut sim = GameAdapter(game)
            .create((), Duration::from_millis(10))
            .unwrap();
        assert_eq!(
            sim.app.world().resource::<TickDuration>().0,
            Duration::from_millis(10)
        );
        sim.advance(Tick {
            number: 1,
            inputs: vec![Input {
                participant: synctick::ParticipantId::Host,
                command: (),
            }],
        })
        .unwrap();
        assert_eq!(observed.load(Ordering::Acquire), 0);
        sim.finish_tick();
        assert_eq!(sim.game.cleaned_inputs.load(Ordering::Acquire), 1);
        assert!(sim.app.world().resource::<TickInputs<()>>().0.is_empty());
        sim.publish().unwrap();
        assert_eq!(sim.game.published_inputs.load(Ordering::Acquire), 0);
        assert_eq!(observed.load(Ordering::Acquire), 2);
        sim.advance(Tick {
            number: 2,
            inputs: vec![Input {
                participant: synctick::ParticipantId::Host,
                command: (),
            }],
        })
        .unwrap();
        sim.publish().unwrap();
        assert_eq!(sim.game.published_inputs.load(Ordering::Acquire), 1);
        sim.finish_tick();
        assert_eq!(sim.game.cleaned_inputs.load(Ordering::Acquire), 1);
        assert_eq!(observed.load(Ordering::Acquire), 3);
        assert!(sim.app.world().resource::<TickInputs<()>>().0.is_empty());
    }
}
