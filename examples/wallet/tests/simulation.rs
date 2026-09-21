//! Identity, rejection, and publication contracts without a network transport.
use std::time::Duration;
use synctick::{Game, Input, ParticipantId, Simulation, Tick};
use synctick_example_wallet::{Initialization, Transfer, WalletGame};

#[test]
fn replay_and_live_share_rules_but_only_live_publishes() {
    let live = WalletGame::default();
    let replay = WalletGame::default();
    let snapshots = live.snapshots();
    let replay_snapshots = replay.snapshots();
    let initialization = Initialization {
        seed: 42,
        balances: vec![100, 200],
    };
    let mut authority = live
        .create(initialization.clone(), Duration::from_millis(10))
        .unwrap();
    let mut replay = replay
        .create(initialization, Duration::from_millis(10))
        .unwrap();
    assert!(snapshots.latest().is_none());
    authority.publish().unwrap();
    let initial_snapshot = snapshots.latest().unwrap();

    for (index, participant) in [
        ParticipantId::Remote(0),
        ParticipantId::Host,
        ParticipantId::Remote(1),
    ]
    .into_iter()
    .enumerate()
    {
        let tick = u64::try_from(index).unwrap() + 1;
        for simulation in [&mut authority, &mut replay] {
            simulation
                .advance(Tick {
                    number: tick,
                    inputs: vec![Input {
                        participant,
                        command: Transfer {
                            recipient: 1,
                            amounts: vec![3, 7],
                        },
                    }],
                })
                .unwrap();
        }
        authority.publish().unwrap();
        authority.finish_tick();
        replay.finish_tick();
        assert_eq!(authority.state_hash(), replay.state_hash());
    }

    let state = snapshots.latest().unwrap();
    assert_eq!(state.balances, [90, 210]);
    assert_eq!(state.rejected, 1);
    assert_eq!(state.tick, 3);
    assert_eq!(initial_snapshot.balances, [100, 200]);
    assert_eq!(initial_snapshot.tick, 0);
    assert!(replay_snapshots.latest().is_none());
}

#[test]
fn empty_initialization_fails_before_publication() {
    let game = WalletGame::default();
    assert!(
        game.create(
            Initialization {
                seed: 42,
                balances: vec![]
            },
            Duration::from_millis(10)
        )
        .is_err()
    );
    assert!(game.snapshots().latest().is_none());
}
