//! Command-line parsing and conversion to framework configuration.
use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;
use synctick::ServerConfig;
use synctick_example_wallet::Initialization;

#[derive(Parser)]
#[command(
    about = "Deterministic wallet game",
    long_about = "Deterministic wallet game. Start a host or server, then a client in a second terminal. Each participant submits one demo transfer on live startup. Press Enter to stop."
)]
pub struct Cli {
    #[command(subcommand)]
    mode: Option<Mode>,
}

impl Cli {
    pub fn mode(self) -> Mode {
        self.mode
            .unwrap_or_else(|| Mode::Host(AuthorityOptions::default()))
    }
}

#[derive(Subcommand)]
pub enum Mode {
    /// Host wallet zero and wait for one remote participant on localhost:5000.
    Host(AuthorityOptions),
    /// Run an authority without a local player; wait for one remote participant.
    Server(AuthorityOptions),
    /// Connect as participant one to localhost:5000.
    Client,
    /// Replay a recording offline and print its final tick and state hash.
    Replay {
        /// Recording to replay.
        path: PathBuf,
    },
}

#[derive(Args, Default)]
pub struct AuthorityOptions {
    /// Record to a new file; the destination must not already exist.
    record: Option<PathBuf>,
    /// Continue a recording using its saved initialization and tick duration.
    #[arg(long)]
    load: Option<PathBuf>,
}

impl AuthorityOptions {
    pub fn into_config(self) -> ServerConfig<Initialization> {
        let mut config = ServerConfig::new(Initialization {
            seed: 42,
            balances: vec![100, 200],
        });
        config.expected_clients = 1;
        config.record_path = self.record;
        config.load_path = self.load;
        config
    }
}
