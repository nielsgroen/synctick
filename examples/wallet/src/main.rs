//! Console presentation using only public game and session APIs.
mod cli;

use clap::Parser;
use cli::{Cli, Mode};
use std::{
    error::Error,
    net::{Ipv4Addr, SocketAddr},
    thread,
    time::Duration,
};
use synctick::{ClientConfig, SessionControl, SessionHandle, SessionStatus, SubmitError};
use synctick_example_wallet::{Snapshots, Transfer, WalletGame};

type AppResult = Result<(), Box<dyn Error>>;
const POLL_INTERVAL: Duration = Duration::from_millis(10);

fn main() -> AppResult {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let game = WalletGame::default();
    let snapshots = game.snapshots();
    match Cli::parse().mode() {
        Mode::Host(options) => run_live(
            synctick::host(game, options.into_config())?,
            snapshots,
            Some(Transfer {
                recipient: 1,
                amounts: vec![3, 7],
            }),
        ),
        Mode::Server(options) => run_live(
            synctick::dedicated_server(game, options.into_config())?,
            snapshots,
            None,
        ),
        Mode::Client => run_live(
            synctick::connect(
                game,
                ClientConfig::new(1, SocketAddr::from((Ipv4Addr::LOCALHOST, 5000))),
            )?,
            snapshots,
            Some(Transfer {
                recipient: 0,
                amounts: vec![2, 3],
            }),
        ),
        Mode::Replay { path } => {
            let outcome = synctick::replay(game, &path, &SessionControl::default())?;
            println!("{outcome:?}");
            Ok(())
        }
    }
}

/// Own the session through console startup, polling, and explicit shutdown.
fn run_live(
    mut session: SessionHandle<Transfer>,
    snapshots: Snapshots,
    demo_transfer: Option<Transfer>,
) -> AppResult {
    let control = session.control();
    let presentation = cancel_on_enter(control.clone())
        .map_err(Into::into)
        .and_then(|()| {
            println!("Press Enter to stop the session.");
            present(&mut session, &control, &snapshots, demo_transfer).map_err(Into::into)
        });
    // Always inspect the worker result, including flush failures, even when
    // console setup or command submission failed first.
    let terminal = session.shutdown();
    match (presentation, &*terminal) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(error.to_string().into()),
        (Err(presentation), Err(worker)) => {
            Err(format!("{presentation}; session shutdown also failed: {worker}").into())
        }
    }
}

/// Submit once when live and print each observed change in balances.
fn present(
    session: &mut SessionHandle<Transfer>,
    control: &SessionControl,
    snapshots: &Snapshots,
    mut demo_transfer: Option<Transfer>,
) -> Result<(), SubmitError> {
    let mut last_balances = None;
    while !control.is_cancelled() {
        session.poll();
        match &*session.status() {
            SessionStatus::Stopped | SessionStatus::Failed(_) => break,
            SessionStatus::Live => {
                if let Some(command) = demo_transfer.take() {
                    session.commands().submit(&command)?;
                }
                if let Some(state) = snapshots.latest()
                    && last_balances.as_ref() != Some(&state.balances)
                {
                    println!("tick {}: balances {:?}", state.tick, state.balances);
                    last_balances = Some(state.balances.clone());
                }
            }
            _ => {}
        }
        thread::sleep(POLL_INTERVAL);
    }
    Ok(())
}

/// Connect Enter or EOF to cancellation; report input errors before cancelling.
///
/// The console reader may block until process exit if the session fails first.
/// It owns no simulation or session handle and is intentionally not joined.
fn cancel_on_enter(control: SessionControl) -> std::io::Result<()> {
    thread::Builder::new()
        .name("wallet-console".into())
        .spawn(move || {
            let mut line = String::new();
            if let Err(error) = std::io::stdin().read_line(&mut line) {
                eprintln!("Could not read console input: {error}");
            }
            control.cancel();
        })?;
    Ok(())
}
