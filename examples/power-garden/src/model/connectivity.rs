//! Reciprocal-port traversal shared by setup validation and live board updates.
use super::tile::{EAST, Kind, NORTH, SOUTH, Tile, WEST};

/// Traverse a validated board using either base ports or current orientations.
pub(super) fn connectivity(tiles: &[Tile], ports: impl Fn(Tile) -> u8) -> [bool; 25] {
    let mut powered = [false; 25];
    let source = tiles
        .iter()
        .position(|tile| tile.kind == Kind::Source)
        .expect("validated source");
    powered[source] = true;
    // Each cell is enqueued once, so a fixed-size queue covers the whole board.
    let mut queue = [source; 25];
    let mut head = 0;
    let mut tail = 1;
    while head < tail {
        let index = queue[head];
        head += 1;
        let outgoing_ports = ports(tiles[index]);
        let neighbors = [
            (NORTH, SOUTH, index.checked_sub(5)),
            (EAST, WEST, (index % 5 < 4).then_some(index + 1)),
            (SOUTH, NORTH, (index < 20).then_some(index + 5)),
            (
                WEST,
                EAST,
                if index % 5 > 0 { Some(index - 1) } else { None },
            ),
        ];
        for (outgoing, incoming, neighbor) in neighbors {
            if let Some(next) = neighbor
                && outgoing_ports & outgoing != 0
                && ports(tiles[next]) & incoming != 0
                && !powered[next]
            {
                powered[next] = true;
                queue[tail] = next;
                tail += 1;
            }
        }
    }
    powered
}

pub(super) fn connected_beacons(tiles: &[Tile], powered: &[bool]) -> usize {
    tiles
        .iter()
        .zip(powered)
        .filter(|(tile, on)| tile.kind == Kind::Beacon && **on)
        .count()
}
