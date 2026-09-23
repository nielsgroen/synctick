//! Checkpoint-backed lobbies over reliable UDP.
//!
//! Identity, organizer authority,
//! synchronization epochs, and gameplay origins are separate from game seats.
//! Identities identify installations, not authenticated accounts.
use crate::protocol::{CommandPayload, connection_config};
use crate::session::SessionWorker;
use crate::{
    CommandSender, Game, Input, ParticipantId, SessionControl, SessionError, SessionHandle,
    SessionResult, SessionStatus, Simulation, SubmitError, Tick, codec,
};
use arc_swap::ArcSwapOption;
use crossbeam_channel::{Receiver, Sender, bounded};
use renet::{RenetClient, RenetServer, ServerEvent};
use renet_netcode::{
    ClientAuthentication, NetcodeClientTransport, NetcodeServerTransport, ServerAuthentication,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    marker::PhantomData,
    net::{SocketAddr, UdpSocket},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const PROTOCOL: u64 = 0x5359_4e43_4c42_0002;
const LIMIT: usize = 256 * 1024;
const CHANNEL: u8 = 2;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub id: u64,
    pub name: String,
}
impl Identity {
    /// # Errors
    /// Rejects zero IDs, empty names, control characters, and names over 64 bytes.
    pub fn validate(&self) -> SessionResult {
        if self.id == 0
            || self.name.trim().is_empty()
            || self.name.len() > 64
            || self.name.chars().any(char::is_control)
        {
            return Err(SessionError::Configuration(
                "invalid player identity or name".into(),
            ));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    pub identity: Identity,
    pub participant: ParticipantId,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lobby {
    /// Connection order; never game/seat order.
    pub members: Vec<Member>,
    pub organizer: Option<u64>,
    pub epoch: u64,
    pub started: bool,
    pub live: bool,
    pub ready: Vec<u64>,
}
/// Game-owned checkpoints and lobby policy. Called only on the worker.
///
/// Configuration must be transactional on error. `start` may shuffle/validate
/// seats; `None` updates connection bindings without changing seat ownership.
pub trait CheckpointSimulation<C>: Simulation<C> {
    /// # Errors
    /// Return serialization or checkpoint validation failures.
    fn checkpoint(&mut self) -> SessionResult<Vec<u8>>;
    /// # Errors
    /// Reject invalid or incompatible checkpoints without partial restoration.
    fn restore(&mut self, bytes: &[u8]) -> SessionResult;
    /// # Errors
    /// Return configuration errors for rejected controls, fatal simulation errors otherwise.
    fn configure(&mut self, lobby: &Lobby, command: Option<&[u8]>, start: bool) -> SessionResult;
}
#[derive(Clone, Debug)]
pub enum OrganizerAction {
    Configure(Vec<u8>),
    Start(Vec<u8>),
}
#[derive(Clone, Serialize, Deserialize)]
enum ClientPacket {
    Hello {
        identity: Identity,
        game: [u8; 16],
        version: u32,
    },
    Ready {
        epoch: u64,
        hash: u64,
    },
    Input {
        epoch: u64,
        bytes: Vec<u8>,
    },
    Configure {
        epoch: u64,
        bytes: Vec<u8>,
        start: bool,
    },
}
#[derive(Serialize, Deserialize)]
enum ServerPacket {
    Roster(Lobby),
    Checkpoint {
        initialization: Vec<u8>,
        lobby: Lobby,
        bytes: Vec<u8>,
        hash: u64,
    },
    Running {
        epoch: u64,
    },
    Tick {
        epoch: u64,
        tick: u64,
        participant: ParticipantId,
        bytes: Vec<u8>,
        hash: u64,
    },
    Error(String),
}
pub(crate) struct Shared {
    lobby: ArcSwapOption<Lobby>,
    error: ArcSwapOption<String>,
    sender: Sender<ClientPacket>,
    epoch: AtomicU64,
}
impl Shared {
    pub(crate) fn input(&self, bytes: Vec<u8>) -> Result<(), SubmitError> {
        self.send(ClientPacket::Input {
            epoch: self.epoch.load(Ordering::Acquire),
            bytes,
        })
    }
    fn send(&self, packet: ClientPacket) -> Result<(), SubmitError> {
        self.sender.try_send(packet).map_err(|e| match e {
            crossbeam_channel::TrySendError::Full(_) => SubmitError::Full,
            crossbeam_channel::TrySendError::Disconnected(_) => SubmitError::Stopped,
        })
    }
    fn publish(&self, lobby: &Lobby) {
        self.epoch.store(lobby.epoch, Ordering::Release);
        self.lobby.store(Some(Arc::new(lobby.clone())));
    }
}
impl SessionControl {
    #[must_use]
    pub fn lobby(&self) -> Option<Arc<Lobby>> {
        self.managed.as_ref().and_then(|s| s.lobby.load_full())
    }
    #[must_use]
    pub fn lobby_error(&self) -> Option<Arc<String>> {
        self.managed.as_ref().and_then(|s| s.error.load_full())
    }
    /// # Errors
    /// Returns queue capacity, inactive session, or oversized control errors.
    pub fn organize(&self, action: OrganizerAction) -> Result<(), SubmitError> {
        let shared = self.managed.as_ref().ok_or(SubmitError::Stopped)?;
        let (bytes, start) = match action {
            OrganizerAction::Configure(b) => (b, false),
            OrganizerAction::Start(b) => (b, true),
        };
        if bytes.len() > 4096 {
            return Err(SubmitError::Codec(codec::CodecError(
                "lobby command too large",
            )));
        }
        shared.error.store(None);
        shared.send(ClientPacket::Configure {
            epoch: shared.epoch.load(Ordering::Acquire),
            bytes,
            start,
        })
    }
}
pub struct HostConfig<I> {
    pub initialization: I,
    pub port: u16,
    pub local: Option<Identity>,
    pub capacity: usize,
    /// For a local-only table, start after the local participant is configured.
    pub auto_start: bool,
}
fn shared() -> (SessionControl, Receiver<ClientPacket>) {
    let (sender, receiver) = bounded(256);
    let mut control = SessionControl::default();
    control.managed = Some(Arc::new(Shared {
        lobby: ArcSwapOption::empty(),
        error: ArcSwapOption::empty(),
        sender,
        epoch: AtomicU64::new(0),
    }));
    (control, receiver)
}
fn handle<C>(control: SessionControl, worker: SessionWorker) -> SessionHandle<C> {
    let (sender, _) = bounded::<CommandPayload>(1);
    SessionHandle {
        commands: CommandSender {
            sender,
            control,
            marker: PhantomData,
        },
        worker,
    }
}
fn encode<T: Serialize>(value: &T) -> SessionResult<Vec<u8>> {
    let bytes = bincode::serde::encode_to_vec(value, bincode::config::standard())
        .map_err(|e| SessionError::Protocol(e.to_string()))?;
    if bytes.len() > LIMIT {
        return Err(SessionError::Protocol(
            "checkpoint session packet too large".into(),
        ));
    }
    Ok(bytes)
}
fn decode<T: for<'a> Deserialize<'a>>(bytes: &[u8]) -> SessionResult<T> {
    if bytes.len() > LIMIT {
        return Err(SessionError::Protocol(
            "checkpoint session packet too large".into(),
        ));
    }
    let (value, used) =
        bincode::serde::decode_from_slice(bytes, bincode::config::standard().with_limit::<LIMIT>())
            .map_err(|e| SessionError::Protocol(e.to_string()))?;
    if used != bytes.len() {
        return Err(SessionError::Protocol("trailing packet bytes".into()));
    }
    Ok(value)
}
fn now() -> SessionResult<Duration> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| SessionError::Transport(e.to_string()))
}
fn transport_error(e: impl std::fmt::Display) -> SessionError {
    SessionError::Transport(e.to_string())
}
fn status(control: &SessionControl, lobby: &Lobby) {
    control.managed.as_ref().unwrap().publish(lobby);
    control.publish(if lobby.live {
        SessionStatus::Live
    } else if lobby.started {
        SessionStatus::Paused
    } else {
        SessionStatus::Lobby
    });
}

/// Open a checkpoint lobby on the worker.
/// # Errors
/// Rejects invalid identities/capacity or worker spawn failure.
pub fn host<G: Game>(
    game: G,
    config: HostConfig<G::Initialization>,
) -> SessionResult<SessionHandle<G::Command>>
where
    G::Simulation: CheckpointSimulation<G::Command>,
{
    if config.capacity == 0
        || config.capacity > 32
        || (config.auto_start && (config.local.is_none() || config.capacity != 1))
    {
        return Err(SessionError::Configuration("invalid lobby capacity".into()));
    }
    if let Some(identity) = &config.local {
        identity.validate()?;
    }
    let initialization = codec::encode(&config.initialization)?;
    let (control, receiver) = shared();
    let worker_control = control.clone();
    let worker = SessionWorker::spawn("checkpoint-authority", move || {
        let socket = UdpSocket::bind(SocketAddr::from(([0, 0, 0, 0], config.port)))?;
        socket.set_nonblocking(true)?;
        let address = socket.local_addr()?;
        let mut transport = NetcodeServerTransport::new(
            renet_netcode::ServerConfig {
                current_time: now()?,
                max_clients: config.capacity + 4,
                protocol_id: PROTOCOL,
                public_addresses: vec![address],
                authentication: ServerAuthentication::Unsecure,
            },
            socket,
        )
        .map_err(transport_error)?;
        let mut server = RenetServer::new(connection_config());
        let mut simulation = game.create(config.initialization, Duration::from_millis(100))?;
        let result = authority::<G>(
            &mut simulation,
            &initialization,
            config.local.as_ref(),
            config.capacity,
            config.auto_start,
            &mut server,
            &mut transport,
            &receiver,
            &worker_control,
        );
        transport.disconnect_all(&mut server);
        result
    })?;
    Ok(handle(control, worker))
}
struct AuthorityState {
    lobby: Lobby,
    ready: BTreeMap<u64, bool>,
    pending_start: bool,
    checkpoint_hash: u64,
}
impl AuthorityState {
    fn refresh<C>(
        &mut self,
        simulation: &mut impl CheckpointSimulation<C>,
        initial: &[u8],
        server: &mut RenetServer,
        control: &SessionControl,
    ) -> SessionResult {
        self.lobby.epoch = self
            .lobby
            .epoch
            .checked_add(1)
            .ok_or_else(|| SessionError::Protocol("epoch exhausted".into()))?;
        self.lobby.live = false;
        simulation.configure(&self.lobby, None, false)?;
        let bytes = simulation.checkpoint()?;
        self.checkpoint_hash = simulation.state_hash();
        self.ready.clear();
        self.lobby.ready = self
            .lobby
            .members
            .iter()
            .filter(|m| m.participant == ParticipantId::Host)
            .map(|m| m.identity.id)
            .collect();
        let packet = encode(&ServerPacket::Checkpoint {
            initialization: initial.to_vec(),
            lobby: self.lobby.clone(),
            bytes,
            hash: self.checkpoint_hash,
        })?;
        for member in &self.lobby.members {
            if let ParticipantId::Remote(id) = member.participant {
                server.send_message(id, CHANNEL, packet.clone());
                self.ready.insert(id, false);
            }
        }
        simulation.publish()?;
        status(control, &self.lobby);
        Ok(())
    }
}
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn authority<G: Game>(
    sim: &mut G::Simulation,
    initial: &[u8],
    local: Option<&Identity>,
    capacity: usize,
    auto_start: bool,
    server: &mut RenetServer,
    transport: &mut NetcodeServerTransport,
    receiver: &Receiver<ClientPacket>,
    control: &SessionControl,
) -> SessionResult
where
    G::Simulation: CheckpointSimulation<G::Command>,
{
    let mut state = AuthorityState {
        lobby: Lobby::default(),
        ready: BTreeMap::new(),
        pending_start: false,
        checkpoint_hash: 0,
    };
    if let Some(identity) = local {
        state.lobby.organizer = Some(identity.id);
        state.lobby.members.push(Member {
            identity: identity.clone(),
            participant: ParticipantId::Host,
        });
    }
    if auto_start {
        sim.configure(&state.lobby, Some(&[]), true)?;
        state.pending_start = true;
    }
    state.refresh(sim, initial, server, control)?;
    let mut last = Instant::now();
    let mut pending = BTreeMap::new();
    while !control.is_cancelled() {
        let dt = last.elapsed();
        last = Instant::now();
        transport.update(dt, server).map_err(transport_error)?;
        server.update(dt);
        while let Some(event) = server.get_event() {
            match event {
                ServerEvent::ClientConnected { client_id } => {
                    pending.insert(client_id, Instant::now());
                }
                ServerEvent::ClientDisconnected { client_id, .. } => {
                    pending.remove(&client_id);
                    let before = state.lobby.members.len();
                    state
                        .lobby
                        .members
                        .retain(|m| m.participant != ParticipantId::Remote(client_id));
                    if before != state.lobby.members.len() {
                        if !state
                            .lobby
                            .members
                            .iter()
                            .any(|m| Some(m.identity.id) == state.lobby.organizer)
                        {
                            state.lobby.organizer =
                                state.lobby.members.first().map(|m| m.identity.id);
                        }
                        state.pending_start = false;
                        state.refresh(sim, initial, server, control)?;
                    }
                }
            }
        }
        for (&id, time) in &pending {
            if time.elapsed() > Duration::from_secs(10) {
                server.disconnect(id);
            }
        }
        let mut packets = Vec::new();
        for _ in 0..256 {
            if let Ok(p) = receiver.try_recv() {
                packets.push((ParticipantId::Host, p));
            } else {
                break;
            }
        }
        for id in server.clients_id() {
            for _ in 0..64 {
                let Some(bytes) = server.receive_message(id, CHANNEL) else {
                    break;
                };
                if let Ok(p) = decode(&bytes) {
                    packets.push((ParticipantId::Remote(id), p));
                } else {
                    server.disconnect(id);
                    break;
                }
            }
        }
        for (origin, packet) in packets {
            let result = process::<G>(
                sim, initial, capacity, &mut state, origin, packet, server, control,
            );
            if let Err(error) = result {
                if !matches!(
                    error,
                    SessionError::Configuration(_)
                        | SessionError::Compatibility(_)
                        | SessionError::Codec(_)
                        | SessionError::Protocol(_)
                        | SessionError::Desync(_)
                ) {
                    return Err(error);
                }
                if matches!(
                    error,
                    SessionError::Codec(_) | SessionError::Protocol(_) | SessionError::Desync(_)
                ) && let ParticipantId::Remote(id) = origin
                {
                    server.disconnect(id);
                    continue;
                }
                match origin {
                    ParticipantId::Host => control
                        .managed
                        .as_ref()
                        .unwrap()
                        .error
                        .store(Some(Arc::new(error.to_string()))),
                    ParticipantId::Remote(id) => server.send_message(
                        id,
                        CHANNEL,
                        encode(&ServerPacket::Error(error.to_string()))?,
                    ),
                }
            }
        }
        pending.retain(|id, _| {
            !state
                .lobby
                .members
                .iter()
                .any(|m| m.participant == ParticipantId::Remote(*id))
        });
        if state.pending_start && state.ready.values().all(|v| *v) {
            state.pending_start = false;
            state.lobby.live = true;
            state.lobby.started = true;
            server.broadcast_message(
                CHANNEL,
                encode(&ServerPacket::Running {
                    epoch: state.lobby.epoch,
                })?,
            );
            status(control, &state.lobby);
        }
        transport.send_packets(server);
        thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn process<G: Game>(
    sim: &mut G::Simulation,
    initial: &[u8],
    capacity: usize,
    state: &mut AuthorityState,
    origin: ParticipantId,
    packet: ClientPacket,
    server: &mut RenetServer,
    control: &SessionControl,
) -> SessionResult
where
    G::Simulation: CheckpointSimulation<G::Command>,
{
    match packet {
        ClientPacket::Hello {
            identity,
            game,
            version,
        } => {
            identity.validate()?;
            if game != G::ID || version != G::VERSION {
                return Err(SessionError::Compatibility("different game version".into()));
            }
            if state.lobby.members.len() >= capacity
                || state
                    .lobby
                    .members
                    .iter()
                    .any(|m| m.identity.id == identity.id || m.participant == origin)
            {
                return Err(SessionError::Configuration(
                    "table full or identity already connected".into(),
                ));
            }
            state.lobby.organizer.get_or_insert(identity.id);
            state.lobby.members.push(Member {
                identity,
                participant: origin,
            });
            state.pending_start = false;
            state.refresh(sim, initial, server, control)?;
        }
        ClientPacket::Ready { epoch, hash } => {
            if epoch == state.lobby.epoch {
                if hash != state.checkpoint_hash {
                    return Err(SessionError::Desync(
                        "checkpoint verification failed".into(),
                    ));
                }
                if let ParticipantId::Remote(id) = origin
                    && let Some(ready) = state.ready.get_mut(&id)
                {
                    *ready = true;
                    if let Some(member) =
                        state.lobby.members.iter().find(|m| m.participant == origin)
                        && !state.lobby.ready.contains(&member.identity.id)
                    {
                        state.lobby.ready.push(member.identity.id);
                    }
                    server.broadcast_message(
                        CHANNEL,
                        encode(&ServerPacket::Roster(state.lobby.clone()))?,
                    );
                    status(control, &state.lobby);
                }
            }
        }
        ClientPacket::Configure {
            epoch,
            bytes,
            start,
        } => {
            let organizer =
                state.lobby.members.iter().any(|m| {
                    m.participant == origin && Some(m.identity.id) == state.lobby.organizer
                });
            if !organizer || epoch != state.lobby.epoch || state.lobby.live {
                return Err(SessionError::Configuration(
                    "only the current organizer may configure a paused table".into(),
                ));
            }
            if start && state.ready.values().any(|r| !r) {
                return Err(SessionError::Configuration(
                    "wait for players to synchronize".into(),
                ));
            }
            sim.configure(&state.lobby, Some(&bytes), start)?;
            state.pending_start = start;
            state.refresh(sim, initial, server, control)?;
        }
        ClientPacket::Input { epoch, bytes } => {
            if !state.lobby.live
                || epoch != state.lobby.epoch
                || !state.lobby.members.iter().any(|m| m.participant == origin)
            {
                return Ok(());
            }
            let command = codec::decode(&bytes)?;
            let tick = sim
                .current_tick()
                .checked_add(1)
                .ok_or_else(|| SessionError::Simulation("tick exhausted".into()))?;
            sim.advance(Tick {
                number: tick,
                inputs: vec![Input {
                    participant: origin,
                    command,
                }],
            })?;
            sim.publish()?;
            sim.finish_tick();
            let hash = sim.state_hash();
            server.broadcast_message(
                CHANNEL,
                encode(&ServerPacket::Tick {
                    epoch,
                    tick,
                    participant: origin,
                    bytes,
                    hash,
                })?,
            );
        }
    }
    Ok(())
}

/// `connection_id` is fresh per connection, distinct from the remembered identity.
/// # Errors
/// Rejects invalid identity or worker spawn failure.
pub fn connect<G: Game>(
    game: G,
    address: SocketAddr,
    connection_id: u64,
    identity: Identity,
) -> SessionResult<SessionHandle<G::Command>>
where
    G::Simulation: CheckpointSimulation<G::Command>,
{
    identity.validate()?;
    let (control, receiver) = shared();
    let worker_control = control.clone();
    let worker = SessionWorker::spawn("checkpoint-peer", move || {
        let socket = UdpSocket::bind(if address.is_ipv4() {
            "0.0.0.0:0"
        } else {
            "[::]:0"
        })?;
        socket.set_nonblocking(true)?;
        let mut transport = NetcodeClientTransport::new(
            now()?,
            ClientAuthentication::Unsecure {
                protocol_id: PROTOCOL,
                client_id: connection_id,
                server_addr: address,
                user_data: None,
            },
            socket,
        )
        .map_err(transport_error)?;
        let mut client = RenetClient::new(connection_config());
        let result = replica(
            game,
            identity,
            &mut client,
            &mut transport,
            &receiver,
            &worker_control,
        );
        transport.disconnect();
        result
    })?;
    Ok(handle(control, worker))
}
#[allow(clippy::too_many_lines)]
fn replica<G: Game>(
    game: G,
    identity: Identity,
    client: &mut RenetClient,
    transport: &mut NetcodeClientTransport,
    receiver: &Receiver<ClientPacket>,
    control: &SessionControl,
) -> SessionResult
where
    G::Simulation: CheckpointSimulation<G::Command>,
{
    let mut sim: Option<G::Simulation> = None;
    let mut lobby = Lobby::default();
    let mut hello = false;
    let mut last = Instant::now();
    while !control.is_cancelled() {
        let dt = last.elapsed();
        last = Instant::now();
        transport.update(dt, client).map_err(transport_error)?;
        client.update(dt);
        if client.is_disconnected() {
            return Err(SessionError::Transport(
                "Connection lost. The last committed game can still be saved; reconnect to resume."
                    .into(),
            ));
        }
        if client.is_connected() && !hello {
            client.send_message(
                CHANNEL,
                encode(&ClientPacket::Hello {
                    identity: identity.clone(),
                    game: G::ID,
                    version: G::VERSION,
                })?,
            );
            hello = true;
        }
        for _ in 0..256 {
            let Some(bytes) = client.receive_message(CHANNEL) else {
                break;
            };
            match decode::<ServerPacket>(&bytes)? {
                ServerPacket::Roster(next) => {
                    if next.epoch == lobby.epoch {
                        lobby = next;
                        status(control, &lobby);
                    }
                }
                ServerPacket::Checkpoint {
                    initialization,
                    lobby: next,
                    bytes,
                    hash,
                } => {
                    if sim.is_none() {
                        sim =
                            Some(game.create(
                                codec::decode(&initialization)?,
                                Duration::from_millis(100),
                            )?);
                    }
                    let simulation = sim.as_mut().unwrap();
                    simulation.restore(&bytes)?;
                    if simulation.state_hash() != hash {
                        return Err(SessionError::Desync(
                            "checkpoint verification failed".into(),
                        ));
                    }
                    simulation.publish()?;
                    lobby = next;
                    status(control, &lobby);
                    client.send_message(
                        CHANNEL,
                        encode(&ClientPacket::Ready {
                            epoch: lobby.epoch,
                            hash,
                        })?,
                    );
                }
                ServerPacket::Running { epoch } => {
                    if epoch != lobby.epoch || sim.is_none() {
                        return Err(SessionError::Protocol(
                            "Start before verified checkpoint".into(),
                        ));
                    }
                    lobby.live = true;
                    lobby.started = true;
                    status(control, &lobby);
                }
                ServerPacket::Tick {
                    epoch,
                    tick,
                    participant,
                    bytes,
                    hash,
                } => {
                    let simulation = sim
                        .as_mut()
                        .ok_or_else(|| SessionError::Protocol("tick before checkpoint".into()))?;
                    if !lobby.live || epoch != lobby.epoch || tick != simulation.current_tick() + 1
                    {
                        return Err(SessionError::Protocol("out of sequence tick".into()));
                    }
                    simulation.advance(Tick {
                        number: tick,
                        inputs: vec![Input {
                            participant,
                            command: codec::decode(&bytes)?,
                        }],
                    })?;
                    if simulation.state_hash() != hash {
                        simulation.finish_tick();
                        return Err(SessionError::Desync("live state hash mismatch".into()));
                    }
                    simulation.publish()?;
                    simulation.finish_tick();
                }
                ServerPacket::Error(error) => {
                    if sim.is_none() {
                        return Err(SessionError::Configuration(error));
                    }
                    control
                        .managed
                        .as_ref()
                        .unwrap()
                        .error
                        .store(Some(Arc::new(error)));
                }
            }
        }
        for _ in 0..256 {
            if let Ok(packet) = receiver.try_recv() {
                client.send_message(CHANNEL, encode(&packet)?);
            } else {
                break;
            }
        }
        transport.send_packets(client).map_err(transport_error)?;
        thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Counter {
        tick: u64,
        value: u64,
    }
    impl Simulation<u64> for Counter {
        fn current_tick(&self) -> u64 {
            self.tick
        }
        fn state_hash(&mut self) -> u64 {
            self.tick ^ self.value
        }
        fn advance(&mut self, tick: Tick<u64>) -> SessionResult {
            self.tick = tick.number;
            for input in tick.inputs {
                self.value += input.command;
            }
            Ok(())
        }
    }
    impl CheckpointSimulation<u64> for Counter {
        fn checkpoint(&mut self) -> SessionResult<Vec<u8>> {
            encode(&(self.tick, self.value))
        }
        fn restore(&mut self, bytes: &[u8]) -> SessionResult {
            let (tick, value) = decode(bytes)?;
            self.tick = tick;
            self.value = value;
            Ok(())
        }
        fn configure(&mut self, _: &Lobby, _: Option<&[u8]>, _: bool) -> SessionResult {
            Ok(())
        }
    }
    struct CounterGame;
    impl Game for CounterGame {
        type Command = u64;
        type Initialization = u64;
        type Simulation = Counter;
        const ID: [u8; 16] = *b"managed-counter1";
        const VERSION: u32 = 1;
        fn create(&self, value: u64, _: Duration) -> SessionResult<Counter> {
            Ok(Counter { tick: 0, value })
        }
    }
    #[test]
    fn queued_inputs_cannot_cross_pause_and_synchronization_epochs() {
        let (control, _receiver) = shared();
        let mut server = RenetServer::new(connection_config());
        let mut sim = Counter {
            tick: 10,
            value: 42,
        };
        let mut state = AuthorityState {
            lobby: Lobby {
                epoch: 5,
                live: true,
                started: true,
                organizer: Some(1),
                members: vec![Member {
                    identity: Identity {
                        id: 1,
                        name: "Host".into(),
                    },
                    participant: ParticipantId::Host,
                }],
                ready: vec![1],
            },
            ready: BTreeMap::new(),
            pending_start: false,
            checkpoint_hash: 0,
        };
        let input = |epoch| ClientPacket::Input {
            epoch,
            bytes: codec::encode(&1u64).unwrap(),
        };
        process::<CounterGame>(
            &mut sim,
            &[],
            1,
            &mut state,
            ParticipantId::Host,
            input(4),
            &mut server,
            &control,
        )
        .unwrap();
        assert_eq!(sim.value, 42);
        state.lobby.live = false;
        process::<CounterGame>(
            &mut sim,
            &[],
            1,
            &mut state,
            ParticipantId::Host,
            input(5),
            &mut server,
            &control,
        )
        .unwrap();
        assert_eq!(sim.value, 42);
        state.lobby.live = true;
        process::<CounterGame>(
            &mut sim,
            &[],
            1,
            &mut state,
            ParticipantId::Remote(99),
            input(5),
            &mut server,
            &control,
        )
        .unwrap();
        assert_eq!(sim.value, 42);
        process::<CounterGame>(
            &mut sim,
            &[],
            1,
            &mut state,
            ParticipantId::Host,
            input(5),
            &mut server,
            &control,
        )
        .unwrap();
        assert_eq!((sim.tick, sim.value), (11, 43));
    }
    #[test]
    fn checkpoint_packets_are_bounded_and_require_complete_encoding() {
        let bytes = encode(&ServerPacket::Running { epoch: 10 }).unwrap();
        assert!(decode::<ServerPacket>(&bytes).is_ok());
        for end in 0..bytes.len() {
            assert!(decode::<ServerPacket>(&bytes[..end]).is_err());
        }
        let mut trailing = bytes;
        trailing.push(0);
        assert!(decode::<ServerPacket>(&trailing).is_err());
        assert!(decode::<ClientPacket>(&vec![255; LIMIT + 1]).is_err());
        assert!(encode(&ServerPacket::Error("x".repeat(LIMIT))).is_err());
        // Huge claimed collections must be rejected within the decoder's allocation limit.
        assert!(decode::<ClientPacket>(&[0, 253, 255, 255, 255, 255, 255, 255, 255, 255]).is_err());
    }
}
