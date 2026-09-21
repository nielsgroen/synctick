//! Tile roles, stable tags, and clockwise port rotation.
use synctick::{StableHash, Wire};

pub(super) const NORTH: u8 = 1;
pub(super) const EAST: u8 = 2;
pub(super) const SOUTH: u8 = 4;
pub(super) const WEST: u8 = 8;

/// A fixed tile role. Numeric wire tags are stable across saves.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Wire, StableHash)]
pub enum Kind {
    /// Inert background, without ports.
    #[wire(tag = 0)]
    Empty,
    /// Player-rotatable wire.
    #[wire(tag = 1)]
    Wire,
    /// Fixed power origin.
    #[wire(tag = 2)]
    Source,
    /// Fixed flower endpoint.
    #[wire(tag = 3)]
    Beacon,
}
/// One cell's recorded role, unrotated connections, and orientation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Wire, StableHash)]
pub struct Tile {
    /// Fixed gameplay role.
    pub kind: Kind,
    /// Four low bits: north, east, south, west.
    pub ports: u8,
    /// Clockwise quarter turns, in 0..4. Fixed tiles require zero.
    pub rotation: u8,
}
impl Tile {
    /// Port mask after clockwise rotation.
    #[must_use]
    pub const fn connections(self) -> u8 {
        let rotation = self.rotation % 4;
        ((self.ports << rotation) | (self.ports >> (4 - rotation))) & 15
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use synctick::codec;
    #[test]
    fn rotates_ports_clockwise() {
        let tile = Tile {
            kind: Kind::Wire,
            ports: NORTH | EAST,
            rotation: 1,
        };
        assert_eq!(tile.connections(), EAST | SOUTH);
    }

    #[test]
    fn kind_tags_preserve_recorded_bytes() {
        for (kind, tag) in [
            (Kind::Empty, 0),
            (Kind::Wire, 1),
            (Kind::Source, 2),
            (Kind::Beacon, 3),
        ] {
            assert_eq!(codec::encode(&kind).unwrap(), [tag]);
            assert_eq!(codec::decode::<Kind>(&[tag]).unwrap(), kind);
        }
        assert!(codec::decode::<Kind>(&[4]).is_err());
    }
}
