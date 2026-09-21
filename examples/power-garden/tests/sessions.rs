//! Public-API coverage of worker schedules, network roles, saved state, and effects.
use std::{
    net::UdpSocket,
    thread,
    time::{Duration, Instant},
};
use synctick::{
    ClientConfig, Game, Input, ParticipantId, ServerConfig, SessionControl, SessionStatus,
    Simulation, Tick,
};
use synctick_bevy::GameAdapter;
use synctick_example_power_garden::{Initialization, PowerGarden, Rotate};

fn wait(mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready() {
        assert!(
            Instant::now() < deadline,
            "session failed to reach expected state"
        );
        thread::sleep(Duration::from_millis(5));
    }
}
#[test]
fn effects_do_not_survive_replay_or_a_subsequent_tick() {
    let game = PowerGarden::default();
    let snapshots = game.snapshots();
    let mut sim = GameAdapter(game)
        .create(Initialization::default(), Duration::from_millis(10))
        .unwrap();
    sim.advance(Tick {
        number: 1,
        inputs: vec![Input {
            participant: ParticipantId::Host,
            command: Rotate { cell: 7 },
        }],
    })
    .unwrap();
    sim.finish_tick();
    assert!(snapshots.latest().is_none());
    let hash = sim.state_hash();
    sim.publish().unwrap();
    assert!(snapshots.latest().unwrap().effects.is_empty());
    assert_eq!(sim.state_hash(), hash);
    sim.advance(Tick {
        number: 2,
        inputs: vec![Input {
            participant: ParticipantId::Remote(1),
            command: Rotate { cell: 17 },
        }],
    })
    .unwrap();
    sim.publish().unwrap();
    let snapshot = snapshots.latest().unwrap();
    assert_eq!(snapshot.effects.len(), 1);
    assert_eq!(snapshot.effects[0].participant, ParticipantId::Remote(1));
    sim.finish_tick();
    sim.advance(Tick {
        number: 3,
        inputs: vec![],
    })
    .unwrap();
    sim.publish().unwrap();
    sim.finish_tick();
    assert!(snapshots.latest().unwrap().effects.is_empty());
    assert_eq!(snapshot.effects.len(), 1, "old snapshots remain immutable");
}

#[test]
fn host_dedicated_client_replay_and_continuation_agree() {
    for local_player in [true, false] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("garden.save");
        let reservation = UdpSocket::bind("127.0.0.1:0").unwrap();
        let address = reservation.local_addr().unwrap();
        drop(reservation);
        let game = PowerGarden::default();
        let authority_state = game.snapshots();
        let mut config = ServerConfig::new(Initialization::default());
        config.port = address.port();
        config.expected_clients = 1;
        config.record_path = Some(path.clone());
        config.tick_duration = Duration::from_millis(10);
        let mut authority = if local_player {
            synctick::host(GameAdapter(game), config)
        } else {
            synctick::dedicated_server(GameAdapter(game), config)
        }
        .unwrap();
        wait(|| {
            authority.poll();
            matches!(&*authority.status(), SessionStatus::Waiting { .. })
        });
        let game = PowerGarden::default();
        let replica_state = game.snapshots();
        let mut replica =
            synctick::connect(GameAdapter(game), ClientConfig::new(1, address)).unwrap();
        wait(|| {
            replica.poll();
            matches!(&*replica.status(), SessionStatus::Live)
        });
        replica.commands().submit(&Rotate { cell: 7 }).unwrap();
        replica.commands().submit(&Rotate { cell: 12 }).unwrap(); // fixed source, no-op
        replica
            .commands()
            .submit(&Rotate { cell: u32::MAX })
            .unwrap(); // invalid cell, no-op
        if local_player {
            authority.commands().submit(&Rotate { cell: 17 }).unwrap();
        }
        let expected_moves = if local_player { 2 } else { 1 };
        wait(|| {
            replica_state
                .latest()
                .is_some_and(|state| state.board.moves() == expected_moves && state.tick >= 110)
        });
        replica.cancel();
        assert!(authority.shutdown().is_ok());
        assert!(replica.join().is_ok());
        let state = authority_state.latest().unwrap();
        let outcome = synctick::replay(
            GameAdapter(PowerGarden::default()),
            &path,
            &SessionControl::default(),
        )
        .unwrap();
        assert_eq!(outcome.final_hash, state.board.state_hash(state.tick));
        assert_eq!(outcome.final_tick, state.tick);
        let continued = directory.path().join("continued.save");
        let game = PowerGarden::default();
        let resumed = game.snapshots();
        let mut config = ServerConfig::new(Initialization {
            width: 0,
            height: 0,
            tiles: vec![],
        });
        config.port = 0;
        config.load_path = Some(path);
        config.record_path = Some(continued.clone());
        let mut host = synctick::host(GameAdapter(game), config).unwrap();
        wait(|| {
            host.poll();
            resumed.latest().is_some_and(|next| next.tick > state.tick)
        });
        assert!(host.shutdown().is_ok());
        let final_state = resumed.latest().unwrap();
        assert_eq!(final_state.board, state.board);
        assert!(final_state.effects.is_empty());
        let replay = synctick::replay(
            GameAdapter(PowerGarden::default()),
            &continued,
            &SessionControl::default(),
        )
        .unwrap();
        assert_eq!(
            replay.final_hash,
            final_state.board.state_hash(final_state.tick)
        );
    }
}
