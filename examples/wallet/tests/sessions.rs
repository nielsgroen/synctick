//! Host/client/replay acceptance coverage through public game and framework APIs.
use std::{
    net::UdpSocket,
    thread,
    time::{Duration, Instant},
};
use synctick::{ClientConfig, ServerConfig, SessionControl, SessionStatus};
use synctick_example_wallet::{Initialization, Transfer, WalletGame};

#[derive(Debug, Clone, Copy)]
enum Authority {
    Host,
    Dedicated,
}

fn wait(mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready() {
        assert!(Instant::now() < deadline, "session timed out");
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn public_api_host_client_dedicated_and_replay_agree() {
    for mode in [Authority::Dedicated, Authority::Host] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("wallet.save");
        let reservation = UdpSocket::bind("127.0.0.1:0").unwrap();
        let address = reservation.local_addr().unwrap();
        drop(reservation);
        let game = WalletGame::default();
        let authority_snapshots = game.snapshots();
        let mut config = ServerConfig::new(Initialization {
            seed: 42,
            balances: vec![100, 200, 300],
        });
        config.port = address.port();
        config.expected_clients = 1;
        config.record_path = Some(path.clone());
        // Exercise cadence other than the default 30 Hz.
        config.tick_duration = Duration::from_millis(10);
        let mut authority = if matches!(mode, Authority::Host) {
            synctick::host(game, config)
        } else {
            synctick::dedicated_server(game, config)
        }
        .unwrap();
        wait(|| {
            authority.poll();
            matches!(&*authority.status(), SessionStatus::Waiting { .. })
        });
        let client_game = WalletGame::default();
        let client_snapshots = client_game.snapshots();
        let mut client = synctick::connect(client_game, ClientConfig::new(1, address)).unwrap();
        wait(|| {
            client.poll();
            matches!(&*client.status(), SessionStatus::Live)
        });
        client
            .commands()
            .submit(&Transfer {
                recipient: 2,
                amounts: vec![3, 7],
            })
            .unwrap();
        client
            .commands()
            .submit(&Transfer {
                recipient: 2,
                amounts: vec![u64::MAX, 1],
            })
            .unwrap();
        if matches!(mode, Authority::Host) {
            authority
                .commands()
                .submit(&Transfer {
                    recipient: 2,
                    amounts: vec![5],
                })
                .unwrap();
        }
        let expected = if matches!(mode, Authority::Host) {
            vec![95, 190, 315]
        } else {
            vec![100, 190, 310]
        };
        wait(|| {
            let state = client_snapshots.latest().unwrap();
            state.balances == expected && state.rejected == 1 && state.tick >= 110
        });
        client.cancel();
        assert!(authority.shutdown().is_ok());
        assert!(client.join().is_ok());
        let state = authority_snapshots.latest().unwrap();
        assert_eq!(state.balances, expected);
        let outcome =
            synctick::replay(WalletGame::default(), &path, &SessionControl::default()).unwrap();
        assert_eq!(outcome.final_hash, state.state_hash());
        assert_eq!(outcome.final_tick, state.tick);
        assert!(!outcome.cancelled);
        check_continuation(&directory, path, &expected, outcome.final_tick);
    }
}
fn check_continuation(
    directory: &tempfile::TempDir,
    path: std::path::PathBuf,
    expected: &[u64],
    final_tick: u64,
) {
    // Resume adopts saved initialization and tick duration, not the supplied fresh defaults.
    let continued_recording = directory.path().join("continued.save");
    let game = WalletGame::default();
    let resumed_snapshots = game.snapshots();
    let mut config = ServerConfig::new(Initialization {
        seed: 999,
        balances: vec![0],
    });
    config.port = 0;
    config.load_path = Some(path);
    config.record_path = Some(continued_recording.clone());
    let mut host = synctick::host(game, config).unwrap();
    wait(|| {
        host.poll();
        resumed_snapshots
            .latest()
            .is_some_and(|state| state.tick > final_tick)
    });
    assert!(host.shutdown().is_ok());
    assert_eq!(resumed_snapshots.latest().unwrap().balances, expected);
    let result = synctick::replay(
        WalletGame::default(),
        &continued_recording,
        &SessionControl::default(),
    )
    .unwrap();
    assert!(result.final_tick > final_tick);
}
