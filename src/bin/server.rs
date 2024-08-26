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
use crossbeam::channel::TryRecvError;

fn recv_system(dtls_server: Res<DtlsServer>) {
    loop {
        let (idx, raw) = match dtls_server.try_recv() {
            Ok(ir) => ir,
            Err(TryRecvError::Empty) => break,
            Err(e) => panic!("{e}")
        };

        let msg = String::from_utf8(raw.to_vec()).unwrap();
        info!("message from conn: {}, {msg}", idx.index());
    }
}

fn main() {
    App::new()
    .add_plugins((
        MinimalPlugins.set(ScheduleRunnerPlugin::run_loop(
            Duration::from_secs_f32(1.0 / 30.0)
        )),
        LogPlugin{
            level: Level::INFO,
            ..default()
        },
        DtlsServerPlugin{
            listen_addr: "127.0.0.1:4443",
            server_name: "localhost",
            buf_size: 512
        }
    ))
    .add_systems(Update, recv_system)
    .run();
}

