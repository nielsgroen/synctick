//! Bevy worker-world integration and immutable publication.
use crate::{Board, Initialization, Rotate};
use arc_swap::ArcSwapOption;
use bevy::prelude::*;
use std::sync::Arc;
use synctick::{ParticipantId, SessionError, SessionResult};
use synctick_bevy::{BevyGame, SimStep, TickInputs, TickNumber};

/// Cosmetic rotation notification. Not part of deterministic state.
#[derive(Clone, Debug)]
pub struct RotationEffect {
    /// Rotated row-major cell.
    pub cell: u32,
    /// Framework-bound origin, used only for the flash color.
    pub participant: ParticipantId,
}
/// Complete published board and effects from its latest tick.
#[derive(Clone, Debug)]
pub struct Snapshot {
    /// Last completed simulation tick.
    pub tick: u64,
    /// Deterministic puzzle state.
    pub board: Board,
    /// Cosmetic effects; readers may miss intermediate coalesced publications.
    pub effects: Vec<RotationEffect>,
}
/// Read-only publication stream. No snapshot exists before live startup.
#[derive(Clone, Default)]
pub struct Snapshots(Arc<ArcSwapOption<Snapshot>>);
impl Snapshots {
    /// Load the latest immutable snapshot. Offline replay never publishes.
    #[must_use]
    pub fn latest(&self) -> Option<Arc<Snapshot>> {
        self.0.load_full()
    }
}
/// Game definition for `synctick_bevy::GameAdapter`.
///
/// Construct a separate value for each session; clones share the publication slot.
#[derive(Clone, Default)]
pub struct PowerGarden {
    snapshots: Snapshots,
}
impl PowerGarden {
    /// Obtain the publication reader before moving this game into a session.
    #[must_use]
    pub fn snapshots(&self) -> Snapshots {
        self.snapshots.clone()
    }
}
#[derive(Resource)]
struct Puzzle(Board);
#[derive(Resource, Default)]
struct Effects(Vec<RotationEffect>);

impl BevyGame for PowerGarden {
    type Command = Rotate;
    type Initialization = Initialization;
    const ID: [u8; 16] = *b"power-garden-v01";
    const VERSION: u32 = 2;

    fn build(&self, initialization: Initialization, app: &mut App) -> SessionResult {
        app.insert_resource(Puzzle(
            Board::new(initialization)
                .map_err(|error| SessionError::Simulation(error.to_string()))?,
        ));
        app.init_resource::<Effects>();
        app.add_systems(SimStep, apply_rotations);
        Ok(())
    }
    fn state_hash(&self, world: &mut World) -> u64 {
        world
            .resource::<Puzzle>()
            .0
            .state_hash(world.resource::<TickNumber>().0)
    }
    fn publish(&self, world: &mut World) -> SessionResult {
        self.snapshots.0.store(Some(Arc::new(Snapshot {
            tick: world.resource::<TickNumber>().0,
            board: world.resource::<Puzzle>().0.clone(),
            effects: world.resource::<Effects>().0.clone(),
        })));
        Ok(())
    }
    fn finish_tick(&self, world: &mut World) {
        world.resource_mut::<Effects>().0.clear();
    }
}
fn apply_rotations(
    inputs: Res<TickInputs<Rotate>>,
    mut puzzle: ResMut<Puzzle>,
    mut effects: ResMut<Effects>,
) {
    for input in &inputs.0 {
        if puzzle.0.rotate(input.command) {
            effects.0.push(RotationEffect {
                cell: input.command.cell,
                participant: input.participant,
            });
        }
    }
}
