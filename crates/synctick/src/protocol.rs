//! Wire protocol for synchronous-lockstep multiplayer.
//!
//! Ordered startup envelopes gate live ticks on a verified initial state.
//! Lockstep, client desync reports, and save chunks use separate reliable
//! ordered channels. Start and Tick share one server-to-client stream.

use crate::ParticipantId;
use renet::{ChannelConfig, ConnectionConfig, SendType};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Framework protocol. Game compatibility is checked independently in the header.
pub const PROTOCOL_ID: u64 = 0x5345_5353_494F_0001;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandPayload(pub Vec<u8>);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Command {
    pub participant: ParticipantId,
    pub payload: CommandPayload,
}

#[cfg(test)]
impl From<CommandPayload> for Command {
    fn from(payload: CommandPayload) -> Self {
        Self {
            participant: ParticipantId::Host,
            payload,
        }
    }
}

/// Channel id for `ClientMessage` and `ServerMessage`.
///
/// Both directions live on the same channel because they're a single
/// conversation: a client's input is acknowledged by appearing in a
/// future `TickAdvance`.
pub const CHANNEL_LOCKSTEP: u8 = 0;

/// Channel id for client-to-server `DesyncHash` reports.
pub const CHANNEL_DESYNC: u8 = 1;

/// Channel id for `SaveMessage` (server→client).
///
/// Carries the session-so-far to a joining client before the live
/// `TickAdvance` stream begins. Reliable-ordered because the save must
/// arrive intact and in order, and chunked because a session log can
/// grow past the per-channel memory cap.
pub const CHANNEL_SAVE: u8 = 2;

/// Per-channel buffer cap for `LOCKSTEP` and `DESYNC`. Reliable
/// channels disconnect a peer when the buffer reaches this size — the
/// cap exists to bound memory if a peer stalls. 1 MiB easily holds
/// many seconds of our (tens-of-bytes-per-tick) traffic.
const CHANNEL_MAX_MEM: usize = 1024 * 1024;

/// Per-channel buffer cap for `SAVE`. Larger because a save chunk is
/// up to 256 KiB and we want headroom for several to be in flight
/// while the receive side is replaying. 4 MiB ≈ 16 chunks ≈ several
/// hours of idle session at typical rates.
pub const CHANNEL_SAVE_MAX_MEM: usize = 4 * 1024 * 1024;

/// Maximum replay record allocation, matching the lockstep channel budget.
pub const MAX_REPLAY_RECORD_BYTES: usize = 256 * 1024;

/// Maximum payload size of a single `SaveMessage::Chunk`. Sender
/// enforces this; receiver does not assume any particular size.
pub const MAX_SAVE_CHUNK_BYTES: usize = 256 * 1024;

/// Retransmit interval for unacknowledged reliable messages. 100 ms is
/// a reasonable LAN/local default; tune later if we see retransmit storms
/// on lossy links.
const RESEND_TIME: Duration = Duration::from_millis(100);

/// Startup messages and commands share an ordered stream in each direction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClientMessage {
    Input(CommandPayload),
    Progress {
        received_bytes: u64,
        replayed_tick: u64,
    },
    Ready {
        tick: u64,
        hash: u64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServerMessage {
    Start { tick: u64, hash: u64 },
    Tick(TickAdvance),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaveMetadata {
    pub total_bytes: u64,
    pub tick: u64,
    pub hash: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TickAdvance {
    pub tick: u64,
    pub inputs: Vec<Command>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct DesyncHash {
    pub tick: u64,
    pub hash: u64,
}

/// Save-distribution protocol.
///
/// A `Begin` carries the total payload size, followed by one or more
/// `Chunk`s totalling exactly that many bytes. The receiver is in
/// joining state until it has accumulated `total_bytes`, at which
/// point it parses the buffer through `ReplayReader::from_reader`
/// (same code path as a recorded log on disk) and replays into its
/// local sim app.
///
/// No explicit "done" terminator: the receiver knows the transfer is
/// complete when `accumulated.len() == total_bytes`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SaveMessage {
    Begin(SaveMetadata),
    Chunk { bytes: Vec<u8> },
}

/// Build the renet `ConnectionConfig` shared by server and client. Both
/// sides must agree on the channel layout — same ids, same delivery
/// guarantees, same cap — or messages will land on the wrong channel.
#[must_use]
pub fn connection_config() -> ConnectionConfig {
    let channels = vec![
        ChannelConfig {
            channel_id: CHANNEL_LOCKSTEP,
            max_memory_usage_bytes: CHANNEL_MAX_MEM,
            send_type: SendType::ReliableOrdered {
                resend_time: RESEND_TIME,
            },
        },
        ChannelConfig {
            channel_id: CHANNEL_DESYNC,
            max_memory_usage_bytes: CHANNEL_MAX_MEM,
            send_type: SendType::ReliableOrdered {
                resend_time: RESEND_TIME,
            },
        },
        ChannelConfig {
            channel_id: CHANNEL_SAVE,
            max_memory_usage_bytes: CHANNEL_SAVE_MAX_MEM,
            send_type: SendType::ReliableOrdered {
                resend_time: RESEND_TIME,
            },
        },
    ];
    ConnectionConfig {
        available_bytes_per_tick: 60_000,
        server_channels_config: channels.clone(),
        client_channels_config: channels,
    }
}

/// Encode a wire message.
///
/// `bincode 2` with the `standard` config: little-endian, varint
/// integer encoding, no size limit. Deterministic for a given input as
/// long as both peers share the same config — which they do, because
/// both sides go through this function.
///
/// # Panics
///
/// Encoding is infallible for the `Serialize`-shapes used by this
/// protocol; the inner `Result` is unwrapped with `expect`. A panic
/// here would indicate a `Serialize` impl that fails on an in-range
/// value, which is a programmer error.
pub fn encode<T: Serialize>(msg: &T) -> Vec<u8> {
    bincode::serde::encode_to_vec(msg, bincode::config::standard())
        .expect("bincode encode of a known-shape message cannot fail")
}

// Validate the complete shape before materializing collections. Scalar reads
// deliberately use bincode itself so its standard varint rules remain canonical.
type DecodeResult<T> = Result<T, bincode::error::DecodeError>;

struct Cursor<'a> {
    remaining: &'a [u8],
    materialize: bool,
}

impl Cursor<'_> {
    fn scalar<T: for<'de> Deserialize<'de>>(&mut self) -> DecodeResult<T> {
        let (value, consumed) =
            bincode::serde::decode_from_slice(self.remaining, bincode::config::standard())?;
        self.remaining = self
            .remaining
            .get(consumed..)
            .ok_or_else(|| invalid("scalar length"))?;
        Ok(value)
    }

    fn length(&mut self) -> DecodeResult<usize> {
        let length: u64 = self.scalar()?;
        let length =
            usize::try_from(length).map_err(|_| invalid("length exceeds platform size"))?;
        if length > self.remaining.len() {
            return Err(invalid("collection length exceeds remaining payload"));
        }
        Ok(length)
    }

    fn payload(&mut self) -> DecodeResult<CommandPayload> {
        let length = self.length()?;
        if length > crate::codec::MAX_PAYLOAD_BYTES {
            return Err(invalid("command byte budget exceeded"));
        }
        let (bytes, rest) = self.remaining.split_at(length);
        self.remaining = rest;
        let mut payload = Vec::new();
        if self.materialize {
            payload
                .try_reserve_exact(length)
                .map_err(|_| invalid("command allocation failed"))?;
            payload.extend_from_slice(bytes);
        }
        Ok(CommandPayload(payload))
    }

    fn tick(&mut self) -> DecodeResult<TickAdvance> {
        let tick = self.scalar()?;
        let count = self.length()?;
        let mut inputs = Vec::new();
        if self.materialize {
            inputs
                .try_reserve_exact(count)
                .map_err(|_| invalid("input allocation failed"))?;
        }
        for _ in 0..count {
            let participant = self.scalar()?;
            let payload = self.payload()?;
            let event = Command {
                participant,
                payload,
            };
            if self.materialize {
                inputs.push(event);
            }
        }
        Ok(TickAdvance { tick, inputs })
    }
}

const fn invalid(reason: &'static str) -> bincode::error::DecodeError {
    bincode::error::DecodeError::Other(reason)
}

fn validated<'a, T>(
    bytes: &'a [u8],
    read: impl Fn(&mut Cursor<'a>) -> DecodeResult<T>,
) -> DecodeResult<T> {
    for materialize in [false, true] {
        let mut cursor = Cursor {
            remaining: bytes,
            materialize,
        };
        let value = read(&mut cursor)?;
        if !cursor.remaining.is_empty() {
            return Err(invalid("trailing bytes"));
        }
        if materialize {
            return Ok(value);
        }
    }
    unreachable!("two passes always include materialization")
}

/// Decode a client envelope without trusting collection lengths.
/// # Errors
/// Rejects malformed, truncated, or trailing data.
pub fn decode_client_message(bytes: &[u8]) -> DecodeResult<ClientMessage> {
    validated(bytes, |cursor| match cursor.scalar::<u32>()? {
        0 => Ok(ClientMessage::Input(cursor.payload()?)),
        1 => Ok(ClientMessage::Progress {
            received_bytes: cursor.scalar()?,
            replayed_tick: cursor.scalar()?,
        }),
        2 => Ok(ClientMessage::Ready {
            tick: cursor.scalar()?,
            hash: cursor.scalar()?,
        }),
        _ => Err(invalid("invalid client message variant")),
    })
}

/// Decode a server envelope after validating its complete input sequence.
/// # Errors
/// Rejects malformed, truncated, or trailing data and allocation failures.
pub fn decode_server_message(bytes: &[u8]) -> DecodeResult<ServerMessage> {
    if bytes.len() > MAX_REPLAY_RECORD_BYTES {
        return Err(invalid("tick byte budget exceeded"));
    }
    validated(bytes, |cursor| match cursor.scalar::<u32>()? {
        0 => Ok(ServerMessage::Start {
            tick: cursor.scalar()?,
            hash: cursor.scalar()?,
        }),
        1 => Ok(ServerMessage::Tick(cursor.tick()?)),
        _ => Err(invalid("invalid server message variant")),
    })
}

/// Decode a save envelope, checking chunk structure before allocation.
/// # Errors
/// Rejects malformed, oversized, truncated, or trailing data and allocation failures.
pub fn decode_save_message(bytes: &[u8]) -> DecodeResult<SaveMessage> {
    validated(bytes, |cursor| match cursor.scalar::<u32>()? {
        0 => Ok(SaveMessage::Begin(SaveMetadata {
            total_bytes: cursor.scalar()?,
            tick: cursor.scalar()?,
            hash: cursor.scalar()?,
        })),
        1 => {
            let length = cursor.length()?;
            if length == 0 || length > MAX_SAVE_CHUNK_BYTES {
                return Err(invalid("invalid save chunk size"));
            }
            let (payload, rest) = cursor.remaining.split_at(length);
            cursor.remaining = rest;
            let mut bytes = Vec::new();
            if cursor.materialize {
                bytes
                    .try_reserve_exact(length)
                    .map_err(|_| invalid("chunk allocation failed"))?;
                bytes.extend_from_slice(payload);
            }
            Ok(SaveMessage::Chunk { bytes })
        }
        _ => Err(invalid("invalid save message variant")),
    })
}

/// Decode a fixed-shape hash report.
/// # Errors
/// Rejects malformed, truncated, or trailing data.
pub fn decode_desync_hash(bytes: &[u8]) -> DecodeResult<DesyncHash> {
    validated(bytes, |cursor| {
        Ok(DesyncHash {
            tick: cursor.scalar()?,
            hash: cursor.scalar()?,
        })
    })
}

/// Decode a replay tick after validating the complete input sequence.
/// # Errors
/// Rejects malformed, truncated, or trailing data and allocation failures.
pub fn decode_tick_advance(bytes: &[u8]) -> DecodeResult<TickAdvance> {
    if bytes.len() > MAX_REPLAY_RECORD_BYTES {
        return Err(invalid("tick byte budget exceeded"));
    }
    validated(bytes, Cursor::tick)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_trailing_bytes() {
        let mut bytes = encode(&ClientMessage::Ready { tick: 0, hash: 1 });
        bytes.push(0);
        assert!(decode_client_message(&bytes).is_err());
    }
    #[test]
    fn framework_protocol_fixtures() {
        macro_rules! fixture {
            ($reader:ident, $value:expr, $bytes:expr) => {{
                let bytes: &[u8] = $bytes;
                assert_eq!(encode(&$value), bytes);
                assert_eq!(encode(&$reader(bytes).unwrap()), bytes);
                for end in 0..bytes.len() {
                    assert!($reader(&bytes[..end]).is_err(), "truncation at {end}");
                }
                let mut trailing = bytes.to_vec();
                trailing.push(0);
                assert!($reader(&trailing).is_err());
            }};
        }
        fixture!(
            decode_client_message,
            ClientMessage::Input(CommandPayload(vec![0])),
            &[0, 1, 0]
        );
        fixture!(
            decode_client_message,
            ClientMessage::Progress {
                received_bytes: 251,
                replayed_tick: 1
            },
            &[1, 251, 251, 0, 1]
        );
        fixture!(
            decode_client_message,
            ClientMessage::Ready { tick: 1, hash: 2 },
            &[2, 1, 2]
        );
        fixture!(
            decode_server_message,
            ServerMessage::Start { tick: 1, hash: 2 },
            &[0, 1, 2]
        );
        fixture!(
            decode_server_message,
            ServerMessage::Tick(TickAdvance {
                tick: 1,
                inputs: vec![CommandPayload(vec![0]).into(); 2]
            }),
            &[1, 1, 2, 0, 1, 0, 0, 1, 0]
        );
        fixture!(
            decode_save_message,
            SaveMessage::Begin(SaveMetadata {
                total_bytes: 3,
                tick: 1,
                hash: 2
            }),
            &[0, 3, 1, 2]
        );
        fixture!(
            decode_save_message,
            SaveMessage::Chunk {
                bytes: vec![10, 20]
            },
            &[1, 2, 10, 20]
        );
        fixture!(decode_desync_hash, DesyncHash { tick: 1, hash: 2 }, &[1, 2]);
        fixture!(
            decode_tick_advance,
            TickAdvance {
                tick: 1,
                inputs: vec![CommandPayload(vec![0]).into(); 2]
            },
            &[1, 2, 0, 1, 0, 0, 1, 0]
        );
    }

    #[test]
    fn malformed_collections_never_reach_materialization() {
        let mut huge = vec![1]; // tick
        huge.extend(encode(&u64::MAX));
        for bytes in [huge, vec![1, 2, 0], vec![1, 1, 99], vec![1, 0, 0]] {
            let result = validated(&bytes, |cursor| {
                assert!(
                    !cursor.materialize,
                    "malformed data reached allocation pass"
                );
                cursor.tick()
            });
            assert!(result.is_err());
        }
        for bytes in [&[1, 2, 0][..], &[1, 0], &[1, 1, 0, 0], &[2], &[255]] {
            assert!(decode_save_message(bytes).is_err());
        }
        assert!(decode_client_message(&[3]).is_err());
        assert!(decode_client_message(&[0, 1]).is_err());
        assert!(decode_server_message(&[2]).is_err());
        let oversized = encode(&SaveMessage::Chunk {
            bytes: vec![0; MAX_SAVE_CHUNK_BYTES + 1],
        });
        assert!(decode_save_message(&oversized).is_err());
        let maximum = encode(&SaveMessage::Chunk {
            bytes: vec![0; MAX_SAVE_CHUNK_BYTES],
        });
        assert!(decode_save_message(&maximum).is_ok());
    }
}
