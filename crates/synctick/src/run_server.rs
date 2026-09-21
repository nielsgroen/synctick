//! Authoritative tick loop, verified startup barrier, and orderly cleanup.
use crate::pacer::TickPacer;
use crate::protocol::CommandPayload;
use crate::protocol::{
    CHANNEL_DESYNC, CHANNEL_LOCKSTEP, ClientMessage, DesyncHash, PROTOCOL_ID, SaveMetadata,
    ServerMessage, TickAdvance, connection_config, decode_client_message, decode_desync_hash,
    encode,
};
use crate::replay::{ReplayReader, ReplayWriter, replay_batch};
use crate::session::{SessionControl, SessionError, SessionResult, SessionStatus};
use crate::simulation::{SessionSimulation, TickPhase, advance_tick};
use crate::startup::StartupPeer;
use crossbeam_channel::{Receiver, TryRecvError};
use renet::{ClientId, RenetServer, ServerEvent};
use renet_netcode::{NetcodeServerTransport, ServerAuthentication, ServerConfig};
use std::collections::BTreeMap;
use std::net::{SocketAddr, UdpSocket};
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(test)]
const HASH_HISTORY_WINDOW: u64 = 300;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum DesyncPolicy {
    #[default]
    Disconnect,
    FailFast,
}

pub struct ServerCfg {
    pub port: u16,
    pub tick_duration: Duration,
    pub expected_clients: u32,
    pub load_path: Option<PathBuf>,
    /// Must be a nonexistent destination; exclusive creation prevents truncation.
    pub record_path: Option<PathBuf>,
    pub desync_policy: DesyncPolicy,
}

/// Run the same authority for headless and host modes. All normal exits flush
/// recordings, including load failures and fail-fast desync reports.
/// # Errors
/// Reports startup, transport, recording, and fail-fast desync failures.
pub fn run_server(
    mut cfg: ServerCfg,
    local_inputs: Option<Receiver<CommandPayload>>,
    sim: &mut impl SessionSimulation,
    control: &SessionControl,
) -> SessionResult {
    crate::api::validate_tick_duration(cfg.tick_duration)?;
    if cfg.expected_clients > 1024 {
        return Err(SessionError::Configuration(
            "Renet supports at most 1024 clients".into(),
        ));
    }
    let mut source = cfg
        .load_path
        .as_ref()
        .map(|path| ReplayReader::open(path))
        .transpose()?;
    let header = source
        .as_ref()
        .map_or_else(|| sim.header(), |reader| reader.header().clone());
    cfg.tick_duration = Duration::from_nanos(header.tick_nanos);
    sim.initialize(&header)?;
    let mut writer = cfg
        .record_path
        .as_ref()
        .map(|path| ReplayWriter::create(path, &header))
        .transpose()?;
    let result = run_session(
        &cfg,
        local_inputs.as_ref(),
        sim,
        control,
        &mut writer,
        source.as_mut(),
    );
    if let Some(writer) = &mut writer
        && let Err(error) = writer.flush()
    {
        if let Err(primary) = &result {
            log::error!("[server] {primary}");
        }
        return Err(SessionError::Io(error));
    }
    result
}

fn run_session(
    cfg: &ServerCfg,
    local_inputs: Option<&Receiver<CommandPayload>>,
    sim: &mut impl SessionSimulation,
    control: &SessionControl,
    writer: &mut Option<ReplayWriter>,
    source: Option<&mut ReplayReader<std::io::BufReader<std::fs::File>>>,
) -> SessionResult {
    if let Some(reader) = source {
        control.publish(SessionStatus::Loading {
            phase: crate::session::LoadingPhase::LoadingSave,
            completed: 0,
        });
        let mut published = Instant::now();
        loop {
            if control.is_cancelled() {
                return Ok(());
            }
            drain_local(local_inputs, false)?;
            let batch = replay_batch(sim, reader, writer.as_mut(), control)?;
            if published.elapsed() >= Duration::from_secs(1) {
                control.publish(SessionStatus::Loading {
                    phase: crate::session::LoadingPhase::LoadingSave,
                    completed: sim.current_tick(),
                });
                published = Instant::now();
            }
            if batch.finished {
                break;
            }
        }
    }
    if control.is_cancelled() {
        return Ok(());
    }
    if let Some(writer) = writer {
        writer.flush()?;
    }
    let save = if let Some(path) = cfg.record_path.as_ref().or(cfg.load_path.as_ref()) {
        std::fs::read(path)?
    } else {
        encode(&sim.header())
    };
    let metadata = SaveMetadata {
        total_bytes: u64::try_from(save.len())
            .map_err(|_| SessionError::Protocol("save length exceeds u64".into()))?,
        tick: sim.current_tick(),
        hash: sim.state_hash(),
    };
    let (mut server, mut transport) = bind_server(cfg)?;
    let result = (|| {
        let participants = wait_for_clients(
            &mut server,
            &mut transport,
            cfg.expected_clients,
            &save,
            metadata,
            local_inputs,
            control,
        )?;
        if control.is_cancelled() {
            return Ok(());
        }
        sim.publish()?;
        control.publish(SessionStatus::Live);
        log::info!("[server] live at tick {}", metadata.tick);
        tick_loop(
            &mut server,
            &mut transport,
            sim,
            local_inputs,
            writer.as_mut(),
            participants,
            cfg.desync_policy,
            cfg.tick_duration,
            control,
        )
    })();
    transport.disconnect_all(&mut server);
    result
}

fn bind_server(cfg: &ServerCfg) -> SessionResult<(RenetServer, NetcodeServerTransport)> {
    let socket = UdpSocket::bind(SocketAddr::from(([0, 0, 0, 0], cfg.port)))?;
    socket.set_nonblocking(true)?;
    let address = socket.local_addr()?;
    let current_time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| SessionError::Transport(error.to_string()))?;
    let config = ServerConfig {
        current_time,
        max_clients: (cfg.expected_clients as usize).max(1),
        protocol_id: PROTOCOL_ID,
        public_addresses: vec![address],
        authentication: ServerAuthentication::Unsecure,
    };
    let transport = NetcodeServerTransport::new(config, socket)
        .map_err(|error| SessionError::Transport(error.to_string()))?;
    log::info!("[server] listening on {address}");
    Ok((RenetServer::new(connection_config()), transport))
}

fn update_network(
    server: &mut RenetServer,
    transport: &mut NetcodeServerTransport,
    last: &mut Instant,
) -> SessionResult {
    let now = Instant::now();
    let elapsed = now.saturating_duration_since(*last);
    *last = now;
    transport
        .update(elapsed, server)
        .map_err(|error| SessionError::Transport(error.to_string()))?;
    server.update(elapsed);
    Ok(())
}

fn wait_for_clients(
    server: &mut RenetServer,
    transport: &mut NetcodeServerTransport,
    expected: u32,
    save: &[u8],
    metadata: SaveMetadata,
    local: Option<&Receiver<CommandPayload>>,
    control: &SessionControl,
) -> SessionResult<BTreeMap<ClientId, u64>> {
    let mut peers: BTreeMap<ClientId, StartupPeer> = BTreeMap::new();
    let mut last = Instant::now();
    let mut published_ready = None;
    while !control.is_cancelled() {
        update_network(server, transport, &mut last)?;
        while let Some(event) = server.get_event() {
            match event {
                ServerEvent::ClientConnected { client_id } => {
                    if peers.len() < expected as usize {
                        peers.insert(client_id, StartupPeer::new(Instant::now()));
                    } else {
                        server.disconnect(client_id);
                    }
                }
                ServerEvent::ClientDisconnected { client_id, .. } => {
                    peers.remove(&client_id);
                }
            }
        }
        drain_local(local, false)?;
        for (&id, peer) in &mut peers {
            peer.pump(server, id, save, metadata);
            let mut invalid = false;
            while let Some(bytes) = server.receive_message(id, CHANNEL_LOCKSTEP) {
                let result = decode_client_message(&bytes)
                    .map_err(|error| SessionError::Protocol(error.to_string()))
                    .and_then(|message| peer.receive(message, metadata, Instant::now()));
                if let Err(error) = result {
                    log::error!("[server] startup client {id}: {error}");
                    invalid = true;
                    break;
                }
            }
            // Desync reports are only legal after Start.
            if server.receive_message(id, CHANNEL_DESYNC).is_some() {
                invalid = true;
            }
            if invalid || peer.timed_out(Instant::now()) {
                server.disconnect(id);
            }
        }
        peers.retain(|id, _| server.is_connected(*id));
        let ready = peers.values().filter(|peer| peer.is_ready()).count();
        if published_ready != Some(ready) {
            control.publish(SessionStatus::Waiting {
                ready,
                expected: expected as usize,
            });
            published_ready = Some(ready);
        }
        if ready == expected as usize {
            let start = encode(&ServerMessage::Start {
                tick: metadata.tick,
                hash: metadata.hash,
            });
            server.broadcast_message(CHANNEL_LOCKSTEP, start);
            transport.send_packets(server);
            return Ok(peers.keys().map(|id| (*id, metadata.tick)).collect());
        }
        transport.send_packets(server);
        thread::sleep(Duration::from_millis(5));
    }
    Ok(BTreeMap::new())
}

fn drain_local(
    receiver: Option<&Receiver<CommandPayload>>,
    live: bool,
) -> SessionResult<Vec<CommandPayload>> {
    let mut inputs = Vec::new();
    if let Some(receiver) = receiver {
        for _ in 0..256 {
            match receiver.try_recv() {
                Ok(event) => {
                    if live {
                        inputs.push(event);
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    return Err(SessionError::InputDisconnected);
                }
            }
        }
    }
    Ok(inputs)
}

// Conservatively budget framing before accumulating/encoding a tick. The final
// encoded envelope is also checked; oversized input never reaches disk or peers.
fn account_input(total: &mut usize, payload: usize) -> SessionResult {
    *total = total
        .checked_add(payload)
        .and_then(|n| n.checked_add(32))
        .ok_or_else(|| SessionError::Protocol("tick byte budget exceeded".into()))?;
    if *total > crate::protocol::MAX_REPLAY_RECORD_BYTES {
        return Err(SessionError::Protocol("tick byte budget exceeded".into()));
    }
    Ok(())
}

fn tick_loop(
    server: &mut RenetServer,
    transport: &mut NetcodeServerTransport,
    sim: &mut impl SessionSimulation,
    local: Option<&Receiver<CommandPayload>>,
    mut writer: Option<&mut ReplayWriter>,
    mut verified: BTreeMap<ClientId, u64>,
    policy: DesyncPolicy,
    tick_duration: Duration,
    control: &SessionControl,
) -> SessionResult {
    let mut pacer = TickPacer::new(tick_duration);
    let mut last = Instant::now();
    let mut hashes = BTreeMap::new();
    while !control.is_cancelled() {
        update_network(server, transport, &mut last)?;
        while let Some(event) = server.get_event() {
            match event {
                ServerEvent::ClientConnected { client_id } => server.disconnect(client_id),
                ServerEvent::ClientDisconnected { client_id, reason } => {
                    log::error!("[server] client {client_id} disconnected: {reason}");
                    verified.remove(&client_id);
                }
            }
        }
        let current = sim.current_tick();
        let mut inputs = Vec::new();
        let mut input_bytes = 0usize;
        for id in server.clients_id() {
            let Some(last_verified) = verified.get_mut(&id) else {
                continue;
            };
            while let Some(bytes) = server.receive_message(id, CHANNEL_LOCKSTEP) {
                match decode_client_message(&bytes) {
                    Ok(ClientMessage::Input(event)) => {
                        if let Err(error) = sim.validate_command(&event.0) {
                            log::warn!("invalid command from {id}: {error}");
                            server.disconnect(id);
                            break;
                        }
                        account_input(&mut input_bytes, event.0.len())?;
                        inputs.push(crate::protocol::Command {
                            participant: crate::ParticipantId::Remote(id),
                            payload: event,
                        });
                    }
                    other => {
                        log::error!("[server] client {id}: invalid live client message: {other:?}");
                        server.disconnect(id);
                        break;
                    }
                }
            }
            while let Some(bytes) = server.receive_message(id, CHANNEL_DESYNC) {
                handle_hash_report(
                    server,
                    id,
                    &bytes,
                    current,
                    last_verified,
                    &hashes,
                    policy,
                    sim.hash_interval(),
                )?;
            }
            enforce_verification_deadline(
                server,
                id,
                current,
                *last_verified,
                sim.hash_interval() * 10,
            );
        }
        for payload in drain_local(local, true)? {
            sim.validate_command(&payload.0)?;
            account_input(&mut input_bytes, payload.0.len())?;
            inputs.push(crate::protocol::Command {
                participant: crate::ParticipantId::Host,
                payload,
            });
        }
        let tick = current
            .checked_add(1)
            .ok_or_else(|| SessionError::Protocol("tick exhausted".into()))?;
        let advance = TickAdvance { tick, inputs };
        let encoded = encode(&ServerMessage::Tick(advance.clone()));
        if encoded.len() > crate::protocol::MAX_REPLAY_RECORD_BYTES {
            return Err(SessionError::Protocol("tick byte budget exceeded".into()));
        }
        if let Some(writer) = writer.as_deref_mut() {
            writer.append(&advance)?;
        }
        server.broadcast_message(CHANNEL_LOCKSTEP, encoded);
        advance_tick(sim, advance, TickPhase::Live)?;
        if tick.is_multiple_of(sim.hash_interval()) {
            hashes.insert(tick, sim.state_hash());
            hashes.retain(|old, _| tick.saturating_sub(*old) <= sim.hash_interval() * 10);
            if let Some(writer) = writer.as_deref_mut() {
                writer.flush()?;
            }
        }
        transport.send_packets(server);
        pacer.sleep_until_next();
    }
    Ok(())
}

fn handle_hash_report(
    server: &mut RenetServer,
    id: ClientId,
    bytes: &[u8],
    current: u64,
    verified: &mut u64,
    hashes: &BTreeMap<u64, u64>,
    policy: DesyncPolicy,
    hash_interval: u64,
) -> SessionResult {
    let result = decode_desync_hash(bytes)
        .map_err(|error| SessionError::Protocol(error.to_string()))
        .and_then(|report| verify_hash(id, report, current, verified, hashes, hash_interval));
    if let Err(error) = result {
        if policy == DesyncPolicy::FailFast && matches!(error, SessionError::Desync(_)) {
            return Err(error);
        }
        log::error!("[server] disconnecting client {id}: {error}");
        server.disconnect(id);
    }
    Ok(())
}

fn enforce_verification_deadline(
    server: &mut RenetServer,
    id: ClientId,
    current: u64,
    verified: u64,
    history_window: u64,
) {
    if current.saturating_sub(verified) > history_window {
        log::error!("[server] client {id}: state verification overdue at tick {current}");
        server.disconnect(id);
    }
}

fn verify_hash(
    id: ClientId,
    report: DesyncHash,
    current: u64,
    verified: &mut u64,
    hashes: &BTreeMap<u64, u64>,
    hash_interval: u64,
) -> SessionResult {
    if report.tick <= *verified
        || report.tick > current
        || !report.tick.is_multiple_of(hash_interval)
    {
        return Err(SessionError::Protocol(format!(
            "invalid hash tick {}",
            report.tick
        )));
    }
    let expected = hashes
        .get(&report.tick)
        .ok_or_else(|| SessionError::Protocol("hash outside retained history".into()))?;
    if *expected != report.hash {
        return Err(SessionError::Desync(format!(
            "client {id}, tick {}, expected {expected:#x}, received {:#x}",
            report.tick, report.hash
        )));
    }
    *verified = report.tick;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{decode_save_message, decode_server_message};
    #[test]
    fn validates_hash_ticks_and_fingerprints() {
        let hashes = BTreeMap::from([(30, 123), (60, 456)]);
        let mut verified = 0;
        assert!(
            verify_hash(
                1,
                DesyncHash {
                    tick: 30,
                    hash: 123
                },
                60,
                &mut verified,
                &hashes,
                30
            )
            .is_ok()
        );
        assert_eq!(verified, 30);
        for tick in [0, 30, 31, 90] {
            assert!(
                verify_hash(
                    1,
                    DesyncHash { tick, hash: 123 },
                    60,
                    &mut verified,
                    &hashes,
                    30
                )
                .is_err()
            );
        }
        assert!(matches!(
            verify_hash(
                1,
                DesyncHash { tick: 60, hash: 0 },
                60,
                &mut verified,
                &hashes,
                30
            ),
            Err(SessionError::Desync(_))
        ));
        assert_eq!(verified, 30);
    }
    #[test]
    fn mismatch_policy_contains_failure_and_verification_cannot_expire_silently() {
        let mut server = RenetServer::new(connection_config());
        let _first = server.new_local_client(1);
        let _second = server.new_local_client(2);
        let report = encode(&DesyncHash {
            tick: 30,
            hash: 999,
        });
        let mut verified = 0;
        let hashes = BTreeMap::from([(30, 123)]);
        handle_hash_report(
            &mut server,
            1,
            &report,
            30,
            &mut verified,
            &hashes,
            DesyncPolicy::Disconnect,
            30,
        )
        .unwrap();
        assert!(!server.is_connected(1));
        assert!(server.is_connected(2));
        assert!(matches!(
            handle_hash_report(
                &mut server,
                2,
                &report,
                30,
                &mut verified,
                &hashes,
                DesyncPolicy::FailFast,
                30
            ),
            Err(SessionError::Desync(_))
        ));
        enforce_verification_deadline(&mut server, 2, 300, 0, HASH_HISTORY_WINDOW);
        assert!(server.is_connected(2));
        enforce_verification_deadline(&mut server, 2, 301, 0, HASH_HISTORY_WINDOW);
        assert!(!server.is_connected(2));
    }
    #[test]
    fn bind_failure_is_returned_and_record_header_is_flushed() {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        let directory = tempfile::tempdir().unwrap();
        let record = directory.path().join("failed.save");
        let result = run_server(
            ServerCfg {
                tick_duration: Duration::from_nanos(33_333_333),
                port: socket.local_addr().unwrap().port(),
                expected_clients: 0,
                load_path: None,
                record_path: Some(record.clone()),
                desync_policy: DesyncPolicy::Disconnect,
            },
            None,
            &mut crate::test_simulation::TestSimulation::default(),
            &SessionControl::default(),
        );
        assert!(matches!(result, Err(SessionError::Io(_))));
        let mut reader = ReplayReader::open(&record).unwrap();
        assert!(reader.next_record().unwrap().is_none());
    }
    fn drive_bad_report(
        address: SocketAddr,
        policy: DesyncPolicy,
        control: &SessionControl,
        worker: &mut crate::session::SessionWorker,
    ) -> bool {
        use crate::protocol::{CHANNEL_SAVE, SaveMessage};
        use renet::RenetClient;
        use renet_netcode::{ClientAuthentication, NetcodeClientTransport};
        let deadline = Instant::now() + Duration::from_secs(10);
        while !matches!(&*control.status(), SessionStatus::Waiting { .. }) {
            if Instant::now() >= deadline {
                return false;
            }
            worker.poll(control);
            thread::sleep(Duration::from_millis(2));
        }
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.set_nonblocking(true).unwrap();
        let mut transport = NetcodeClientTransport::new(
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap(),
            ClientAuthentication::Unsecure {
                protocol_id: PROTOCOL_ID,
                client_id: 1,
                server_addr: address,
                user_data: None,
            },
            socket,
        )
        .unwrap();
        let mut client = RenetClient::new(connection_config());
        let mut metadata = None;
        let mut received = 0;
        let mut ready = false;
        let mut start_seen = false;
        let mut report_sent = false;
        let mut last = Instant::now();
        while Instant::now() < deadline {
            let dt = last.elapsed();
            last = Instant::now();
            let update = transport.update(dt, &mut client);
            if update.is_err() && transport.disconnect_reason().is_none() {
                return false;
            }
            client.update(dt);
            worker.poll(control);
            if report_sent && (client.is_disconnected() || transport.disconnect_reason().is_some())
            {
                if policy == DesyncPolicy::FailFast {
                    worker.join(control);
                    return matches!(&*control.status(), SessionStatus::Failed(message) if message.contains("desync"));
                }
                return matches!(&*control.status(), SessionStatus::Live);
            }
            while let Some(bytes) = client.receive_message(CHANNEL_SAVE) {
                match decode_save_message(&bytes).unwrap() {
                    SaveMessage::Begin(value) => metadata = Some(value),
                    SaveMessage::Chunk { bytes } => {
                        received += u64::try_from(bytes.len()).unwrap();
                    }
                }
            }
            if let Some(meta) = metadata
                && received == meta.total_bytes
                && !ready
            {
                client.send_message(
                    CHANNEL_LOCKSTEP,
                    encode(&ClientMessage::Ready {
                        tick: meta.tick,
                        hash: meta.hash,
                    }),
                );
                ready = true;
            }
            while let Some(bytes) = client.receive_message(CHANNEL_LOCKSTEP) {
                match decode_server_message(&bytes).unwrap() {
                    ServerMessage::Start { .. } => start_seen = true,
                    ServerMessage::Tick(advance) if advance.tick == 30 => {
                        assert!(start_seen);
                        // Tick is valid; the deliberately wrong fingerprint must
                        // follow the configured policy, not unwind the worker.
                        client.send_message(
                            CHANNEL_DESYNC,
                            encode(&DesyncHash { tick: 30, hash: 0 }),
                        );
                        report_sent = true;
                    }
                    ServerMessage::Tick(_) => {}
                }
            }
            transport.send_packets(&mut client).unwrap();
            worker.poll(control);
            thread::sleep(Duration::from_millis(2));
        }
        false
    }

    #[test]
    fn udp_desync_policy_and_cleanup_flush_recording() {
        for policy in [DesyncPolicy::Disconnect, DesyncPolicy::FailFast] {
            let directory = tempfile::tempdir().unwrap();
            let record = directory.path().join("session.save");
            let reservation = UdpSocket::bind("127.0.0.1:0").unwrap();
            let address = reservation.local_addr().unwrap();
            drop(reservation);
            let control = SessionControl::default();
            let worker_control = control.clone();
            let record_path = record.clone();
            let mut worker =
                crate::session::SessionWorker::spawn("desync-test-server", move || {
                    run_server(
                        ServerCfg {
                            tick_duration: Duration::from_nanos(33_333_333),
                            port: address.port(),
                            expected_clients: 1,
                            load_path: None,
                            record_path: Some(record_path),
                            desync_policy: policy,
                        },
                        None,
                        &mut crate::test_simulation::TestSimulation::default(),
                        &worker_control,
                    )
                })
                .unwrap();
            let outcome = drive_bad_report(address, policy, &control, &mut worker);
            control.cancel();
            worker.join(&control);
            assert!(outcome, "desync policy {policy:?} did not complete");
            let mut reader = ReplayReader::open(&record).unwrap();
            let mut last_tick = 0;
            while let Some(record) = reader.next_record().unwrap() {
                last_tick = record.tick;
            }
            assert!(last_tick >= 30);
        }
    }
}
