//! Bevy UI rendering. Animation and wall time never enter the worker simulation.
use crate::menu::{self, Action, ErrorText, FieldText, Menu, Mode};
use bevy::prelude::*;
use synctick::{ParticipantId, SessionStatus};
use synctick_bevy::{SessionPlugin, SessionState};
use synctick_example_power_garden::{Kind, Rotate, Snapshot};

const INK: Color = Color::srgb(0.035, 0.065, 0.085);
const PANEL: Color = Color::srgb(0.065, 0.11, 0.14);
const DIM: Color = Color::srgb(0.28, 0.39, 0.43);
const LIGHT: Color = Color::srgb(0.84, 0.95, 0.90);
const MINT: Color = Color::srgb(0.36, 0.96, 0.68);
const GOLD: Color = Color::srgb(1.0, 0.78, 0.35);

#[derive(Component)]
struct Root;
#[derive(Component)]
struct Status;
#[derive(Component)]
struct TileView(usize);
#[derive(Component)]
struct Port {
    cell: usize,
    bit: u8,
}
#[derive(Component)]
struct Flower {
    cell: usize,
}
#[derive(Component)]
struct Completion;
#[derive(Resource, Default)]
struct Animation {
    tick: Option<u64>,
    flashes: [Option<(f64, bool)>; 25],
}

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let (plugin, guard) = SessionPlugin::<Rotate>::idle();
    let mut app = App::new();
    app.add_plugins(DefaultPlugins.set(WindowPlugin {
        primary_window: Some(Window {
            title: "Power Garden".into(),
            resolution: (1000, 820).into(),
            ..default()
        }),
        ..default()
    }))
    .insert_resource(ClearColor(INK))
    .init_resource::<Menu>()
    .init_resource::<Animation>()
    .add_plugins(plugin)
    .add_systems(Startup, |mut commands: Commands| {
        commands.spawn(Camera2d);
    })
    .add_systems(
        Update,
        (
            menu::edit_fields,
            menu::actions,
            rebuild,
            animate,
            update_fields,
            button_colors,
        )
            .chain(),
    );
    app.run();
    if let Err(error) = &*guard.shutdown() {
        return Err(error.to_string().into());
    }
    Ok(())
}

fn label(parent: &mut ChildSpawnerCommands, value: &str, size: f32, color: Color) {
    parent.spawn((
        Text::new(value),
        TextFont {
            font_size: size,
            ..default()
        },
        TextColor(color),
    ));
}
fn button(parent: &mut ChildSpawnerCommands, caption: &str, action: Action) {
    parent
        .spawn((
            Button,
            action,
            Node {
                padding: UiRect::axes(px(22), px(12)),
                border: UiRect::all(px(1)),
                border_radius: BorderRadius::all(px(10)),
                ..default()
            },
            BackgroundColor(PANEL),
            BorderColor::all(DIM),
        ))
        .with_children(|p| label(p, caption, 18.0, LIGHT));
}
fn field(parent: &mut ChildSpawnerCommands, caption: &str, index: usize) {
    label(parent, caption, 15.0, Color::srgb(0.55, 0.68, 0.70));
    parent
        .spawn((
            Button,
            Action::Field(index),
            Node {
                width: percent(100),
                min_height: px(46),
                padding: UiRect::all(px(12)),
                border: UiRect::all(px(1)),
                border_radius: BorderRadius::all(px(10)),
                overflow: Overflow::clip(),
                ..default()
            },
            BackgroundColor(INK),
            BorderColor::all(DIM),
        ))
        .with_children(|p| {
            p.spawn((
                Text::new(""),
                TextFont {
                    font_size: 18.0,
                    ..default()
                },
                TextColor(LIGHT),
                FieldText(index),
            ));
        });
}
fn rebuild(
    mut commands: Commands,
    roots: Query<Entity, With<Root>>,
    mut menu: ResMut<Menu>,
    mut animation: ResMut<Animation>,
) {
    if !menu.rebuild {
        return;
    }
    menu.rebuild = false;
    *animation = Animation::default();
    for root in &roots {
        commands.entity(root).despawn();
    }
    commands
        .spawn((
            Root,
            Node {
                width: percent(100),
                height: percent(100),
                align_items: AlignItems::Center,
                flex_direction: FlexDirection::Column,
                padding: UiRect::all(px(28)),
                row_gap: px(12),
                overflow: Overflow::scroll_y(),
                ..default()
            },
            BackgroundColor(INK),
        ))
        .with_children(|root| {
            label(root, "P O W E R   G A R D E N", 32.0, MINT);
            label(root, "Connect the light. Grow together.", 18.0, LIGHT);
            if menu.running {
                game_layout(root);
            } else {
                menu_layout(root, menu.mode);
            }
            root.spawn((
                Text::new(""),
                TextFont {
                    font_size: 16.0,
                    ..default()
                },
                TextColor(Color::srgb(1.0, 0.48, 0.42)),
                Node {
                    max_width: px(740),
                    ..default()
                },
                ErrorText,
            ));
        });
}

fn menu_layout(root: &mut ChildSpawnerCommands, mode: Mode) {
    root.spawn(Node {
        column_gap: px(12),
        margin: UiRect::vertical(px(14)),
        ..default()
    })
    .with_children(|p| {
        button(p, "Solo", Action::Mode(Mode::Solo));
        button(p, "Host", Action::Mode(Mode::Host));
        button(p, "Join", Action::Mode(Mode::Join));
    });
    root.spawn((
        Node {
            width: px(490),
            padding: UiRect::all(px(24)),
            flex_direction: FlexDirection::Column,
            row_gap: px(10),
            border_radius: BorderRadius::all(px(16)),
            ..default()
        },
        BackgroundColor(PANEL),
    ))
    .with_children(|p| {
        match mode {
            Mode::Solo => label(p, "A quiet garden, just for you.", 20.0, MINT),
            Mode::Host => {
                label(p, "Grow with one other player.", 20.0, MINT);
                field(p, "Host port", 0);
            }
            Mode::Join => {
                label(p, "Join a friend's garden.", 20.0, MINT);
                field(p, "Server address", 1);
            }
        }
        if mode != Mode::Join {
            field(p, "Load recording  |  optional", 2);
            field(p, "Record to a new file  |  optional", 3);
        }
        label(
            p,
            "Click a field to edit | Tab: next | Ctrl+A: clear",
            13.0,
            DIM,
        );
        button(
            p,
            match mode {
                Mode::Solo => "Open garden",
                Mode::Host => "Host garden",
                Mode::Join => "Join garden",
            },
            Action::Start,
        );
    });
    label(
        root,
        "One shared board. No timer. Every connection counts.",
        16.0,
        LIGHT,
    );
}

fn game_layout(root: &mut ChildSpawnerCommands) {
    root.spawn(Node {
        width: px(660),
        justify_content: JustifyContent::SpaceBetween,
        align_items: AlignItems::Center,
        margin: UiRect::vertical(px(10)),
        ..default()
    })
    .with_children(|p| {
        p.spawn((
            Text::new("Starting session..."),
            TextFont {
                font_size: 18.0,
                ..default()
            },
            TextColor(LIGHT),
            Node {
                max_width: px(450),
                ..default()
            },
            Status,
        ));
        button(p, "Back to menu", Action::Back);
    });
    root.spawn(Node {
        width: px(440),
        height: px(440),
        display: Display::Grid,
        grid_template_columns: RepeatedGridTrack::px(5, 80.0),
        grid_template_rows: RepeatedGridTrack::px(5, 80.0),
        column_gap: px(10),
        row_gap: px(10),
        ..default()
    })
    .with_children(|grid| {
        for cell in 0..25 {
            tile_layout(grid, cell);
        }
    });
    root.spawn((
        Text::new(""),
        TextFont {
            font_size: 25.0,
            ..default()
        },
        TextColor(GOLD),
        Completion,
    ));
    label(
        root,
        "Click a wire to rotate clockwise. Light all four flowers.",
        17.0,
        LIGHT,
    );
    label(
        root,
        "Mint flash: host  |  Gold flash: remote player",
        14.0,
        DIM,
    );
}

fn tile_layout(grid: &mut ChildSpawnerCommands, cell: usize) {
    grid.spawn((
        Button,
        Action::Rotate(u32::try_from(cell).expect("25 cells")),
        TileView(cell),
        Node {
            width: px(80),
            height: px(80),
            border: UiRect::all(px(2)),
            border_radius: BorderRadius::all(px(12)),
            ..default()
        },
        BorderColor::all(PANEL),
        BackgroundColor(PANEL),
    ))
    .with_children(|tile| {
        for (bit, left, top, width, height) in [
            (1, 35.0, 0.0, 6.0, 40.0),
            (2, 38.0, 35.0, 40.0, 6.0),
            (4, 35.0, 38.0, 6.0, 40.0),
            (8, 0.0, 35.0, 40.0, 6.0),
        ] {
            tile.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    left: px(left),
                    top: px(top),
                    width: px(width),
                    height: px(height),
                    ..default()
                },
                BackgroundColor(DIM),
                Port { cell, bit },
            ));
        }
        // Petals and center use geometry, independent of installed symbol fonts.
        for (left, top) in [(31., 31.), (20., 31.), (42., 31.), (31., 20.), (31., 42.)] {
            tile.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    left: px(left),
                    top: px(top),
                    width: px(14),
                    height: px(14),
                    border_radius: BorderRadius::MAX,
                    ..default()
                },
                BackgroundColor(GOLD),
                Flower { cell },
            ));
        }
    });
}
fn update_fields(
    menu: Res<Menu>,
    mut fields: Query<(&FieldText, &mut Text)>,
    mut errors: Query<&mut Text, (With<ErrorText>, Without<FieldText>)>,
) {
    for (field, mut text) in &mut fields {
        let value = &menu.fields[field.0];
        text.0 = if menu.focus == Some(field.0) {
            format!("{value}|")
        } else if value.is_empty() {
            "-".into()
        } else {
            value.clone()
        };
    }
    for mut text in &mut errors {
        text.0.clone_from(&menu.error);
    }
}
fn button_colors(
    menu: Res<Menu>,
    mut buttons: Query<(&Interaction, &Action, &mut BorderColor), Without<TileView>>,
) {
    for (interaction, action, mut border) in &mut buttons {
        let selected = match action {
            Action::Mode(mode) => *mode == menu.mode,
            Action::Field(index) => menu.focus == Some(*index),
            _ => false,
        };
        *border = BorderColor::all(if selected || *interaction != Interaction::None {
            MINT
        } else {
            DIM
        });
    }
}

fn animate(
    menu: Res<Menu>,
    state: Res<SessionState>,
    clock: Res<Time>,
    mut animation: ResMut<Animation>,
    mut tiles: Query<(
        &TileView,
        &Interaction,
        &mut BackgroundColor,
        &mut BorderColor,
    )>,
    mut ports: Query<(&Port, &mut Node, &mut BackgroundColor), Without<TileView>>,
    mut flowers: Query<
        (&Flower, &mut Node, &mut BackgroundColor),
        (Without<Port>, Without<TileView>),
    >,
    mut status: Query<&mut Text, With<Status>>,
    mut completion: Query<&mut Text, (With<Completion>, Without<Status>)>,
) {
    if !menu.running {
        return;
    }
    let snapshot = menu.snapshots.latest();
    let live = matches!(&*state.0, SessionStatus::Live);
    let status_text = describe_status(&state.0, snapshot.as_deref());
    for mut text in &mut status {
        text.0.clone_from(&status_text);
    }
    let Some(snapshot) = snapshot else { return };
    let now = clock.elapsed_secs_f64();
    if animation.tick != Some(snapshot.tick) {
        for effect in &snapshot.effects {
            if let Ok(cell) = usize::try_from(effect.cell)
                && cell < 25
            {
                animation.flashes[cell] = Some((now, effect.participant == ParticipantId::Host));
            }
        }
        animation.tick = Some(snapshot.tick);
    }
    for (view, interaction, mut background, mut border) in &mut tiles {
        let tile = snapshot.board.tiles()[view.0];
        let powered = snapshot.board.powered()[view.0];
        background.0 = if tile.kind == Kind::Empty {
            INK
        } else if powered {
            Color::srgb(0.09, 0.21, 0.18)
        } else {
            PANEL
        };
        let flash = animation.flashes[view.0].filter(|(started, _)| now - started < 0.4);
        *border = BorderColor::all(if let Some((_, host)) = flash {
            if host { MINT } else { GOLD }
        } else if live
            && !snapshot.board.complete()
            && tile.kind == Kind::Wire
            && *interaction != Interaction::None
        {
            LIGHT
        } else if tile.kind == Kind::Source {
            MINT
        } else {
            INK
        });
    }
    for (port, mut node, mut color) in &mut ports {
        let tile = snapshot.board.tiles()[port.cell];
        node.display = if tile.connections() & port.bit != 0 {
            Display::Flex
        } else {
            Display::None
        };
        color.0 = if snapshot.board.powered()[port.cell] {
            MINT
        } else {
            DIM
        };
    }
    for (flower, mut node, mut color) in &mut flowers {
        let tile = snapshot.board.tiles()[flower.cell];
        node.display = if matches!(tile.kind, Kind::Beacon | Kind::Source) {
            Display::Flex
        } else {
            Display::None
        };
        color.0 = if snapshot.board.powered()[flower.cell] {
            if tile.kind == Kind::Source {
                MINT
            } else {
                GOLD
            }
        } else {
            DIM
        };
        // Bloom is presentation-only; simulation stores only connectivity.
        let size = if snapshot.board.powered()[flower.cell] {
            16.0
        } else {
            10.0
        };
        node.width = px(size);
        node.height = px(size);
    }
    for mut text in &mut completion {
        text.0 = if snapshot.board.complete() {
            "Your garden is glowing.".into()
        } else {
            String::new()
        };
    }
}

fn describe_status(status: &SessionStatus, snapshot: Option<&Snapshot>) -> String {
    match status {
        SessionStatus::Live => snapshot.map_or_else(
            || "Preparing garden...".into(),
            |s| {
                format!(
                    "{}/4 flowers  |  {} moves",
                    s.board.connected_beacons(),
                    s.board.moves()
                )
            },
        ),
        SessionStatus::Connecting => "Connecting...".into(),
        SessionStatus::Loading { phase, completed } => format!("Loading {phase:?}: {completed}"),
        SessionStatus::Waiting { ready, expected } => {
            format!("Waiting for player: {ready}/{expected}")
        }
        SessionStatus::AwaitingStart => "Ready - waiting for host".into(),
        SessionStatus::Stopped => "Session stopped".into(),
        SessionStatus::Failed(error) => format!("Session failed: {error}"),
    }
}
