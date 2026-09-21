//! Remote replica: bounded loading, verified Start, then authoritative ticks.
use crate::protocol::CommandPayload;
use crate::protocol::{
    CHANNEL_DESYNC, CHANNEL_LOCKSTEP, CHANNEL_SAVE, ClientMessage, DesyncHash, PROTOCOL_ID,
    ServerMessage, TickAdvance, connection_config, decode_save_message, decode_server_message,
    encode,
};
use crate::session::{SessionControl, SessionError, SessionResult, SessionStatus};
use crate::simulation::{SessionSimulation, TickPhase, advance_tick};
use crate::startup::ClientStartup;
use crossbeam_channel::{Receiver, TryRecvError};
use renet::RenetClient;
use renet_netcode::{ClientAuthentication, NetcodeClientTransport};
use std::net::{SocketAddr, UdpSocket};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub struct ClientCfg {
    pub id: u64,
    pub server_addr: SocketAddr,
}

/// # Errors
/// Returns connection, protocol, replay, or input-channel failures.
pub fn run_client(
    cfg: ClientCfg,
    sim: &mut impl SessionSimulation,
    input_rx: Receiver<CommandPayload>,
    control: &SessionControl,
) -> SessionResult {
    let (mut client, mut transport) = bind_client(&cfg)?;
    let result = client_loop(&mut client, &mut transport, sim, &input_rx, control);
    transport.disconnect();
    result
}

fn client_loop(
    client: &mut RenetClient,
    transport: &mut NetcodeClientTransport,
    sim: &mut impl SessionSimulation,
    input_rx: &Receiver<CommandPayload>,
    control: &SessionControl,
) -> SessionResult {
    let mut startup = ClientStartup::default();
    let mut last_poll = Instant::now();
    let mut published = Instant::now();
    let mut reported = (0, 0);
    while !control.is_cancelled() {
        let now = Instant::now();
        let dt = now.saturating_duration_since(last_poll);
        last_poll = now;
        transport
            .update(dt, client)
            .map_err(|error| SessionError::Transport(error.to_string()))?;
        client.update(dt);
        if client.is_disconnected() {
            return Err(SessionError::Transport(format!(
                "disconnected: {:?}",
                client.disconnect_reason()
            )));
        }
        forward_inputs(client, input_rx, startup.is_live())?;
        while let Some(bytes) = client.receive_message(CHANNEL_SAVE) {
            let message = decode_save_message(&bytes)
                .map_err(|error| SessionError::Protocol(format!("save message: {error}")))?;
            startup.receive_save(message)?;
        }
        // Acknowledge application consumption promptly, independently of UI
        // progress throttling. This releases the server's receive-memory window.
        let progress = startup.progress();
        if progress.0 != reported.0 {
            client.send_message(
                CHANNEL_LOCKSTEP,
                encode(&ClientMessage::Progress {
                    received_bytes: progress.0,
                    replayed_tick: progress.1,
                }),
            );
            reported = progress;
        }
        if let Some(ready) = startup.step(sim, control)? {
            client.send_message(CHANNEL_LOCKSTEP, encode(&ready));
            control.publish(SessionStatus::AwaitingStart);
        }
        if startup.is_loading() && published.elapsed() >= Duration::from_secs(1) {
            let progress = startup.progress();
            if progress != reported {
                client.send_message(
                    CHANNEL_LOCKSTEP,
                    encode(&ClientMessage::Progress {
                        received_bytes: progress.0,
                        replayed_tick: progress.1,
                    }),
                );
                reported = progress;
            }
            control.publish(SessionStatus::Loading {
                phase: if progress.1 == 0 {
                    crate::session::LoadingPhase::ReceivingSave
                } else {
                    crate::session::LoadingPhase::ReplayingSave
                },
                completed: if progress.1 == 0 {
                    progress.0
                } else {
                    progress.1
                },
            });
            published = Instant::now();
        }
        // Keep transport servicing bounded even when catching up on live ticks.
        let started = Instant::now();
        for _ in 0..128 {
            if control.is_cancelled() || started.elapsed() >= Duration::from_millis(5) {
                break;
            }
            let Some(bytes) = client.receive_message(CHANNEL_LOCKSTEP) else {
                break;
            };
            match decode_server_message(&bytes)
                .map_err(|error| SessionError::Protocol(format!("server message: {error}")))?
            {
                ServerMessage::Start { tick, hash } => {
                    start_live(sim, &mut startup, tick, hash, control)?;
                }
                ServerMessage::Tick(advance) if startup.is_live() => {
                    apply_tick(sim, advance, client)?;
                }
                ServerMessage::Tick(_) => {
                    return Err(SessionError::Protocol("tick before Start".into()));
                }
            }
        }
        if client.receive_message(CHANNEL_DESYNC).is_some() {
            return Err(SessionError::Protocol(
                "unexpected server desync message".into(),
            ));
        }
        transport
            .send_packets(client)
            .map_err(|error| SessionError::Transport(error.to_string()))?;
        thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

fn start_live(
    sim: &mut impl SessionSimulation,
    startup: &mut ClientStartup,
    tick: u64,
    hash: u64,
    control: &SessionControl,
) -> SessionResult {
    startup.start(tick, hash)?;
    sim.publish()?;
    control.publish(SessionStatus::Live);
    Ok(())
}

fn forward_inputs(
    client: &mut RenetClient,
    input_rx: &Receiver<CommandPayload>,
    live: bool,
) -> SessionResult {
    for _ in 0..256 {
        match input_rx.try_recv() {
            Ok(event) if live => {
                client.send_message(CHANNEL_LOCKSTEP, encode(&ClientMessage::Input(event)));
            }
            Ok(_) => {} // Pre-start input is discarded, never deferred into tick one.
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => {
                return Err(SessionError::InputDisconnected);
            }
        }
    }
    Ok(())
}

fn apply_tick(
    sim: &mut impl SessionSimulation,
    advance: TickAdvance,
    client: &mut RenetClient,
) -> SessionResult {
    let tick = advance.tick;
    advance_tick(sim, advance, TickPhase::Live)?;
    if tick.is_multiple_of(sim.hash_interval()) {
        client.send_message(
            CHANNEL_DESYNC,
            encode(&DesyncHash {
                tick,
                hash: sim.state_hash(),
            }),
        );
    }
    Ok(())
}

fn bind_client(cfg: &ClientCfg) -> SessionResult<(RenetClient, NetcodeClientTransport)> {
    let bind_address = if cfg.server_addr.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    };
    let socket = UdpSocket::bind(bind_address)?;
    socket.set_nonblocking(true)?;
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| SessionError::Transport(error.to_string()))?;
    let authentication = ClientAuthentication::Unsecure {
        protocol_id: PROTOCOL_ID,
        client_id: cfg.id,
        server_addr: cfg.server_addr,
        user_data: None,
    };
    let transport = NetcodeClientTransport::new(time, authentication, socket)
        .map_err(|error| SessionError::Transport(error.to_string()))?;
    Ok((RenetClient::new(connection_config()), transport))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        protocol::{SaveMessage, SaveMetadata},
        replay::ReplayHeader,
        test_simulation::TestSimulation,
    };

    #[test]
    fn live_tick_gap_is_a_reported_failure() {
        let mut sim = TestSimulation::default();
        let mut client = RenetClient::new(connection_config());
        assert!(
            apply_tick(
                &mut sim,
                TickAdvance {
                    tick: 2,
                    inputs: vec![]
                },
                &mut client
            )
            .is_err()
        );
        assert_eq!(sim.current_tick(), 0);
        assert_eq!(sim.live_ticks, 0);
    }

    #[test]
    fn start_hook_runs_only_after_verification_before_live() {
        for loaded_tick in [0, 5] {
            let mut sim = TestSimulation::default();
            let mut authority = TestSimulation::default();
            let mut bytes = encode(&ReplayHeader::current());
            for tick in 1..=loaded_tick {
                let advance = TickAdvance {
                    tick,
                    inputs: vec![CommandPayload(vec![0]).into()],
                };
                let payload = encode(&advance);
                bytes.extend(u32::try_from(payload.len()).unwrap().to_le_bytes());
                bytes.extend(payload);
                advance_tick(&mut authority, advance, TickPhase::Replay).unwrap();
            }
            let hash = authority.state_hash();
            let mut startup = ClientStartup::default();
            startup
                .receive_save(SaveMessage::Begin(SaveMetadata {
                    total_bytes: u64::try_from(bytes.len()).unwrap(),
                    tick: loaded_tick,
                    hash,
                }))
                .unwrap();
            startup.receive_save(SaveMessage::Chunk { bytes }).unwrap();
            let control = SessionControl::default();
            while startup.step(&mut sim, &control).unwrap().is_none() {}
            assert_eq!(sim.starts, 0);
            assert_eq!(sim.live_ticks, 0);
            assert!(start_live(&mut sim, &mut startup, loaded_tick, hash ^ 1, &control).is_err());
            assert_eq!(sim.starts, 0);
            start_live(&mut sim, &mut startup, loaded_tick, hash, &control).unwrap();
            assert_eq!(*control.status(), SessionStatus::Live);
            assert_eq!(sim.starts, 1);
            assert_eq!(sim.current_tick(), loaded_tick);
            assert!(start_live(&mut sim, &mut startup, loaded_tick, hash, &control).is_err());
            let mut client = RenetClient::new(connection_config());
            apply_tick(
                &mut sim,
                TickAdvance {
                    tick: loaded_tick + 1,
                    inputs: vec![],
                },
                &mut client,
            )
            .unwrap();
            assert_eq!(sim.current_tick(), loaded_tick + 1);
            assert_eq!(sim.live_ticks, 1);
        }
    }
    fn wait_for(mut ready: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if ready() {
                return true;
            }
            thread::sleep(Duration::from_millis(5));
        }
        false
    }

    #[test]
    fn udp_session_replays_drives_commands_and_flushes_without_engine() {
        use crate::{
            replay::{ReplayReader, ReplayWriter, replay_into},
            run_server::{DesyncPolicy, ServerCfg, run_server},
            session::SessionWorker,
        };
        use std::sync::{
            Arc,
            atomic::{AtomicU64, Ordering},
            mpsc,
        };
        let directory = tempfile::tempdir().unwrap();
        let load = directory.path().join("load.save");
        let record = directory.path().join("record.save");
        let mut writer = ReplayWriter::create(&load, &ReplayHeader::current()).unwrap();
        for tick in 1..=5 {
            writer
                .append(&TickAdvance {
                    tick,
                    inputs: vec![CommandPayload(vec![0]).into()],
                })
                .unwrap();
        }
        writer.flush().unwrap();
        drop(writer);
        let reservation = UdpSocket::bind("127.0.0.1:0").unwrap();
        let address = reservation.local_addr().unwrap();
        drop(reservation);
        let server_control = SessionControl::default();
        let client_control = SessionControl::default();
        let worker_control = server_control.clone();
        let record_path = record.clone();
        let (server_result, server_receiver) = mpsc::channel();
        let mut server = SessionWorker::spawn("net-only-server", move || {
            let mut sim = TestSimulation::default();
            let result = run_server(
                ServerCfg {
                    port: address.port(),
                    expected_clients: 1,
                    load_path: Some(load),
                    record_path: Some(record_path),
                    desync_policy: DesyncPolicy::Disconnect,
                    tick_duration: Duration::from_nanos(33_333_333),
                },
                None,
                &mut sim,
                &worker_control,
            );
            server_result.send(sim).unwrap();
            result
        })
        .unwrap();
        let bound = wait_for(|| matches!(&*server_control.status(), SessionStatus::Waiting { .. }));
        let observed = Arc::new(AtomicU64::new(0));
        let observed_sim = observed.clone();
        let worker_control = client_control.clone();
        let (inputs, receiver) = crossbeam_channel::unbounded();
        let (client_result, client_receiver) = mpsc::channel();
        let mut client = SessionWorker::spawn("net-only-client", move || {
            let mut sim = TestSimulation {
                observed_commands: Some(observed_sim),
                ..Default::default()
            };
            let result = run_client(
                ClientCfg {
                    id: 17,
                    server_addr: address,
                },
                &mut sim,
                receiver,
                &worker_control,
            );
            client_result.send(sim).unwrap();
            result
        })
        .unwrap();
        let live = bound && wait_for(|| matches!(&*client_control.status(), SessionStatus::Live));
        let sent = live && inputs.send(CommandPayload(vec![0])).is_ok();
        let applied = sent && wait_for(|| observed.load(Ordering::Acquire) == 6);
        client_control.cancel();
        server_control.cancel();
        client.join(&client_control);
        server.join(&server_control);
        assert!(
            applied,
            "network-only session failed: {:?}, {:?}",
            server_control.status(),
            client_control.status()
        );
        let mut authority = server_receiver.recv().unwrap();
        let replica = client_receiver.recv().unwrap();
        assert_eq!(replica.starts, 1);
        assert_eq!(authority.starts, 1);
        assert!(replica.live_ticks > 0);
        assert_eq!(*server_control.status(), SessionStatus::Stopped);
        let mut replayed = TestSimulation::default();
        let mut reader = ReplayReader::open(&record).unwrap();
        replay_into(&mut replayed, &mut reader, None, &SessionControl::default()).unwrap();
        assert_eq!(authority.state_hash(), replayed.state_hash());
        assert_eq!(replayed.commands, 6);
    }
}
