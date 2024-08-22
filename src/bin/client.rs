use std::sync::Arc;
use bevy::{
    log::{Level, LogPlugin}, 
    prelude::*, 
    tasks::futures_lite::future
};
use tokio::{
    net::UdpSocket as TokioUdpSocket, 
    runtime::{self, Runtime}
};
use webrtc_dtls::{
    config::{Config, ExtendedMasterSecretType}, 
    conn::DTLSConn, 
    crypto::Certificate
};
use webrtc_util::Conn;

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

#[derive(Resource)]
pub struct DtlsClient {
    runtime: Runtime,
    conn: Option<Arc<dyn Conn + Sync + Send>>
}

impl DtlsClient {
    pub fn new() -> anyhow::Result<Self> {
        let runtime = runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?; 
        Ok(Self{
            runtime,
            conn: None
        })
    }

    pub fn build_dtls(&mut self) -> anyhow::Result<()> {
        let result = future::block_on(self.runtime.spawn(
            Self::build_dtls_conn()
        ))?;
        self.conn = Some(result?);
        Ok(())
    }

    async fn build_dtls_conn() 
    -> anyhow::Result<Arc<impl Conn + Sync + Send>> {
        let server_addr = "127.0.0.1:4443";
        let client_addr = "127.0.0.1:0";
        let socket = TokioUdpSocket::bind(client_addr).await?;
        socket.connect(server_addr).await?;
        info!("connecting to {server_addr}");

        let server_name = "localhost".to_string();
        let certificate = Certificate::generate_self_signed(vec![
            server_name.clone()
        ])?;
        let config = Config{
            certificates: vec![certificate],
            insecure_skip_verify: true,
            extended_master_secret: ExtendedMasterSecretType::Require,
            server_name,
            ..default()
        };
        let dtls_conn = DTLSConn::new(
            Arc::new(socket), 
            config, 
            true, 
            None
        ).await?;

        info!("connected");
        Ok(Arc::new(dtls_conn))
    }
}

pub struct DtlsClientPlugin;

impl Plugin for DtlsClientPlugin {
    fn build(&self, app: &mut App) {
        let mut dtls_client = match DtlsClient::new() {
            Ok(c) => c,
            Err(e) => panic!("{e}")
        };

        if let Err(e) = dtls_client.build_dtls() {
            panic!("{e}")
        }

        app.insert_resource(dtls_client);
    }
}

#[tokio::main]
async fn main() {
    App::new()
    .add_plugins((
        DefaultPlugins.set(LogPlugin{
            level: Level::INFO,
            ..default()
        }),
        ClientGraphicsPlugin,
        DtlsClientPlugin
    ))
    .run();
}
