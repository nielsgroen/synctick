//! Authoritative runtime state, commands, and the board's connectivity cache.
use super::{
    connectivity::{connected_beacons, connectivity},
    initialization::{Initialization, InitializationError},
    tile::{Kind, Tile},
};
use synctick::{StableHash, Wire, stable_hash};

/// Rotate a cell clockwise once. Origin is supplied by the session framework.
#[derive(Clone, Copy, Debug, Wire)]
pub struct Rotate {
    /// Row-major cell index.
    pub cell: u32,
}

/// Complete puzzle state. Presentation reads this through immutable snapshots.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Board {
    state: PuzzleState,
    powered: [bool; 25],
}

/// Authoritative board state; connectivity is a recomputable cache in `Board`.
#[derive(Clone, Debug, PartialEq, Eq, StableHash)]
struct PuzzleState {
    width: u8,
    height: u8,
    tiles: Vec<Tile>,
    moves: u64,
    complete: bool,
}

impl TryFrom<Initialization> for PuzzleState {
    type Error = InitializationError;

    fn try_from(initial: Initialization) -> Result<Self, Self::Error> {
        initial.validate()?;
        let powered = connectivity(&initial.tiles, Tile::connections);
        let complete = connected_beacons(&initial.tiles, &powered) == 4;
        let Initialization {
            width,
            height,
            tiles,
        } = initial;
        Ok(Self {
            width,
            height,
            tiles,
            moves: 0,
            complete,
        })
    }
}

impl Board {
    /// Validate and consume a recorded setup, then build its connectivity cache.
    /// # Errors
    /// Rejects invalid dimensions, roles, ports, orientations, or unsolvable bases.
    pub fn new(initial: Initialization) -> Result<Self, InitializationError> {
        let state = PuzzleState::try_from(initial)?;
        let powered = connectivity(&state.tiles, Tile::connections);
        Ok(Self { state, powered })
    }

    /// Cells in stable row-major order.
    #[must_use]
    pub fn tiles(&self) -> &[Tile] {
        &self.state.tiles
    }
    /// Power connectivity in the same order as `tiles()`.
    #[must_use]
    pub const fn powered(&self) -> &[bool] {
        &self.powered
    }
    /// Count of accepted rotations.
    #[must_use]
    pub const fn moves(&self) -> u64 {
        self.state.moves
    }
    /// Whether all four beacons are powered.
    #[must_use]
    pub const fn complete(&self) -> bool {
        self.state.complete
    }
    /// Number of currently powered flowers.
    #[must_use]
    pub fn connected_beacons(&self) -> usize {
        connected_beacons(self.tiles(), &self.powered)
    }

    /// Apply a rotation atomically; illegal moves are deterministic no-ops.
    /// A saturated move counter also rejects further moves.
    pub fn rotate(&mut self, command: Rotate) -> bool {
        let Ok(index) = usize::try_from(command.cell) else {
            return false;
        };
        let Some(tile) = self.state.tiles.get_mut(index) else {
            return false;
        };
        if self.state.complete || tile.kind != Kind::Wire {
            return false;
        }
        let Some(moves) = self.state.moves.checked_add(1) else {
            return false;
        };
        tile.rotation = (tile.rotation + 1) % 4;
        self.state.moves = moves;
        self.recompute();
        true
    }

    fn recompute(&mut self) {
        self.powered = connectivity(self.tiles(), Tile::connections);
        self.state.complete = self.connected_beacons() == 4;
    }

    /// Stable FNV-1a fingerprint of tick and all rule-affecting state.
    #[must_use]
    pub fn state_hash(&self, tick: u64) -> u64 {
        stable_hash(&(tick, &self.state))
    }
}

#[cfg(test)]
mod tests {
    use super::super::tile::NORTH;
    use super::*;
    #[test]
    fn initialization_conversion_preserves_board_and_hash_layout() {
        for solved in [false, true] {
            let mut initial = Initialization::default();
            if solved {
                for tile in &mut initial.tiles {
                    tile.rotation = 0;
                }
            }
            // Flattening the former Initialization field must preserve hash bytes.
            let expected_hash = stable_hash(&(
                7u64,
                initial.width,
                initial.height,
                &initial.tiles,
                0u64,
                solved,
            ));
            let expected_tiles = initial.tiles.clone();
            let state = PuzzleState::try_from(initial).unwrap();
            assert_eq!(state.tiles, expected_tiles);
            assert_eq!((state.width, state.height), (5, 5));
            assert_eq!(state.moves, 0);
            assert_eq!(state.complete, solved);
            assert_eq!(stable_hash(&(7u64, &state)), expected_hash);
        }
    }
    #[test]
    fn hash_covers_authoritative_state_but_excludes_connectivity_cache() {
        let board = Board::new(Initialization::default()).unwrap();
        let hash = board.state_hash(1);
        assert_ne!(hash, board.state_hash(2));
        for edit in [
            |s: &mut PuzzleState| s.moves += 1,
            |s: &mut PuzzleState| s.complete = !s.complete,
            |s: &mut PuzzleState| s.width += 1,
            |s: &mut PuzzleState| s.height += 1,
            |s: &mut PuzzleState| s.tiles[1].kind = Kind::Empty,
            |s: &mut PuzzleState| s.tiles[1].ports ^= NORTH,
            |s: &mut PuzzleState| s.tiles[1].rotation += 1,
            |s: &mut PuzzleState| {
                s.tiles.pop();
            },
        ] {
            let mut changed = board.clone();
            edit(&mut changed.state);
            assert_ne!(hash, changed.state_hash(1));
        }
        let mut changed = board;
        changed.powered.fill(true);
        assert_eq!(hash, changed.state_hash(1));
    }
    #[test]
    fn rotation_and_reciprocal_connections() {
        let mut board = Board::new(Initialization::default()).unwrap();
        assert_eq!(board.powered().iter().filter(|on| **on).count(), 1);
        assert!(board.rotate(Rotate { cell: 7 }));
        assert!(board.powered()[7]);
        assert!(!board.powered()[1]); // horizontal neighbor still has vertical ports
    }
    #[test]
    fn invalid_moves_are_noops_and_completion_stops_play() {
        let mut board = Board::new(Initialization::default()).unwrap();
        let before = board.clone();
        for cell in [0, 6, 12, 25, u32::MAX] {
            assert!(!board.rotate(Rotate { cell }));
        }
        assert_eq!(board, before);
        for cell in 0..25 {
            if board.tiles()[cell].kind == Kind::Wire {
                while board.tiles()[cell].rotation != 0 && !board.complete() {
                    board.rotate(Rotate {
                        cell: u32::try_from(cell).unwrap(),
                    });
                }
            }
        }
        assert!(board.complete());
        let solved = board.clone();
        assert!(!board.rotate(Rotate { cell: 1 }));
        assert_eq!(board, solved);
    }
}
