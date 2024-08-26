use std::sync::Arc;
use anyhow::bail;
use bevy::{
    log::{Level, LogPlugin}, 
    prelude::*, 
    tasks::futures_lite::future
};
use bytes::Bytes;
use tokio::{
    net::UdpSocket as TokioUdpSocket, 
    runtime::{self, Runtime},
    sync::mpsc::{
        unbounded_channel as tokio_channel, 
        UnboundedSender as TokioTx,
        UnboundedReceiver as TokioRx
    }, 
    task::JoinHandle
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

pub struct DtlsClientConfig {
    pub server_addr: &'static str,
    pub client_addr: &'static str,
    pub server_name: &'static str
}

#[derive(Resource)]
pub struct DtlsClient {
    runtime: Arc<Runtime>,
    conn: Option<Arc<dyn Conn + Sync + Send>>,
    send_tx: Option<TokioTx<Bytes>>,
    send_handle: Option<JoinHandle<anyhow::Result<()>>>
}

impl DtlsClient {
    pub fn new() -> anyhow::Result<Self> {
        let rt = runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?; 

        Ok(Self{
            runtime: Arc::new(rt),
            conn: None,
            send_tx: None,
            send_handle: None
        })
    }

    pub fn build_dtls(&mut self, config: DtlsClientConfig) 
    -> anyhow::Result<()> {
        let conn = future::block_on(self.runtime.spawn(
            Self::build_dtls_conn(config)
        ))??;
        self.conn = Some(conn);
        Ok(())
    }

    async fn build_dtls_conn(config: DtlsClientConfig) 
    -> anyhow::Result<Arc<impl Conn + Sync + Send>> {
        let socket = TokioUdpSocket::bind(config.client_addr).await?;
        socket.connect(config.server_addr).await?;
        info!("connecting to {}", config.server_addr);

        let certificate = Certificate::generate_self_signed(vec![
            config.server_name.to_string()
        ])?;
        let dtls_conn = DTLSConn::new(
            Arc::new(socket), 
            Config{
                certificates: vec![certificate],
                insecure_skip_verify: true,
                extended_master_secret: ExtendedMasterSecretType::Require,
                server_name: config.server_name.to_string(),
                ..default()
            }, 
            true, 
            None
        ).await?;

        info!("connected");
        Ok(Arc::new(dtls_conn))
    }

    pub fn start_send_loop(&mut self) -> anyhow::Result<()> {
        if self.send_tx.is_some() {
            bail!("send tx is already some");
        }

        let (send_tx, send_rx) = tokio_channel::<Bytes>();
        self.send_tx = Some(send_tx);
        let c = match self.conn {
            Some(ref c) => c.clone(),
            None => bail!("conn is none")
        };
        let handle = self.runtime.spawn(Self::send_loop(send_rx, c));
        self.send_handle = Some(handle);

        Ok(())
    }

    async fn send_loop(
        mut send_rx: TokioRx<Bytes>,
        conn: Arc<dyn Conn + Sync + Send>
    )-> anyhow::Result<()> {
        loop {
            let Some(msg) = send_rx.recv().await else {
                break;
            };

            conn.send(&msg).await?;
        }

        Ok(())
    }

    pub fn send(&self, message: Bytes) -> anyhow::Result<()> {
        let Some(ref tx) = self.send_tx else {
            bail!("send tx is None");
        };

        tx.send(message)?;
        Ok(())
    }
}

fn send_helooon_system(dtls_client: Res<DtlsClient>) {
    let msg = Bytes::from_static(b"helloooooon!!");
    if let Err(e) = dtls_client.send(msg) {
        panic!("{e}");
    }
}

pub struct DtlsClientPlugin {
    pub server_addr: &'static str,
    pub client_addr: &'static str,
    pub server_name: &'static str
}

impl Plugin for DtlsClientPlugin {
    fn build(&self, app: &mut App) {
        let mut dtls_client = match DtlsClient::new() {
            Ok(c) => c,
            Err(e) => panic!("{e}")
        };

        if let Err(e) = dtls_client.build_dtls(DtlsClientConfig{ 
            server_addr: self.server_addr, 
            client_addr: self.client_addr, 
            server_name: self.server_name 
        }) {
            panic!("{e}")
        }
        if let Err(e) = dtls_client.start_send_loop() {
            panic!("{e}");
        } 

        app.insert_resource(dtls_client)
        .add_systems(Update, send_helooon_system);
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
    .run();
}
