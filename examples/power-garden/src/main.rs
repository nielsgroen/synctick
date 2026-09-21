//! GUI launcher and headless tools for Power Garden.
mod menu;
mod presentation;

use clap::{Parser, Subcommand};
use std::{path::PathBuf, thread, time::Duration};
use synctick::{ServerConfig, SessionControl, SessionStatus};
use synctick_bevy::GameAdapter;
use synctick_example_power_garden::{Initialization, PowerGarden};

#[derive(Parser)]
#[command(about = "Power Garden — connect the light, grow together")]
struct Cli {
    #[command(subcommand)]
    mode: Option<Mode>,
}
#[derive(Subcommand)]
enum Mode {
    /// Run an authority without graphics; wait for one remote player.
    Server {
        #[arg(long, default_value_t = 5000)]
        port: u16,
        #[arg(long)]
        load: Option<PathBuf>,
        #[arg(long)]
        record: Option<PathBuf>,
    },
    /// Replay offline and print the final tick and deterministic state hash.
    Replay { path: PathBuf },
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    match cli.mode {
        None => presentation::run(),
        Some(mode) => {
            tracing_subscriber::fmt()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| "info".into()),
                )
                .init();
            match mode {
                Mode::Replay { path } => {
                    println!(
                        "{:?}",
                        synctick::replay(
                            GameAdapter(PowerGarden::default()),
                            &path,
                            &SessionControl::default()
                        )?
                    );
                }
                Mode::Server { port, load, record } => {
                    let mut config = ServerConfig::new(Initialization::default());
                    config.port = port;
                    config.expected_clients = 1;
                    config.load_path = load;
                    config.record_path = record;
                    let mut session =
                        synctick::dedicated_server(GameAdapter(PowerGarden::default()), config)?;
                    let control = session.control();
                    thread::Builder::new()
                        .name("garden-console".into())
                        .spawn(move || {
                            if let Err(error) = std::io::stdin().read_line(&mut String::new()) {
                                eprintln!("Console input: {error}");
                            }
                            control.cancel();
                        })?;
                    println!("Power Garden server — press Enter to stop.");
                    while !session.control().is_cancelled() {
                        session.poll();
                        if matches!(
                            &*session.status(),
                            SessionStatus::Failed(_) | SessionStatus::Stopped
                        ) {
                            break;
                        }
                        thread::sleep(Duration::from_millis(10));
                    }
                    if let Err(error) = &*session.shutdown() {
                        return Err(error.to_string().into());
                    }
                }
            }
            Ok(())
        }
    }
}
