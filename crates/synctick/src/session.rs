//! Worker lifecycle shared by headless, host, and remote-client runtimes.
use arc_swap::ArcSwap;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("configuration: {0}")]
    Configuration(String),
    #[error("compatibility: {0}")]
    Compatibility(String),
    #[error(transparent)]
    Codec(#[from] crate::codec::CodecError),
    #[error("simulation: {0}")]
    Simulation(String),
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("transport: {0}")]
    Transport(String),
    #[error("simulation input channel disconnected")]
    InputDisconnected,
    #[error("worker: {0}")]
    Worker(String),
    #[error("desync: {0}")]
    Desync(String),
}

pub type SessionResult<T = ()> = Result<T, SessionError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadingPhase {
    LoadingSave,
    ReceivingSave,
    ReplayingSave,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionStatus {
    Connecting,
    Loading { phase: LoadingPhase, completed: u64 },
    Waiting { ready: usize, expected: usize },
    AwaitingStart,
    Live,
    Stopped,
    Failed(String),
}

/// Presentation status never enters deterministic simulation state.
#[derive(Clone)]
pub struct SessionControl {
    cancelled: Arc<AtomicBool>,
    status: Arc<ArcSwap<SessionStatus>>,
}

impl Default for SessionControl {
    fn default() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            status: Arc::new(ArcSwap::from_pointee(SessionStatus::Connecting)),
        }
    }
}

impl SessionControl {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
    #[must_use]
    pub fn status(&self) -> Arc<SessionStatus> {
        self.status.load_full()
    }
    pub(crate) fn publish(&self, status: SessionStatus) {
        self.status.store(Arc::new(status));
    }
}

/// Owns the join handle: completion (including panic) must be observed.
pub struct SessionWorker(
    Option<JoinHandle<SessionResult>>,
    Option<Arc<SessionResult>>,
);

impl SessionWorker {
    /// # Errors
    /// Returns an error if the operating system cannot create the thread.
    pub fn spawn<F>(name: &str, work: F) -> SessionResult<Self>
    where
        F: FnOnce() -> SessionResult + Send + 'static,
    {
        Ok(Self(
            Some(thread::Builder::new().name(name.into()).spawn(work)?),
            None,
        ))
    }

    pub fn poll(&mut self, control: &SessionControl) {
        if self.0.as_ref().is_some_and(JoinHandle::is_finished) {
            self.join(control);
        }
    }

    pub fn join(&mut self, control: &SessionControl) -> Arc<SessionResult> {
        if let Some(result) = &self.1 {
            return result.clone();
        }
        let result = match self
            .0
            .take()
            .expect("worker handle or terminal result exists")
            .join()
        {
            Ok(Ok(()) | Err(SessionError::InputDisconnected)) if control.is_cancelled() => Ok(()),
            Ok(Ok(())) => Err(SessionError::Worker(
                "simulation worker stopped unexpectedly".into(),
            )),
            Ok(Err(error)) => Err(error),
            Err(payload) => Err(SessionError::Worker(format!(
                "simulation worker panicked: {}",
                payload
                    .downcast_ref::<String>()
                    .map(String::as_str)
                    .or_else(|| payload.downcast_ref::<&str>().copied())
                    .unwrap_or("unknown panic")
            ))),
        };
        if let Err(error) = &result {
            log::error!("{error}");
            control.cancel();
            control.publish(SessionStatus::Failed(error.to_string()));
        } else if !matches!(&*control.status(), SessionStatus::Failed(_)) {
            control.publish(SessionStatus::Stopped);
        }
        let result = Arc::new(result);
        self.1 = Some(result.clone());
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn observes_completion_errors_panics_and_cancellation() {
        for mode in 0..4 {
            let control = SessionControl::default();
            if mode == 0 {
                control.cancel();
            }
            let mut worker = SessionWorker::spawn("test-worker", move || match mode {
                2 => Err(SessionError::Worker("failed".into())),
                3 => panic!("test panic"),
                _ => Ok(()),
            })
            .unwrap();
            worker.join(&control);
            if mode == 0 {
                assert_eq!(*control.status(), SessionStatus::Stopped);
            } else {
                assert!(matches!(&*control.status(), SessionStatus::Failed(_)));
            }
        }
    }
}
