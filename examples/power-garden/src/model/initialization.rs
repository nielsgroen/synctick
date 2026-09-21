//! Recorded board setup, default puzzle, and validation errors.
use super::{
    connectivity::{connected_beacons, connectivity},
    tile::{EAST, Kind, NORTH, SOUTH, Tile, WEST},
};
use synctick::Wire;

/// Complete initial board, stored before the command log.
#[derive(Clone, Debug, PartialEq, Eq, Wire)]
pub struct Initialization {
    /// Board width; this example supports exactly five columns.
    pub width: u8,
    /// Board height; this example supports exactly five rows.
    pub height: u8,
    /// Row-major cells, including fixed tiles and the scrambled orientations.
    pub tiles: Vec<Tile>,
}
impl Default for Initialization {
    fn default() -> Self {
        let mut tiles = vec![
            Tile {
                kind: Kind::Empty,
                ports: 0,
                rotation: 0
            };
            25
        ];
        for row in [0usize, 4] {
            tiles[row * 5] = Tile {
                kind: Kind::Beacon,
                ports: EAST,
                rotation: 0,
            };
            tiles[row * 5 + 4] = Tile {
                kind: Kind::Beacon,
                ports: WEST,
                rotation: 0,
            };
            for column in 1..4 {
                let ports = EAST
                    | WEST
                    | if column == 2 {
                        if row == 0 { SOUTH } else { NORTH }
                    } else {
                        0
                    };
                tiles[row * 5 + column] = Tile {
                    kind: Kind::Wire,
                    ports,
                    rotation: 1,
                };
            }
        }
        for index in [7, 17] {
            tiles[index] = Tile {
                kind: Kind::Wire,
                ports: NORTH | SOUTH,
                rotation: 1,
            };
        }
        tiles[12] = Tile {
            kind: Kind::Source,
            ports: NORTH | SOUTH,
            rotation: 0,
        };
        Self {
            width: 5,
            height: 5,
            tiles,
        }
    }
}

/// Why a recorded board cannot become a playable puzzle.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum InitializationError {
    /// Dimensions or cell count differ from the supported 5 x 5 board.
    #[error("Power Garden requires a 5 x 5 board")]
    Dimensions,
    /// A port mask or orientation contains an unsupported value.
    #[error("Invalid tile port mask or orientation")]
    TileEncoding,
    /// A tile's ports or orientation violate the rules for its role.
    #[error("Tile ports or orientation do not match its role")]
    TileRole,
    /// The board does not contain exactly one source and four beacons.
    #[error("A garden needs one source and four beacons")]
    Endpoints,
    /// The zero-rotation layout cannot power all four beacons.
    #[error("The unrotated board does not connect all four beacons")]
    Unsolvable,
}

impl Initialization {
    /// Check the supported board shape, tile roles, and a solvable base layout.
    pub(super) fn validate(&self) -> Result<(), InitializationError> {
        if self.width != 5 || self.height != 5 || self.tiles.len() != 25 {
            return Err(InitializationError::Dimensions);
        }
        let mut sources = 0;
        let mut beacons = 0;
        for tile in &self.tiles {
            if tile.ports > 15 || tile.rotation > 3 {
                return Err(InitializationError::TileEncoding);
            }
            let ports = tile.ports.count_ones();
            let valid = match tile.kind {
                Kind::Empty => ports == 0,
                Kind::Wire => matches!(ports, 2 | 3),
                Kind::Source => {
                    sources += 1;
                    ports > 0
                }
                Kind::Beacon => {
                    beacons += 1;
                    ports == 1
                }
            };
            if !valid || (tile.kind != Kind::Wire && tile.rotation != 0) {
                return Err(InitializationError::TileRole);
            }
        }
        if sources != 1 || beacons != 4 {
            return Err(InitializationError::Endpoints);
        }
        let solution = connectivity(&self.tiles, |tile| tile.ports);
        if connected_beacons(&self.tiles, &solution) != 4 {
            return Err(InitializationError::Unsolvable);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::Board;
    use super::*;
    #[test]
    fn rejects_invalid_initialization() {
        for defect in 0..7 {
            let mut init = Initialization::default();
            match defect {
                0 => init.width = 4,
                1 => {
                    init.tiles.pop();
                }
                2 => init.tiles[1].ports = 16,
                3 => init.tiles[1].rotation = 4,
                4 => init.tiles[0].rotation = 1,
                5 => init.tiles[12].kind = Kind::Wire,
                _ => init.tiles[7].ports = EAST | WEST,
            }
            let expected = match defect {
                0 | 1 => InitializationError::Dimensions,
                2 | 3 => InitializationError::TileEncoding,
                4 => InitializationError::TileRole,
                5 => InitializationError::Endpoints,
                _ => InitializationError::Unsolvable,
            };
            assert_eq!(init.validate(), Err(expected.clone()));
            assert_eq!(Board::new(init), Err(expected));
        }
    }
}
