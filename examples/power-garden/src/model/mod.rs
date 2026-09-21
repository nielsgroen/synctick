//! Integer-only circuit rules, validated startup data, and runtime state.
mod board;
mod connectivity;
mod initialization;
mod tile;

pub use board::{Board, Rotate};
pub use initialization::{Initialization, InitializationError};
pub use tile::{Kind, Tile};
