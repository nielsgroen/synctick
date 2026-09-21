//! Menu state, text editing, and session attachment.
use bevy::{
    input::{
        ButtonState,
        keyboard::{Key, KeyboardFocusLost, KeyboardInput},
    },
    prelude::*,
};
use std::{net::SocketAddr, path::PathBuf};
use synctick::{ClientConfig, ServerConfig, SessionStatus};
use synctick_bevy::{GameAdapter, SessionCommands, SessionController, SessionState};
use synctick_example_power_garden::{Initialization, PowerGarden, Rotate, Snapshots};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Solo,
    Host,
    Join,
}

#[derive(Resource)]
pub struct Menu {
    pub mode: Mode,
    pub fields: [String; 4],
    pub focus: Option<usize>,
    pub running: bool,
    pub rebuild: bool,
    pub error: String,
    pub snapshots: Snapshots,
}
impl Default for Menu {
    fn default() -> Self {
        Self {
            mode: Mode::Solo,
            fields: [
                "5000".into(),
                "127.0.0.1:5000".into(),
                String::new(),
                String::new(),
            ],
            focus: None,
            running: false,
            rebuild: true,
            error: String::new(),
            snapshots: Snapshots::default(),
        }
    }
}
#[derive(Component, Clone, Copy)]
pub enum Action {
    Mode(Mode),
    Field(usize),
    Start,
    Back,
    Rotate(u32),
}
#[derive(Component)]
pub struct FieldText(pub usize);
#[derive(Component)]
pub struct ErrorText;

fn optional_path(text: &str) -> Option<PathBuf> {
    (!text.trim().is_empty()).then(|| PathBuf::from(text.trim()))
}
impl Menu {
    fn start(&mut self, controller: &SessionController<Rotate>) -> Result<(), String> {
        let game = PowerGarden::default();
        let snapshots = game.snapshots();
        let handle = match self.mode {
            Mode::Join => {
                let address: SocketAddr = self.fields[1]
                    .trim()
                    .parse()
                    .map_err(|_| "Enter an address such as 127.0.0.1:5000")?;
                synctick::connect(GameAdapter(game), ClientConfig::new(1, address))
            }
            Mode::Solo | Mode::Host => {
                let mut config = ServerConfig::new(Initialization::default());
                config.port = if self.mode == Mode::Solo {
                    0
                } else {
                    let port: u16 = self.fields[0]
                        .trim()
                        .parse()
                        .map_err(|_| "Port must be an integer from 1 to 65535")?;
                    if port == 0 {
                        return Err("Host port must be between 1 and 65535".into());
                    }
                    port
                };
                config.expected_clients = u32::from(self.mode == Mode::Host);
                config.load_path = optional_path(&self.fields[2]);
                config.record_path = optional_path(&self.fields[3]);
                synctick::host(GameAdapter(game), config)
            }
        }
        .map_err(|error| error.to_string())?;
        controller
            .attach(handle)
            .map_err(|error| error.to_string())?;
        self.snapshots = snapshots;
        self.running = true;
        self.focus = None;
        self.rebuild = true;
        self.error.clear();
        Ok(())
    }
}
pub fn actions(
    interactions: Query<(&Interaction, &Action), Changed<Interaction>>,
    mut menu: ResMut<Menu>,
    controller: Res<SessionController<Rotate>>,
    sender: Option<Res<SessionCommands<Rotate>>>,
    state: Res<SessionState>,
) {
    for (interaction, action) in &interactions {
        if *interaction != Interaction::Pressed {
            continue;
        }
        match *action {
            Action::Mode(mode) => {
                menu.mode = mode;
                menu.focus = None;
                menu.rebuild = true;
            }
            Action::Field(index) => {
                menu.focus = Some(index);
            }
            Action::Start => {
                if !menu.running
                    && let Err(error) = menu.start(&controller)
                {
                    menu.error = error;
                }
            }
            Action::Back => {
                let result = controller.detach();
                menu.error = result
                    .as_ref()
                    .as_ref()
                    .err()
                    .map(ToString::to_string)
                    .unwrap_or_default();
                menu.running = false;
                menu.rebuild = true;
                menu.snapshots = Snapshots::default();
            }
            Action::Rotate(cell) => {
                if menu.running
                    && matches!(&*state.0, SessionStatus::Live)
                    && let Some(sender) = &sender
                    && let Err(error) = sender.0.submit(&Rotate { cell })
                {
                    menu.error = error.to_string();
                }
            }
        }
    }
}

pub fn edit_fields(
    mut input: MessageReader<KeyboardInput>,
    mut keys: Local<ButtonInput<KeyCode>>,
    mut focus_lost: MessageReader<KeyboardFocusLost>,
    mut menu: ResMut<Menu>,
) {
    for event in input.read() {
        match event.state {
            ButtonState::Pressed => keys.press(event.key_code),
            ButtonState::Released => keys.release(event.key_code),
        }
        if !event.state.eq(&ButtonState::Pressed) || menu.running {
            continue;
        }
        if event.logical_key == Key::Tab {
            let fields: &[usize] = match menu.mode {
                Mode::Solo => &[2, 3],
                Mode::Host => &[0, 2, 3],
                Mode::Join => &[1],
            };
            let next = menu
                .focus
                .and_then(|current| fields.iter().position(|index| *index == current))
                .map_or(0, |index| (index + 1) % fields.len());
            menu.focus = Some(fields[next]);
            continue;
        }
        let Some(index) = menu.focus else { continue };
        let control = keys.pressed(KeyCode::ControlLeft)
            || keys.pressed(KeyCode::ControlRight)
            || keys.pressed(KeyCode::SuperLeft)
            || keys.pressed(KeyCode::SuperRight);
        match &event.logical_key {
            Key::Backspace => {
                menu.fields[index].pop();
            }
            Key::Escape => menu.focus = None,
            Key::Character(text) if control && text.eq_ignore_ascii_case("a") => {
                menu.fields[index].clear();
            }
            Key::Character(_) if !control => {
                if let Some(text) = &event.text {
                    for ch in text.chars().filter(|ch| !ch.is_control()) {
                        if menu.fields[index].len() < 1024 {
                            menu.fields[index].push(ch);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    keys.clear();
    if focus_lost.read().next().is_some() {
        keys.reset_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shortcut_modifiers_follow_event_order_even_with_same_frame_release() {
        let mut app = App::new();
        app.add_message::<KeyboardInput>();
        app.add_message::<KeyboardFocusLost>();
        let mut menu = Menu {
            focus: Some(3),
            ..Menu::default()
        };
        menu.fields[3] = "old.save".into();
        app.insert_resource(menu);
        app.add_systems(Update, edit_fields);
        for (key_code, logical_key, state, text) in [
            (
                KeyCode::ControlLeft,
                Key::Control,
                ButtonState::Pressed,
                None,
            ),
            (
                KeyCode::KeyA,
                Key::Character("a".into()),
                ButtonState::Pressed,
                Some("a".into()),
            ),
            (
                KeyCode::ControlLeft,
                Key::Control,
                ButtonState::Released,
                None,
            ),
            (
                KeyCode::KeyB,
                Key::Character("b".into()),
                ButtonState::Pressed,
                Some("b".into()),
            ),
        ] {
            app.world_mut().write_message(KeyboardInput {
                key_code,
                logical_key,
                state,
                text,
                repeat: false,
                window: Entity::PLACEHOLDER,
            });
        }
        app.update();
        assert_eq!(app.world().resource::<Menu>().fields[3], "b");
    }
}
