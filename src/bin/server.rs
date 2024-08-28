use std::time::Duration;
use bevy::{
    app::ScheduleRunnerPlugin, 
    log::{Level, LogPlugin}, 
    prelude::*
};
use bevy_dtls_dev::server::{
    dtls_server::DtlsServer,
    plugin::DtlsServerPlugin
};

#[derive(Resource)]
struct ServerHellooonCounter(pub usize);

fn recv_hellooon_system(
    mut dtls_server: ResMut<DtlsServer>,
    mut counter: ResMut<ServerHellooonCounter>
) {
    loop {
        let Some((idx, raw)) = dtls_server.recv() else {
            return;
        };

        let msg = String::from_utf8(raw.to_vec()).unwrap();
        info!("message from conn: {}, {msg}", idx.index());
        counter.0 += 1;

        if counter.0 >= 10 {
            dtls_server.close_conn(0);
        }
    }
}

fn health_check_system(mut dtls_server: ResMut<DtlsServer>) {
    let health = dtls_server.health_check();
    if let Some(Err(e)) = health.listener {
        panic!("{e}");
    }
    if let Some((idx, Err(e))) = health.sender.get(0) {
        warn!("conn index {idx}: {e}");
    }
    if let Some((idx, Err(e))) = health.recver.get(0) {
        warn!("conn index {idx}: {e}");
    }
}

fn main() {
    App::new()
    .add_plugins((
        MinimalPlugins.set(ScheduleRunnerPlugin::run_loop(
            Duration::from_secs_f32(1.0 / 30.0)
        )),
        LogPlugin{
            level: Level::DEBUG,
            ..default()
        },
        DtlsServerPlugin{
            listen_addr: "127.0.0.1:4443",
            server_name: "localhost",
            buf_size: 512
        }
    ))
    .insert_resource(ServerHellooonCounter(0))
    .add_systems(Update, (
        recv_hellooon_system,
        health_check_system
    ))
    .run();
}
