//! Bounded startup transfer and the client replay state machine.
//! All clocks here govern transport/liveness, never simulation outcomes.
use crate::protocol::{
    CHANNEL_SAVE, CHANNEL_SAVE_MAX_MEM, ClientMessage, MAX_SAVE_CHUNK_BYTES, SaveMessage,
    SaveMetadata, encode,
};
use crate::replay::{ReplayReader, replay_batch};
use crate::session::{SessionControl, SessionError, SessionResult};
use crate::simulation::SessionSimulation;
use renet::{ClientId, RenetServer};
use std::collections::VecDeque;
use std::io::{BufReader, Cursor};
use std::time::{Duration, Instant};

// Renet 2.0's private packet::SLICE_SIZE. Its receive channel reserves the
// rounded size while assembling a fragmented message. Recheck on Renet upgrades;
// the packet-loss regression exercises this against the actual transport.
const RENET_SLICE_BYTES: usize = 1200;

const fn receive_memory(encoded_bytes: usize) -> usize {
    encoded_bytes.div_ceil(RENET_SLICE_BYTES) * RENET_SLICE_BYTES
}

pub const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, PartialEq, Eq)]
enum TransferPhase {
    Transferring,
    AwaitingReadiness,
    Ready,
}

pub struct StartupPeer {
    phase: TransferPhase,
    began: bool,
    offset: usize,
    received: u64,
    // (end offset in save, maximum Renet receive memory). Include Begin at 0;
    // the first consumed chunk also proves Begin has left the receive queue.
    outstanding: VecDeque<(usize, usize)>,
    receive_memory: usize,
    replayed: u64,
    last_progress: Instant,
}

impl StartupPeer {
    #[must_use]
    pub const fn new(now: Instant) -> Self {
        Self {
            phase: TransferPhase::Transferring,
            began: false,
            offset: 0,
            received: 0,
            outstanding: VecDeque::new(),
            receive_memory: 0,
            replayed: 0,
            last_progress: now,
        }
    }

    /// Bound both sender capacity and unconsumed receiver memory. Transport
    /// acknowledgements alone cannot release the latter: ordered messages can
    /// remain buffered behind a missing fragment after being acknowledged.
    pub fn pump(
        &mut self,
        server: &mut RenetServer,
        id: ClientId,
        save: &[u8],
        metadata: SaveMetadata,
    ) {
        if self.phase != TransferPhase::Transferring {
            return;
        }
        if !self.began {
            let message = encode(&SaveMessage::Begin(metadata));
            if !server.can_send_message(id, CHANNEL_SAVE, message.len()) {
                return;
            }
            let memory = receive_memory(message.len());
            server.send_message(id, CHANNEL_SAVE, message);
            self.outstanding.push_back((0, memory));
            self.receive_memory += memory;
            self.began = true;
        }
        while self.offset < save.len() {
            let end = self
                .offset
                .saturating_add(MAX_SAVE_CHUNK_BYTES)
                .min(save.len());
            if server.channel_available_memory(id, CHANNEL_SAVE) < end - self.offset {
                return;
            }
            let message = encode(&SaveMessage::Chunk {
                bytes: save[self.offset..end].to_vec(),
            });
            let memory = receive_memory(message.len());
            if memory > CHANNEL_SAVE_MAX_MEM - self.receive_memory
                || !server.can_send_message(id, CHANNEL_SAVE, message.len())
            {
                return;
            }
            server.send_message(id, CHANNEL_SAVE, message);
            self.outstanding.push_back((end, memory));
            self.receive_memory += memory;
            self.offset = end;
        }
        self.phase = TransferPhase::AwaitingReadiness;
    }

    /// # Errors
    /// Rejects premature commands, invalid progress, and mismatched readiness.
    pub fn receive(
        &mut self,
        message: ClientMessage,
        metadata: SaveMetadata,
        now: Instant,
    ) -> SessionResult {
        match message {
            ClientMessage::Progress {
                received_bytes,
                replayed_tick,
            } if self.phase != TransferPhase::Ready
                && received_bytes >= self.received
                && usize::try_from(received_bytes).is_ok_and(|received| {
                    received_bytes == self.received
                        || self.outstanding.iter().any(|(end, _)| *end == received)
                })
                && replayed_tick >= self.replayed
                && replayed_tick <= metadata.tick
                && (replayed_tick == 0 || received_bytes == metadata.total_bytes) =>
            {
                if received_bytes > self.received || replayed_tick > self.replayed {
                    self.last_progress = now;
                    self.received = received_bytes;
                    self.replayed = replayed_tick;
                    while let Some(&(end, memory)) = self.outstanding.front() {
                        if received_bytes == 0
                            || !u64::try_from(end).is_ok_and(|end| end <= received_bytes)
                        {
                            break;
                        }
                        self.outstanding.pop_front();
                        self.receive_memory -= memory;
                    }
                }
                Ok(())
            }
            ClientMessage::Ready { tick, hash }
                if self.phase == TransferPhase::AwaitingReadiness
                    && tick == metadata.tick
                    && hash == metadata.hash =>
            {
                self.phase = TransferPhase::Ready;
                self.last_progress = now;
                Ok(())
            }
            _ => Err(SessionError::Protocol(
                "invalid startup message or state fingerprint".into(),
            )),
        }
    }

    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.phase == TransferPhase::Ready
    }
    #[must_use]
    pub fn timed_out(&self, now: Instant) -> bool {
        !self.is_ready() && now.saturating_duration_since(self.last_progress) >= STARTUP_TIMEOUT
    }
}

type SaveReader = ReplayReader<BufReader<Cursor<Vec<u8>>>>;

enum ClientPhase {
    Receiving {
        metadata: Option<SaveMetadata>,
        bytes: Vec<u8>,
    },
    Replaying {
        metadata: SaveMetadata,
        reader: SaveReader,
    },
    AwaitingStart(SaveMetadata),
    Live,
    Failed,
}

pub struct ClientStartup {
    phase: ClientPhase,
    received: u64,
    replayed: u64,
    initialized: bool,
}

impl Default for ClientStartup {
    fn default() -> Self {
        Self {
            phase: ClientPhase::Receiving {
                metadata: None,
                bytes: Vec::new(),
            },
            received: 0,
            replayed: 0,
            initialized: false,
        }
    }
}

impl ClientStartup {
    /// # Errors
    /// Rejects out-of-order transfer messages, excess bytes, and invalid headers.
    pub fn receive_save(&mut self, message: SaveMessage) -> SessionResult {
        let ClientPhase::Receiving { metadata, bytes } = &mut self.phase else {
            return Err(SessionError::Protocol(
                "save data outside receiving phase".into(),
            ));
        };
        match message {
            SaveMessage::Begin(begin) if metadata.is_none() && begin.total_bytes > 0 => {
                usize::try_from(begin.total_bytes).map_err(|_| {
                    SessionError::Protocol("save too large for this platform".into())
                })?;
                *metadata = Some(begin);
            }
            SaveMessage::Chunk { bytes: chunk } => {
                let begin =
                    metadata.ok_or_else(|| SessionError::Protocol("chunk before Begin".into()))?;
                let size = u64::try_from(chunk.len())
                    .map_err(|_| SessionError::Protocol("chunk too large".into()))?;
                if chunk.is_empty()
                    || chunk.len() > MAX_SAVE_CHUNK_BYTES
                    || self
                        .received
                        .checked_add(size)
                        .is_none_or(|n| n > begin.total_bytes)
                {
                    return Err(SessionError::Protocol("invalid save chunk size".into()));
                }
                bytes
                    .try_reserve(chunk.len())
                    .map_err(|error| SessionError::Protocol(format!("save allocation: {error}")))?;
                bytes.extend_from_slice(&chunk);
                self.received += size;
            }
            SaveMessage::Begin(_) => {
                return Err(SessionError::Protocol("invalid or duplicate Begin".into()));
            }
        }
        if let Some(begin) = *metadata
            && self.received == begin.total_bytes
        {
            let data = std::mem::take(bytes);
            // Leave a terminal state if header decoding fails.
            self.phase = ClientPhase::Failed;
            let reader = ReplayReader::from_reader(BufReader::new(Cursor::new(data)))?;
            self.phase = ClientPhase::Replaying {
                metadata: begin,
                reader,
            };
        }
        Ok(())
    }

    /// Yield back to the caller's network loop after a bounded replay batch.
    /// # Errors
    /// Returns malformed replay or final-state mismatch errors.
    pub fn step(
        &mut self,
        sim: &mut impl SessionSimulation,
        control: &SessionControl,
    ) -> SessionResult<Option<ClientMessage>> {
        let ClientPhase::Replaying { metadata, reader } = &mut self.phase else {
            return Ok(None);
        };
        if !self.initialized {
            sim.initialize(reader.header())?;
            self.initialized = true;
        }
        let batch = replay_batch(sim, reader, None, control)?;
        self.replayed = sim.current_tick();
        if self.replayed > metadata.tick {
            return Err(SessionError::Protocol(
                "replay exceeds declared final tick".into(),
            ));
        }
        if batch.finished {
            let expected = *metadata;
            if self.replayed != expected.tick || sim.state_hash() != expected.hash {
                return Err(SessionError::Protocol(
                    "loaded save fingerprint mismatch".into(),
                ));
            }
            self.phase = ClientPhase::AwaitingStart(expected);
            return Ok(Some(ClientMessage::Ready {
                tick: expected.tick,
                hash: expected.hash,
            }));
        }
        Ok(None)
    }

    /// # Errors
    /// Rejects premature/duplicate Start and fingerprints differing from Ready.
    pub fn start(&mut self, tick: u64, hash: u64) -> SessionResult {
        match self.phase {
            ClientPhase::AwaitingStart(expected)
                if tick == expected.tick && hash == expected.hash =>
            {
                self.phase = ClientPhase::Live;
                Ok(())
            }
            _ => Err(SessionError::Protocol("invalid Start".into())),
        }
    }

    #[must_use]
    pub const fn is_live(&self) -> bool {
        matches!(self.phase, ClientPhase::Live)
    }
    #[must_use]
    pub const fn is_loading(&self) -> bool {
        matches!(
            self.phase,
            ClientPhase::Receiving { .. } | ClientPhase::Replaying { .. }
        )
    }
    #[must_use]
    pub const fn progress(&self) -> (u64, u64) {
        (self.received, self.replayed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::CommandPayload;
    use crate::protocol::{
        CHANNEL_LOCKSTEP, ServerMessage, connection_config, decode_save_message,
        decode_server_message,
    };

    #[test]
    fn large_transfer_obeys_capacity_and_readiness() {
        let mut server = RenetServer::new(connection_config());
        let mut client = server.new_local_client(1);
        let mut bytes = encode(&crate::replay::ReplayHeader::current());
        for tick in 1..=600_000 {
            let payload = encode(&crate::protocol::TickAdvance {
                tick,
                inputs: vec![],
            });
            bytes.extend_from_slice(&u32::try_from(payload.len()).unwrap().to_le_bytes());
            bytes.extend(payload);
        }
        assert!(bytes.len() > 4 * 1024 * 1024);
        let meta = SaveMetadata {
            total_bytes: u64::try_from(bytes.len()).unwrap(),
            tick: 600_000,
            hash: 42,
        };
        let now = Instant::now();
        let mut peer = StartupPeer::new(now);
        peer.pump(&mut server, 1, &bytes, meta);
        assert!(peer.offset < bytes.len());
        for _ in 0..5 {
            peer.pump(&mut server, 1, &bytes, meta);
        }
        assert!(server.disconnect_reason(1).is_none());
        let mut received = Vec::new();
        for _ in 0..2000 {
            server.update(Duration::from_millis(10));
            client.update(Duration::from_millis(10));
            server.process_local_client(1, &mut client).unwrap();
            while let Some(message) = client.receive_message(CHANNEL_SAVE) {
                if let SaveMessage::Chunk { bytes } = decode_save_message(&message).unwrap() {
                    received.extend(bytes);
                }
            }
            peer.receive(
                ClientMessage::Progress {
                    received_bytes: u64::try_from(received.len()).unwrap(),
                    replayed_tick: 0,
                },
                meta,
                now,
            )
            .unwrap();
            peer.pump(&mut server, 1, &bytes, meta);
            if received.len() == bytes.len() {
                break;
            }
        }
        assert_eq!(received, bytes);
        assert!(!peer.is_ready());
        peer.receive(
            ClientMessage::Ready {
                tick: 600_000,
                hash: 42,
            },
            meta,
            now,
        )
        .unwrap();
        assert!(peer.is_ready());
        server.send_message(
            1,
            CHANNEL_LOCKSTEP,
            encode(&ServerMessage::Start {
                tick: 600_000,
                hash: 42,
            }),
        );
        server.send_message(
            1,
            CHANNEL_LOCKSTEP,
            encode(&ServerMessage::Tick(crate::protocol::TickAdvance {
                tick: 600_001,
                inputs: vec![],
            })),
        );
        server.process_local_client(1, &mut client).unwrap();
        assert!(matches!(
            decode_server_message(&client.receive_message(CHANNEL_LOCKSTEP).unwrap()).unwrap(),
            ServerMessage::Start { .. }
        ));
        assert!(matches!(
            decode_server_message(&client.receive_message(CHANNEL_LOCKSTEP).unwrap()).unwrap(),
            ServerMessage::Tick(_)
        ));
    }

    /// Withhold an early fragment while acknowledging later messages. Those
    /// transport ACKs must not allow the sender to overrun the ordered receiver.
    #[test]
    fn transfer_survives_packet_loss_and_delayed_consumption_reports() {
        use crate::protocol::decode_client_message;

        let mut bytes = encode(&crate::replay::ReplayHeader::current());
        for tick in 1..=1_000_000 {
            let record = encode(&crate::protocol::TickAdvance {
                tick,
                inputs: vec![],
            });
            bytes.extend_from_slice(&u32::try_from(record.len()).unwrap().to_le_bytes());
            bytes.extend(record);
        }
        assert!(bytes.len() > 2 * CHANNEL_SAVE_MAX_MEM);
        // Also withhold Begin: all subsequent complete chunks then remain queued.
        for lose_begin in [false, true] {
            let mut server = RenetServer::new(connection_config());
            let mut client = server.new_local_client(1);
            let now = Instant::now();
            let meta = SaveMetadata {
                total_bytes: u64::try_from(bytes.len()).unwrap(),
                tick: 1_000_000,
                hash: 42,
            };
            let mut peer = StartupPeer::new(now);
            let mut received = Vec::new();
            let mut reported = 0;
            for iteration in 0..2000 {
                server.update(Duration::from_millis(5));
                client.update(Duration::from_millis(5));
                peer.pump(&mut server, 1, &bytes, meta);
                for packet in server.get_packets_to_send(1).unwrap() {
                    // Renet 2.0 packet format (not the game's protocol codec).
                    let mut header = octets::Octets::with_slice(&packet);
                    let kind = header.get_u8().unwrap();
                    let _sequence = header.get_varint().unwrap();
                    let blocked = if kind == 2 {
                        let channel = header.get_u8().unwrap();
                        let message = header.get_varint().unwrap();
                        let slice = header.get_varint().unwrap();
                        !lose_begin && channel == CHANNEL_SAVE && message == 1 && slice == 0
                    } else {
                        lose_begin && kind == 0 && header.get_u8().unwrap() == CHANNEL_SAVE
                    };
                    if !(blocked && iteration < 200) {
                        client.process_packet(&packet);
                    }
                }
                while let Some(message) = client.receive_message(CHANNEL_SAVE) {
                    if let SaveMessage::Chunk { bytes } = decode_save_message(&message).unwrap() {
                        received.extend(bytes);
                    }
                }
                // Delay application acknowledgements beyond fragment recovery.
                if iteration >= 300 && received.len() != reported {
                    reported = received.len();
                    client.send_message(
                        CHANNEL_LOCKSTEP,
                        encode(&ClientMessage::Progress {
                            received_bytes: u64::try_from(reported).unwrap(),
                            replayed_tick: 0,
                        }),
                    );
                }
                for packet in client.get_packets_to_send() {
                    server.process_packet_from(&packet, 1).unwrap();
                }
                while let Some(message) = server.receive_message(1, CHANNEL_LOCKSTEP) {
                    peer.receive(decode_client_message(&message).unwrap(), meta, now)
                        .unwrap();
                }
                assert!(
                    !client.is_disconnected(),
                    "{:?}",
                    client.disconnect_reason()
                );
                assert!(server.is_connected(1));
                assert!(peer.receive_memory <= CHANNEL_SAVE_MAX_MEM);
                if iteration == 199 {
                    assert!(received.is_empty());
                    assert!(peer.offset < bytes.len());
                }
                if iteration == 299 {
                    assert!(
                        peer.offset < bytes.len(),
                        "transport ACKs released the receive window"
                    );
                }
                if received.len() == bytes.len() {
                    break;
                }
            }
            assert_eq!(received, bytes);
        }
    }

    #[test]
    fn consumption_reports_release_only_complete_sent_messages() {
        let now = Instant::now();
        let mut server = RenetServer::new(connection_config());
        let _client = server.new_local_client(1);
        let bytes = vec![0; 2 * MAX_SAVE_CHUNK_BYTES + 1];
        let meta = SaveMetadata {
            total_bytes: u64::try_from(bytes.len()).unwrap(),
            tick: 0,
            hash: 0,
        };
        let mut peer = StartupPeer::new(now);
        peer.pump(&mut server, 1, &bytes, meta);
        let initial = peer.receive_memory;
        let progress = |received_bytes| ClientMessage::Progress {
            received_bytes,
            replayed_tick: 0,
        };
        assert!(peer.receive(progress(1), meta, now).is_err());
        assert!(
            peer.receive(progress(meta.total_bytes + 1), meta, now)
                .is_err()
        );
        assert_eq!(peer.receive_memory, initial);
        peer.receive(
            progress(u64::try_from(MAX_SAVE_CHUNK_BYTES).unwrap()),
            meta,
            now,
        )
        .unwrap();
        assert!(peer.receive_memory < initial);
        let remaining = peer.receive_memory;
        peer.receive(
            progress(u64::try_from(MAX_SAVE_CHUNK_BYTES).unwrap()),
            meta,
            now,
        )
        .unwrap();
        assert_eq!(peer.receive_memory, remaining);
        assert!(peer.receive(progress(0), meta, now).is_err());
        peer.receive(progress(meta.total_bytes), meta, now).unwrap();
        assert_eq!(peer.receive_memory, 0);
        assert!(peer.outstanding.is_empty());
    }

    #[test]
    fn rejects_invalid_startup_and_times_out_only_without_progress() {
        let now = Instant::now();
        let meta = SaveMetadata {
            total_bytes: 10,
            tick: 2,
            hash: 3,
        };
        let mut peer = StartupPeer::new(now);
        assert!(
            peer.receive(ClientMessage::Input(CommandPayload(vec![0])), meta, now)
                .is_err()
        );
        assert!(
            peer.receive(ClientMessage::Ready { tick: 2, hash: 3 }, meta, now)
                .is_err()
        );
        assert!(peer.timed_out(now + STARTUP_TIMEOUT));
        peer.offset = 10;
        peer.outstanding.push_back((10, 1200));
        peer.receive_memory = 1200;
        peer.phase = TransferPhase::AwaitingReadiness;
        peer.receive(
            ClientMessage::Progress {
                received_bytes: 10,
                replayed_tick: 0,
            },
            meta,
            now + Duration::from_secs(20),
        )
        .unwrap();
        assert!(!peer.timed_out(now + STARTUP_TIMEOUT));
        peer.receive(
            ClientMessage::Progress {
                received_bytes: 10,
                replayed_tick: 0,
            },
            meta,
            now + Duration::from_secs(40),
        )
        .unwrap();
        assert!(peer.timed_out(now + Duration::from_secs(51)));
        assert!(
            peer.receive(ClientMessage::Ready { tick: 2, hash: 4 }, meta, now)
                .is_err()
        );
    }

    #[test]
    fn receiver_rejects_out_of_order_and_excess_data() {
        let mut client = ClientStartup::default();
        assert!(
            client
                .receive_save(SaveMessage::Chunk { bytes: vec![0] })
                .is_err()
        );
        assert!(client.start(0, 0).is_err());
        let meta = SaveMetadata {
            total_bytes: 1,
            tick: 0,
            hash: 0,
        };
        client.receive_save(SaveMessage::Begin(meta)).unwrap();
        assert!(client.receive_save(SaveMessage::Begin(meta)).is_err());
        assert!(
            client
                .receive_save(SaveMessage::Chunk { bytes: vec![0, 1] })
                .is_err()
        );
        assert!(
            client
                .receive_save(SaveMessage::Chunk { bytes: vec![0] })
                .is_err()
        );
    }
    #[test]
    fn incremental_join_matches_authority_and_requires_start() {
        let mut bytes = encode(&crate::replay::ReplayHeader::current());
        for tick in 1..=300 {
            let payload = encode(&crate::protocol::TickAdvance {
                tick,
                inputs: if tick % 30 == 0 {
                    vec![CommandPayload(vec![0]).into()]
                } else {
                    vec![]
                },
            });
            bytes.extend_from_slice(&u32::try_from(payload.len()).unwrap().to_le_bytes());
            bytes.extend(payload);
        }
        let mut authority = crate::test_simulation::TestSimulation::default();
        let mut reader = ReplayReader::from_reader(Cursor::new(bytes.clone())).unwrap();
        crate::replay::replay_into(
            &mut authority,
            &mut reader,
            None,
            &SessionControl::default(),
        )
        .unwrap();
        let metadata = SaveMetadata {
            total_bytes: u64::try_from(bytes.len()).unwrap(),
            tick: 300,
            hash: authority.state_hash(),
        };
        let mut sim = crate::test_simulation::TestSimulation::default();
        let mut startup = ClientStartup::default();
        startup.receive_save(SaveMessage::Begin(metadata)).unwrap();
        startup.receive_save(SaveMessage::Chunk { bytes }).unwrap();
        let control = SessionControl::default();
        assert!(startup.step(&mut sim, &control).unwrap().is_none());
        let tick = sim.current_tick();
        assert!(tick <= 128);
        control.cancel();
        assert!(startup.step(&mut sim, &control).unwrap().is_none());
        assert_eq!(sim.current_tick(), tick);
        let control = SessionControl::default();
        loop {
            // Each step returns to the caller, where networking is serviced.
            if let Some(ClientMessage::Ready { tick, hash }) =
                startup.step(&mut sim, &control).unwrap()
            {
                assert_eq!(tick, metadata.tick);
                assert_eq!(hash, metadata.hash);
                break;
            }
        }
        assert!(!startup.is_live());
        assert!(startup.start(metadata.tick, 0).is_err());
        startup.start(metadata.tick, metadata.hash).unwrap();
        assert!(startup.is_live());
        assert!(startup.start(metadata.tick, metadata.hash).is_err());
        assert!(startup.receive_save(SaveMessage::Begin(metadata)).is_err());
    }
}
