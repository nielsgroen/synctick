//! Hash format v2 rejects old saves and peers before constructing their simulations.
use std::{
    net::UdpSocket,
    thread,
    time::{Duration, Instant},
};
use synctick::{ClientConfig, Game, ServerConfig, SessionError, SessionStatus};
use synctick_bevy::GameAdapter;
use synctick_example_power_garden::{Initialization, PowerGarden};

type Current = GameAdapter<PowerGarden>;

// The old header contract, without retaining an obsolete hash implementation.
struct Legacy {
    allow_factory: bool,
}
impl Game for Legacy {
    type Command = <Current as Game>::Command;
    type Initialization = <Current as Game>::Initialization;
    type Simulation = <Current as Game>::Simulation;
    const ID: [u8; 16] = Current::ID;
    const VERSION: u32 = 1;
    fn create(
        &self,
        initialization: Self::Initialization,
        duration: Duration,
    ) -> synctick::SessionResult<Self::Simulation> {
        assert!(
            self.allow_factory,
            "incompatible peer reached the simulation factory"
        );
        (GameAdapter(PowerGarden::default())).create(initialization, duration)
    }
}
fn wait(mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready() {
        assert!(Instant::now() < deadline, "compatibility test timed out");
        thread::sleep(Duration::from_millis(5));
    }
}
#[test]
fn version_one_save_and_peer_are_rejected() {
    assert_eq!(Current::VERSION, 2);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("v1.save");
    let mut config = ServerConfig::new(Initialization::default());
    config.port = 0;
    config.expected_clients = 1;
    config.record_path = Some(path.clone());
    let mut old = synctick::host(
        Legacy {
            allow_factory: true,
        },
        config,
    )
    .unwrap();
    wait(|| {
        old.poll();
        matches!(&*old.status(), SessionStatus::Waiting { .. })
    });
    assert!(old.shutdown().is_ok());
    assert!(
        matches!(synctick::replay(GameAdapter(PowerGarden::default()), &path, &synctick::SessionControl::default()),
        Err(SessionError::Compatibility(message)) if message == "different game version")
    );
    let mut load = ServerConfig::new(Initialization::default());
    load.port = 0;
    load.load_path = Some(path);
    load.record_path = Some(directory.path().join("must-not-exist.save"));
    assert!(
        matches!(synctick::host(GameAdapter(PowerGarden::default()), load),
        Err(SessionError::Compatibility(message)) if message == "different game version")
    );
    assert!(!directory.path().join("must-not-exist.save").exists());

    let reservation = UdpSocket::bind("127.0.0.1:0").unwrap();
    let address = reservation.local_addr().unwrap();
    drop(reservation);
    let mut config = ServerConfig::new(Initialization::default());
    config.port = address.port();
    config.expected_clients = 1;
    let mut server =
        synctick::dedicated_server(GameAdapter(PowerGarden::default()), config).unwrap();
    wait(|| {
        server.poll();
        matches!(&*server.status(), SessionStatus::Waiting { .. })
    });
    let mut client = synctick::connect(
        Legacy {
            allow_factory: false,
        },
        ClientConfig::new(1, address),
    )
    .unwrap();
    wait(|| {
        client.poll();
        matches!(&*client.status(), SessionStatus::Failed(_))
    });
    assert!(matches!(
        &*client.join(),
        Err(SessionError::Compatibility(_))
    ));
    assert!(matches!(&*server.status(), SessionStatus::Waiting { .. }));
    assert!(server.shutdown().is_ok());
}
