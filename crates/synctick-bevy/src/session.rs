//! Presentation-side session attachment, resources, and owning teardown guard.
use bevy_app::{App, AppExit, Last, Plugin, PreUpdate};
use bevy_ecs::prelude::*;
use std::{
    marker::PhantomData,
    sync::{Arc, Mutex},
};
use synctick::{CommandSender, SessionControl, SessionHandle, SessionResult, SessionStatus};

/// Typed command submission for the currently attached session.
#[derive(Resource)]
pub struct SessionCommands<C: Send + Sync + 'static>(pub CommandSender<C>);
/// Latest coalesced session status, refreshed during `PreUpdate`.
#[derive(Resource, Clone)]
pub struct SessionState(pub Arc<SessionStatus>);
/// Cancellation control for the currently attached session.
#[derive(Resource, Clone)]
pub struct SessionCancellation(pub SessionControl);

struct SessionSlot<C> {
    handle: Option<SessionHandle<C>>,
    result: Arc<SessionResult>,
    generation: u64,
}
impl<C> SessionSlot<C> {
    fn shutdown(&mut self) -> Arc<SessionResult> {
        if let Some(handle) = &mut self.handle {
            self.result = handle.shutdown();
        }
        self.result.clone()
    }
}
struct OwnedSession<C> {
    slot: Mutex<SessionSlot<C>>,
}
impl<C> Drop for OwnedSession<C> {
    fn drop(&mut self) {
        self.slot
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .shutdown();
    }
}

/// Attach sessions after a menu selection without replacing the Bevy App.
///
/// Status, command and cancellation resources synchronize in the next `PreUpdate`.
/// Explicitly detach even a completed session before attaching another.
#[derive(Resource)]
pub struct SessionController<C: Send + Sync + 'static> {
    worker: Arc<OwnedSession<C>>,
}
impl<C: Send + Sync + 'static> SessionController<C> {
    /// Attach an already constructed session.
    /// # Errors
    /// Rejects an occupied slot without replacing it. The rejected incoming
    /// handle is dropped, cancelling and joining that incoming session.
    pub fn attach(&self, handle: SessionHandle<C>) -> SessionResult {
        let mut slot = self
            .worker
            .slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if slot.handle.is_some() {
            return Err(synctick::SessionError::Configuration(
                "session slot is occupied; detach it first".into(),
            ));
        }
        slot.handle = Some(handle);
        slot.result = Arc::new(Ok(()));
        slot.generation = slot.generation.wrapping_add(1);
        drop(slot);
        Ok(())
    }

    /// Cancel and join, release the slot, and retain the terminal result.
    /// Repeated calls return the same result until another session is attached.
    pub fn detach(&self) -> Arc<SessionResult> {
        let mut slot = self
            .worker
            .slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let result = slot.shutdown();
        if slot.handle.take().is_some() {
            slot.generation = slot.generation.wrapping_add(1);
        }
        result
    }
}

/// Retain this guard outside `App::run`: Bevy consumes/replaces the original App.
/// Dropping the guard cancels and joins even if the runner retains its World.
pub struct SessionGuard<C> {
    worker: Arc<OwnedSession<C>>,
}
impl<C> SessionGuard<C> {
    /// Cancel, join and return the retained typed result, including flush failures.
    /// Call after `App::run` when the executable needs to report a failure exit.
    pub fn shutdown(&self) -> Arc<SessionResult> {
        self.worker
            .slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .shutdown()
    }
}
impl<C> Drop for SessionGuard<C> {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Presentation-side session resources and lifecycle systems.
///
/// Keep the returned [`SessionGuard`] outside `App::run` to join the worker
/// after runner teardown and inspect its terminal result.
pub struct SessionPlugin<C: Send + Sync + 'static> {
    worker: Arc<OwnedSession<C>>,
    marker: PhantomData<fn(C)>,
}
impl<C: Send + Sync + 'static> SessionPlugin<C> {
    /// Install an already constructed session and return its teardown guard.
    #[must_use]
    pub fn new(handle: SessionHandle<C>) -> (Self, SessionGuard<C>) {
        Self::from_handle(Some(handle))
    }

    /// Install session resources without starting a worker.
    /// Use `SessionController<C>` to attach after GUI setup. Idle status is
    /// Stopped; command/cancellation resources exist only while attached.
    #[must_use]
    pub fn idle() -> (Self, SessionGuard<C>) {
        Self::from_handle(None)
    }

    fn from_handle(handle: Option<SessionHandle<C>>) -> (Self, SessionGuard<C>) {
        let worker = Arc::new(OwnedSession {
            slot: Mutex::new(SessionSlot {
                handle,
                result: Arc::new(Ok(())),
                generation: 0,
            }),
        });
        (
            Self {
                worker: worker.clone(),
                marker: PhantomData,
            },
            SessionGuard { worker },
        )
    }
}
impl<C: Send + Sync + 'static> Plugin for SessionPlugin<C> {
    fn build(&self, app: &mut App) {
        let slot = self
            .worker
            .slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let status = slot.handle.as_ref().map_or_else(
            || Arc::new(SessionStatus::Stopped),
            |handle| {
                app.insert_resource(SessionCommands(handle.commands()));
                app.insert_resource(SessionCancellation(handle.control()));
                handle.status()
            },
        );
        app.insert_resource(SessionState(status));
        drop(slot);
        app.insert_resource(SessionController {
            worker: self.worker.clone(),
        });
        app.add_systems(PreUpdate, observe::<C>);
        app.add_systems(Last, cancel_on_exit);
    }
}
fn observe<C: Send + Sync + 'static>(
    controller: Res<SessionController<C>>,
    mut state: ResMut<SessionState>,
    mut commands: Commands,
    mut generation: Local<Option<u64>>,
) {
    let mut slot = controller
        .worker
        .slot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(handle) = &mut slot.handle {
        handle.poll();
        state.0 = handle.status();
    } else {
        state.0 = Arc::new(match &*slot.result {
            Ok(()) => SessionStatus::Stopped,
            Err(error) => SessionStatus::Failed(error.to_string()),
        });
    }
    if *generation != Some(slot.generation) {
        if let Some(handle) = &slot.handle {
            commands.insert_resource(SessionCommands(handle.commands()));
            commands.insert_resource(SessionCancellation(handle.control()));
        } else {
            commands.remove_resource::<SessionCommands<C>>();
            commands.remove_resource::<SessionCancellation>();
        }
        *generation = Some(slot.generation);
    }
}
fn cancel_on_exit(mut exit: MessageReader<AppExit>, control: Option<Res<SessionCancellation>>) {
    if exit.read().next().is_some()
        && let Some(control) = control
    {
        control.0.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BevyGame, BevySimulation, GameAdapter, test_game::Example};
    use std::{
        sync::atomic::Ordering,
        thread,
        time::{Duration, Instant},
    };
    use synctick::{Game, ServerConfig, SessionStatus};
    #[test]
    fn guard_joins_even_when_the_app_and_plugin_still_own_the_worker() {
        let game = Example::default();
        let observed = game.published.clone();
        let mut cfg = ServerConfig::new(());
        cfg.port = 0;
        let handle = synctick::host(GameAdapter(game), cfg).unwrap();
        let control = handle.control();
        let (plugin, guard) = SessionPlugin::new(handle);
        let mut app = App::new();
        app.add_plugins(plugin);
        let deadline = Instant::now() + Duration::from_secs(5);
        while observed.load(Ordering::Acquire) == 0 {
            assert!(Instant::now() < deadline);
            app.update();
            thread::sleep(Duration::from_millis(1));
        }
        drop(guard);
        assert_eq!(*control.status(), SessionStatus::Stopped);
        app.update();
        assert_eq!(
            *app.world().resource::<SessionState>().0,
            SessionStatus::Stopped
        );
        assert!(matches!(
            app.world().resource::<SessionCommands<()>>().0.submit(&()),
            Err(synctick::SubmitError::Stopped)
        ));
    }
    #[test]
    fn guard_exposes_typed_worker_failure_after_plugin_takes_handle() {
        struct Fails;
        impl Game for Fails {
            type Command = ();
            type Initialization = ();
            type Simulation = BevySimulation<Example>;
            const ID: [u8; 16] = Example::ID;
            const VERSION: u32 = 1;
            fn create(&self, (): (), _: Duration) -> SessionResult<Self::Simulation> {
                Err(synctick::SessionError::Simulation(
                    "initialization failed".into(),
                ))
            }
        }
        let mut cfg = ServerConfig::new(());
        cfg.port = 0;
        let (plugin, guard) = SessionPlugin::new(synctick::host(Fails, cfg).unwrap());
        let mut app = App::new();
        app.add_plugins(plugin);
        assert!(
            matches!(&*guard.shutdown(), Err(synctick::SessionError::Simulation(message)) if message == "initialization failed")
        );
        let result = app.world().resource::<SessionController<()>>().detach();
        assert!(Arc::ptr_eq(&result, &guard.shutdown()));
        app.update();
        assert!(
            matches!(&*app.world().resource::<SessionState>().0, SessionStatus::Failed(message) if message.contains("initialization failed"))
        );
    }
    #[test]
    fn idle_slot_attaches_rejects_replacement_and_can_be_reused() {
        let (plugin, guard) = SessionPlugin::<()>::idle();
        let mut app = App::new();
        app.add_plugins(plugin);
        app.update();
        assert!(!app.world().contains_resource::<SessionCommands<()>>());
        let make = || {
            let mut config = ServerConfig::new(());
            config.port = 0;
            synctick::host(GameAdapter(Example::default()), config).unwrap()
        };
        let first = make();
        let control = first.control();
        app.world()
            .resource::<SessionController<()>>()
            .attach(first)
            .unwrap();
        app.update();
        assert!(app.world().contains_resource::<SessionCommands<()>>());
        let second = make();
        let rejected = second.control();
        assert!(
            app.world()
                .resource::<SessionController<()>>()
                .attach(second)
                .is_err()
        );
        assert!(rejected.is_cancelled());
        assert!(!control.is_cancelled());
        let result = app.world().resource::<SessionController<()>>().detach();
        assert!(result.is_ok());
        assert!(control.is_cancelled());
        assert!(Arc::ptr_eq(
            &result,
            &app.world().resource::<SessionController<()>>().detach()
        ));
        app.update();
        assert!(!app.world().contains_resource::<SessionCommands<()>>());
        assert!(!app.world().contains_resource::<SessionCancellation>());
        let next = make();
        let next_control = next.control();
        app.world()
            .resource::<SessionController<()>>()
            .attach(next)
            .unwrap();
        app.update();
        drop(guard);
        assert!(next_control.is_cancelled());
    }
}
