use bevy::{
    log::{Level, LogPlugin}, 
    prelude::*
};
use bytes::Bytes;
use bevy_dtls_dev::client::{
    dtls_client::*, 
    plugin::DtlsClientPlugin
};

#[derive(Component)]
pub struct RollingBox;

fn setup_graphics(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>
) {
    commands.spawn(DirectionalLightBundle{
        transform: Transform{
            translation: Vec3::new(0.0, 10.0, 0.0),
            rotation: Quat::from_rotation_x(-std::f32::consts::PI / 2.0),
            ..default()
        },
        ..default()
    });

    commands.spawn(Camera3dBundle{
        transform: Transform::from_translation(Vec3::new(0.0, 10.0, 10.0))
        .looking_at(Vec3::ZERO, Vec3::Y),
        ..default()
    });

    commands.spawn(PbrBundle{
        mesh: meshes.add(Mesh::from(Cuboid::from_size(Vec3::new(3.0, 3.0, 3.0)))),
        material: materials.add(Color::from(bevy::color::palettes::basic::MAROON)),
        ..default()
    })
    .insert(RollingBox);
}

fn graphics_system(
    mut query: Query<&mut Transform, With<RollingBox>>,
    time: Res<Time>
) {
    for mut transform in query.iter_mut() {
        transform.rotate_y(std::f32::consts::PI * 0.5 * time.delta_seconds());
    }
}

pub struct ClientGraphicsPlugin;

impl Plugin for ClientGraphicsPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup_graphics)
        .add_systems(Update, graphics_system);
    }
}

fn send_helooon_system(dtls_client: Res<DtlsClient>) {
    let msg = Bytes::from_static(b"helloooooon!!");
    if let Err(e) = dtls_client.send(msg) {
        panic!("{e}");
    }
}


fn main() {
    App::new()
    .add_plugins((
        DefaultPlugins.set(LogPlugin{
            level: Level::INFO,
            ..default()
        }),
        ClientGraphicsPlugin,
        DtlsClientPlugin{
            server_addr: "127.0.0.1:4443",
            client_addr: "127.0.0.1:0",
            server_name: "localhost"
        }
    ))
    .add_systems(Update, send_helooon_system)
    .run();
}
