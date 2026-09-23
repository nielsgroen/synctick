use crate::{
    codec::{self, Wire},
    protocol::{CommandPayload, TickAdvance},
    replay::{ReplayHeader, ReplayReader},
    session::{SessionControl, SessionError, SessionResult, SessionStatus, SessionWorker},
    simulation::SessionSimulation,
};
use crossbeam_channel::{Sender, TrySendError};
use serde::{Deserialize, Serialize};
use std::{
    marker::PhantomData,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

/// Session-local origin, not an authenticated account. Remote IDs come from
/// the connection, never from the command payload. Host cannot be impersonated
/// by selecting a remote numeric ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ParticipantId {
    Host,
    Remote(u64),
}

#[derive(Debug)]
pub struct Input<C> {
    pub participant: ParticipantId,
    pub command: C,
}

#[derive(Debug)]
pub struct Tick<C> {
    pub number: u64,
    pub inputs: Vec<Input<C>>,
}

/// Deterministic state, owned exclusively by the session worker.
///
/// Hash every future-affecting value, including tick, allocators and RNG state.
/// Do not include transport state, wall time or presentation data in the hash.
pub trait Simulation<C>: 'static {
    fn current_tick(&self) -> u64;
    fn state_hash(&mut self) -> u64;
    /// Evaluate gameplay legality and execute accepted actions in canonical order.
    /// Illegal gameplay actions must have deterministic outcomes, including replay.
    /// # Errors
    /// Return terminal simulation failures; successful execution ends at tick.number.
    fn advance(&mut self, tick: Tick<C>) -> SessionResult;
    /// Publish the complete current state before Live and after each valid live tick.
    ///
    /// Never called during replay or after a failed/invalid tick. Must not mutate
    /// deterministic state. At startup there is no pending tick input or effect.
    /// Per-tick presentation effects may be consumed here, before `finish_tick`.
    /// # Errors
    /// Publication failures terminate the session without rolling back the tick.
    fn publish(&mut self) -> SessionResult {
        Ok(())
    }
    /// Release per-tick inputs, presentation effects, and tracking data.
    ///
    /// Called exactly once after each attempted `advance`, in live play and replay,
    /// including returned advance/publication errors. Not called for rejected tick
    /// continuity or initial publication. Must not alter deterministic state or fail.
    /// Panics are terminal worker failures; cleanup after a panic is not guaranteed.
    fn finish_tick(&mut self) {}
}

/// Supply only game rules, initialization, commands and state fingerprinting.
/// The factory runs on the worker thread; the simulation need not be `Send`.
pub trait Game: Send + 'static {
    type Command: Wire + Send + Sync + 'static;
    type Initialization: Wire + Send + 'static;
    type Simulation: Simulation<Self::Command>;
    const ID: [u8; 16];
    /// Bump for changes to command encoding, initialization, rules or state hashing.
    const VERSION: u32;
    /// # Errors
    /// Reject unsupported initialization before any ticks execute.
    fn create(
        &self,
        initialization: Self::Initialization,
        tick_duration: Duration,
    ) -> SessionResult<Self::Simulation>;
}

/// Fixed-duration sessions only. Queue capacity is local; the duration is saved
/// and adopted by clients and resumed sessions.
#[derive(Debug, Clone)]
pub struct ServerConfig<I> {
    pub port: u16,
    pub expected_clients: u32,
    pub initialization: I,
    pub tick_duration: Duration,
    pub load_path: Option<PathBuf>,
    pub record_path: Option<PathBuf>,
    pub desync_policy: crate::DesyncPolicy,
    pub command_capacity: usize,
}
impl<I> ServerConfig<I> {
    pub const fn new(initialization: I) -> Self {
        Self {
            port: 5000,
            expected_clients: 0,
            initialization,
            tick_duration: Duration::from_nanos(33_333_333),
            load_path: None,
            record_path: None,
            desync_policy: crate::DesyncPolicy::Disconnect,
            command_capacity: 256,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ClientConfig {
    pub id: u64,
    pub server_addr: SocketAddr,
    pub command_capacity: usize,
}
impl ClientConfig {
    #[must_use]
    pub const fn new(id: u64, server_addr: SocketAddr) -> Self {
        Self {
            id,
            server_addr,
            command_capacity: 256,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SubmitError {
    #[error("session is not live")]
    NotLive,
    #[error("command queue is full")]
    Full,
    #[error("session has stopped")]
    Stopped,
    #[error(transparent)]
    Codec(#[from] codec::CodecError),
}

pub struct CommandSender<C> {
    pub(crate) sender: Sender<CommandPayload>,
    pub(crate) control: SessionControl,
    pub(crate) marker: PhantomData<fn(C)>,
}
impl<C> Clone for CommandSender<C> {
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
            control: self.control.clone(),
            marker: PhantomData,
        }
    }
}
impl<C: Wire> CommandSender<C> {
    /// Success acknowledges local queue admission, not gameplay acceptance.
    /// # Errors
    /// Returns explicit not-live, stopped, full or codec errors.
    pub fn submit(&self, command: &C) -> Result<(), SubmitError> {
        if self.control.is_cancelled() {
            return Err(SubmitError::Stopped);
        }
        match &*self.control.status() {
            SessionStatus::Live => {}
            SessionStatus::Stopped | SessionStatus::Failed(_) => return Err(SubmitError::Stopped),
            _ => return Err(SubmitError::NotLive),
        }
        let payload = CommandPayload(codec::encode(command)?);
        if let Some(managed) = &self.control.managed {
            return managed.input(payload.0);
        }
        self.sender.try_send(payload).map_err(|error| match error {
            TrySendError::Full(_) => SubmitError::Full,
            TrySendError::Disconnected(_) => SubmitError::Stopped,
        })
    }
}

/// Owns worker shutdown. Explicit `shutdown`/`join` returns the retained terminal
/// result; `Drop` also cancels and joins, logging otherwise unobserved failures.
pub struct SessionHandle<C> {
    pub(crate) commands: CommandSender<C>,
    pub(crate) worker: SessionWorker,
}
impl<C> SessionHandle<C> {
    #[must_use]
    pub fn commands(&self) -> CommandSender<C> {
        self.commands.clone()
    }
    #[must_use]
    pub fn control(&self) -> SessionControl {
        self.commands.control.clone()
    }
    #[must_use]
    pub fn status(&self) -> Arc<SessionStatus> {
        self.commands.control.status()
    }
    pub fn cancel(&self) {
        self.commands.control.cancel();
    }
    pub fn poll(&mut self) {
        self.worker.poll(&self.commands.control);
    }
    /// Wait for completion without requesting cancellation. The result is retained
    /// independently of status, even after repeated joins.
    pub fn join(&mut self) -> Arc<SessionResult> {
        self.worker.join(&self.commands.control)
    }
    pub fn shutdown(&mut self) -> Arc<SessionResult> {
        self.cancel();
        self.join()
    }
}
impl<C> Drop for SessionHandle<C> {
    fn drop(&mut self) {
        self.cancel();
        self.worker.join(&self.commands.control);
    }
}

pub fn validate_tick_duration(duration: Duration) -> SessionResult {
    if duration < Duration::from_micros(1) || duration > Duration::from_secs(1) {
        return Err(SessionError::Configuration(
            "tick duration must be between 1 microsecond and 1 second".into(),
        ));
    }
    Ok(())
}
fn validate_capacity(capacity: usize) -> SessionResult {
    if capacity == 0 || capacity > 65_536 {
        return Err(SessionError::Configuration(
            "command capacity must be in 1..=65536".into(),
        ));
    }
    Ok(())
}

struct Adapter<G: Game> {
    game: G,
    header: ReplayHeader,
    simulation: Option<G::Simulation>,
}
impl<G: Game> Adapter<G> {
    const fn new(game: G, header: ReplayHeader) -> Self {
        Self {
            game,
            header,
            simulation: None,
        }
    }
    fn sim(&mut self) -> SessionResult<&mut G::Simulation> {
        self.simulation
            .as_mut()
            .ok_or_else(|| SessionError::Simulation("simulation was not initialized".into()))
    }
}
impl<G: Game> SessionSimulation for Adapter<G> {
    fn header(&self) -> ReplayHeader {
        self.header.clone()
    }
    fn hash_interval(&self) -> u64 {
        (1_000_000_000 + self.header.tick_nanos / 2) / self.header.tick_nanos
    }
    fn initialize(&mut self, header: &ReplayHeader) -> SessionResult {
        validate_header::<G>(header)?;
        if self.simulation.is_some() {
            return Err(SessionError::Simulation("duplicate initialization".into()));
        }
        let initial = codec::decode(&header.initialization)?;
        let simulation = self
            .game
            .create(initial, Duration::from_nanos(header.tick_nanos))?;
        if simulation.current_tick() != 0 {
            return Err(SessionError::Simulation(
                "new simulation must start at tick zero".into(),
            ));
        }
        self.header = header.clone();
        self.simulation = Some(simulation);
        Ok(())
    }
    fn validate_command(&self, payload: &[u8]) -> SessionResult {
        Ok(codec::validate::<G::Command>(payload)?)
    }
    fn current_tick(&self) -> u64 {
        self.simulation.as_ref().map_or(0, Simulation::current_tick)
    }
    fn state_hash(&mut self) -> u64 {
        self.simulation
            .as_mut()
            .expect("driver initializes before hashing")
            .state_hash()
    }
    fn advance(&mut self, advance: TickAdvance) -> SessionResult {
        let mut inputs = Vec::new();
        inputs
            .try_reserve_exact(advance.inputs.len())
            .map_err(|error| SessionError::Simulation(error.to_string()))?;
        for input in advance.inputs {
            inputs.push(Input {
                participant: input.participant,
                command: codec::decode(&input.payload.0)?,
            });
        }
        self.sim()?.advance(Tick {
            number: advance.tick,
            inputs,
        })
    }
    fn publish(&mut self) -> SessionResult {
        self.sim()?.publish()
    }
    fn finish_tick(&mut self) {
        if let Some(simulation) = &mut self.simulation {
            simulation.finish_tick();
        }
    }
}
fn validate_header<G: Game>(header: &ReplayHeader) -> SessionResult {
    if header.game_id != G::ID {
        return Err(SessionError::Compatibility("different game".into()));
    }
    if header.game_version != G::VERSION {
        return Err(SessionError::Compatibility("different game version".into()));
    }
    validate_tick_duration(Duration::from_nanos(header.tick_nanos))?;
    codec::validate::<G::Initialization>(&header.initialization)?;
    Ok(())
}

/// Start an authority with a local participant and optional remote participants.
/// # Errors
/// Rejects invalid configuration and incompatible saves before side effects.
pub fn host<G: Game>(
    game: G,
    config: ServerConfig<G::Initialization>,
) -> SessionResult<SessionHandle<G::Command>> {
    start_server(game, config, true)
}
/// Start an authority without a local participant. Its command handle stays disabled.
/// # Errors
/// Rejects invalid configuration and incompatible saves before side effects.
pub fn dedicated_server<G: Game>(
    game: G,
    config: ServerConfig<G::Initialization>,
) -> SessionResult<SessionHandle<G::Command>> {
    start_server(game, config, false)
}
fn start_server<G: Game>(
    game: G,
    config: ServerConfig<G::Initialization>,
    local: bool,
) -> SessionResult<SessionHandle<G::Command>> {
    validate_capacity(config.command_capacity)?;
    validate_tick_duration(config.tick_duration)?;
    if config.expected_clients > 1024 {
        return Err(SessionError::Configuration(
            "Renet supports at most 1024 clients".into(),
        ));
    }
    let header = if let Some(path) = &config.load_path {
        let reader = ReplayReader::open(path)?;
        validate_header::<G>(reader.header())?;
        reader.header().clone()
    } else {
        ReplayHeader::new(
            G::ID,
            G::VERSION,
            u64::try_from(config.tick_duration.as_nanos())
                .map_err(|_| SessionError::Configuration("tick duration overflow".into()))?,
            codec::encode(&config.initialization)?,
        )
    };
    let tick_duration = Duration::from_nanos(header.tick_nanos);
    let control = SessionControl::default();
    let worker_control = control.clone();
    let (sender, receiver) = crossbeam_channel::bounded(config.command_capacity);
    let worker = SessionWorker::spawn("session-authority", move || {
        let mut adapter = Adapter::new(game, header);
        crate::run_server::run_server(
            crate::run_server::ServerCfg {
                port: config.port,
                tick_duration,
                expected_clients: config.expected_clients,
                load_path: config.load_path,
                record_path: config.record_path,
                desync_policy: config.desync_policy,
            },
            if local {
                Some(receiver)
            } else {
                drop(receiver);
                None
            },
            &mut adapter,
            &worker_control,
        )
    })?;
    Ok(SessionHandle {
        commands: CommandSender {
            sender,
            control,
            marker: PhantomData,
        },
        worker,
    })
}

/// Connect a replica. The server's validated header supplies initialization and timing.
/// # Errors
/// Rejects invalid local configuration and thread creation failures.
pub fn connect<G: Game>(game: G, config: ClientConfig) -> SessionResult<SessionHandle<G::Command>> {
    validate_capacity(config.command_capacity)?;
    let control = SessionControl::default();
    let worker_control = control.clone();
    let (sender, receiver) = crossbeam_channel::bounded(config.command_capacity);
    let worker = SessionWorker::spawn("session-replica", move || {
        let header = ReplayHeader::new(G::ID, G::VERSION, 33_333_333, vec![]);
        let mut adapter = Adapter::new(game, header);
        crate::run_client::run_client(
            crate::run_client::ClientCfg {
                id: config.id,
                server_addr: config.server_addr,
            },
            &mut adapter,
            receiver,
            &worker_control,
        )
    })?;
    Ok(SessionHandle {
        commands: CommandSender {
            sender,
            control,
            marker: PhantomData,
        },
        worker,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplayOutcome {
    pub records: u64,
    pub final_tick: u64,
    pub final_hash: u64,
    pub cancelled: bool,
}
/// Replay synchronously on the calling thread. Pass a control to support signals.
/// # Errors
/// Rejects incompatible saves, malformed records and simulation failures.
pub fn replay<G: Game>(
    game: G,
    path: &Path,
    control: &SessionControl,
) -> SessionResult<ReplayOutcome> {
    let mut reader = ReplayReader::open(path)?;
    validate_header::<G>(reader.header())?;
    let mut adapter = Adapter::new(game, reader.header().clone());
    adapter.initialize(reader.header())?;
    let records = crate::replay::replay_into(&mut adapter, &mut reader, None, control)?;
    Ok(ReplayOutcome {
        records,
        final_tick: adapter.current_tick(),
        final_hash: adapter.state_hash(),
        cancelled: control.is_cancelled(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        protocol::{Command, MAX_REPLAY_RECORD_BYTES},
        replay::ReplayWriter,
    };
    #[derive(Clone)]
    struct Counter;
    struct Count(u64);
    impl Game for Counter {
        type Command = Vec<u64>;
        type Initialization = u64;
        type Simulation = Count;
        const ID: [u8; 16] = *b"counter-game-001";
        const VERSION: u32 = 1;
        fn create(&self, initial: u64, _: Duration) -> SessionResult<Count> {
            if initial != 42 {
                return Err(SessionError::Simulation("expected seed 42".into()));
            }
            Ok(Count(0))
        }
    }
    impl Simulation<Vec<u64>> for Count {
        fn current_tick(&self) -> u64 {
            self.0
        }
        fn state_hash(&mut self) -> u64 {
            self.0
        }
        fn advance(&mut self, tick: Tick<Vec<u64>>) -> SessionResult {
            self.0 = tick.number;
            Ok(())
        }
    }
    #[test]
    fn submission_reports_not_live_full_stopped_and_oversized() {
        let control = SessionControl::default();
        let (sender, receiver) = crossbeam_channel::bounded(1);
        let commands = CommandSender {
            sender,
            control: control.clone(),
            marker: PhantomData::<fn(Vec<u64>)>,
        };
        assert!(matches!(
            commands.submit(&vec![1]),
            Err(SubmitError::NotLive)
        ));
        control.publish(SessionStatus::Live);
        commands.submit(&vec![1]).unwrap();
        assert!(matches!(commands.submit(&vec![1]), Err(SubmitError::Full)));
        assert!(matches!(
            commands.submit(&vec![0; codec::MAX_PAYLOAD_BYTES]),
            Err(SubmitError::Codec(_))
        ));
        drop(receiver);
        assert!(matches!(
            commands.submit(&vec![1]),
            Err(SubmitError::Stopped)
        ));
        control.cancel();
        assert!(matches!(
            commands.submit(&vec![1]),
            Err(SubmitError::Stopped)
        ));
    }
    #[test]
    fn compatibility_and_configuration_fail_before_creating_recordings() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("input.save");
        let target = directory.path().join("output.save");
        for (id, version) in [(Counter::ID, 2), (*b"different-game01", 1)] {
            let header = ReplayHeader::new(id, version, 33_333_333, codec::encode(&42u64).unwrap());
            let path = directory.path().join(format!("{version}.save"));
            ReplayWriter::create(&path, &header)
                .unwrap()
                .flush()
                .unwrap();
            let mut cfg = ServerConfig::new(42);
            cfg.load_path = Some(path.clone());
            cfg.record_path = Some(target.clone());
            assert!(matches!(
                host(Counter, cfg),
                Err(SessionError::Compatibility(_))
            ));
            assert!(!target.exists());
            assert!(matches!(
                replay(Counter, &path, &SessionControl::default()),
                Err(SessionError::Compatibility(_))
            ));
        }
        std::fs::write(&source, b"PLNTlegacy").unwrap();
        assert!(replay(Counter, &source, &SessionControl::default()).is_err());
        for bad in 0..3 {
            let mut cfg = ServerConfig::new(42);
            cfg.record_path = Some(target.clone());
            match bad {
                0 => cfg.expected_clients = 1025,
                1 => cfg.tick_duration = Duration::ZERO,
                _ => cfg.command_capacity = 0,
            }
            assert!(matches!(
                host(Counter, cfg),
                Err(SessionError::Configuration(_))
            ));
            assert!(!target.exists());
        }
    }
    #[test]
    fn recorded_origin_and_invalid_game_payload_are_preserved_and_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("bad.save");
        let header = ReplayHeader::new(Counter::ID, 1, 33_333_333, codec::encode(&42u64).unwrap());
        let input = Command {
            participant: ParticipantId::Remote(123),
            payload: CommandPayload(vec![255; 4]),
        };
        let mut writer = ReplayWriter::create(&path, &header).unwrap();
        writer
            .append(&TickAdvance {
                tick: 1,
                inputs: vec![input.clone()],
            })
            .unwrap();
        writer.flush().unwrap();
        let mut reader = ReplayReader::open(&path).unwrap();
        assert_eq!(reader.next_record().unwrap().unwrap().inputs[0], input);
        assert!(matches!(
            replay(Counter, &path, &SessionControl::default()),
            Err(SessionError::Codec(_))
        ));
        let oversized = TickAdvance {
            tick: 2,
            inputs: vec![
                Command {
                    participant: ParticipantId::Host,
                    payload: CommandPayload(vec![0; codec::MAX_PAYLOAD_BYTES])
                };
                MAX_REPLAY_RECORD_BYTES / codec::MAX_PAYLOAD_BYTES + 1
            ],
        };
        assert!(writer.append(&oversized).is_err());
        assert_eq!(
            ReplayReader::open(&path)
                .unwrap()
                .next_record()
                .unwrap()
                .unwrap()
                .tick,
            1
        );
    }
    #[test]
    fn cancellation_and_io_failure_are_retained_separately_from_status() {
        let control = SessionControl::default();
        let work_control = control.clone();
        let (sender, receiver) = crossbeam_channel::bounded(1);
        let worker = SessionWorker::spawn("cancel-test", move || {
            while !work_control.is_cancelled() {
                std::thread::yield_now();
            }
            drop(receiver);
            Ok(())
        })
        .unwrap();
        let mut handle = SessionHandle {
            commands: CommandSender {
                sender,
                control,
                marker: PhantomData::<fn(u64)>,
            },
            worker,
        };
        let first = handle.shutdown();
        let second = handle.join();
        assert!(first.is_ok());
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(*handle.status(), SessionStatus::Stopped);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("exists.save");
        std::fs::write(&path, b"preserve").unwrap();
        let mut cfg = ServerConfig::new(42);
        cfg.port = 0;
        cfg.record_path = Some(path.clone());
        let mut handle = host(Counter, cfg).unwrap();
        let result = handle.join();
        assert!(
            matches!(&*result, Err(SessionError::Io(error)) if error.kind() == std::io::ErrorKind::AlreadyExists)
        );
        assert_eq!(std::fs::read(path).unwrap(), b"preserve");
        assert!(Arc::ptr_eq(&result, &handle.shutdown()));
    }
    struct Incompatible<const VERSION: u32>;
    impl<const VERSION: u32> Game for Incompatible<VERSION> {
        type Command = Vec<u64>;
        type Initialization = u64;
        type Simulation = Count;
        const ID: [u8; 16] = if VERSION == 1 {
            *b"different-game01"
        } else {
            Counter::ID
        };
        const VERSION: u32 = VERSION;
        fn create(&self, _: u64, _: Duration) -> SessionResult<Count> {
            panic!("incompatible peers must be rejected before creating a simulation")
        }
    }
    fn wait_for(mut condition: impl FnMut() -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !condition() {
            assert!(std::time::Instant::now() < deadline, "session timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn rejects_incompatible_peer<G: Game>(game: G, address: SocketAddr) {
        let mut client = connect(game, ClientConfig::new(17, address)).unwrap();
        wait_for(|| {
            client.poll();
            matches!(&*client.status(), SessionStatus::Failed(_))
        });
        assert!(matches!(
            &*client.join(),
            Err(SessionError::Compatibility(_))
        ));
    }
    #[test]
    fn incompatible_peers_fail_before_factory_or_ticks() {
        let reservation = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let address = reservation.local_addr().unwrap();
        drop(reservation);
        let mut cfg = ServerConfig::new(42);
        cfg.port = address.port();
        cfg.expected_clients = 1;
        let mut server = dedicated_server(Counter, cfg).unwrap();
        wait_for(|| {
            server.poll();
            matches!(&*server.status(), SessionStatus::Waiting { .. })
        });
        rejects_incompatible_peer(Incompatible::<1>, address);
        rejects_incompatible_peer(Incompatible::<2>, address);
        assert!(server.shutdown().is_ok());
    }
}
