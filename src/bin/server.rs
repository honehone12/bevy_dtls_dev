use std::{
    sync::{Arc, RwLock as StdRwLock}, 
    time::Duration
};
use anyhow::bail;
use bevy::{
    app::ScheduleRunnerPlugin, 
    log::{Level, LogPlugin}, 
    prelude::*, tasks::futures_lite::future, 
};
use tokio::{
    runtime::{self, Runtime}, 
    task::JoinHandle
};
use webrtc_dtls::{
    config::{Config, ExtendedMasterSecretType}, 
    crypto::Certificate, listener,
};
use webrtc_util::conn::{Listener, Conn};

pub struct DtlsServerConfig {
    pub listen_addr: &'static str,
    pub server_names: Vec<String>
}

pub struct DtlsConn {
    conn: Arc<dyn Conn + Sync + Send>,
    message_handle: JoinHandle<anyhow::Result<()>>
}

#[derive(Resource)]
pub struct DtlsServer {
    runtime: Arc<Runtime>,
    listener: Option<Arc<dyn Listener + Sync + Send>>,
    conns: Arc<StdRwLock<Vec<Option<DtlsConn>>>>,
    accept_handle: Option<JoinHandle<anyhow::Result<()>>>
}

impl DtlsServer {
    pub fn new() 
    -> anyhow::Result<Self> {
        let rt = runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

        Ok(Self { 
            runtime: Arc::new(rt),
            listener: None, 
            conns: default(),
            accept_handle: None  
        })
    }

    pub fn build_dtls(&mut self, config: DtlsServerConfig) 
    -> anyhow::Result<()> {
        let listener = future::block_on(
            self.runtime.spawn(Self::build_dtls_listener(config))
        )??;
        self.listener = Some(listener);
        Ok(())
    }

    async fn build_dtls_listener(config: DtlsServerConfig)
    -> anyhow::Result<Arc<dyn Listener + Sync + Send>> {
        let cert = Certificate::generate_self_signed(config.server_names)?;
        let listener = listener::listen(
            config.listen_addr, 
            Config{
                certificates: vec![cert],
                extended_master_secret: ExtendedMasterSecretType::Require,
                ..default()
            }
        ).await?;
        info!("listening at {}", config.listen_addr);
        Ok(Arc::new(listener))
    }

    pub fn start_accept_loop(&mut self)
    -> anyhow::Result<()> {
        let runtime = self.runtime.clone();
        let listener = match self.listener {
            Some(ref l) => l.clone(),
            None => bail!("listener is none")
        };
        let conns = self.conns.clone();
        let handle = self.runtime.spawn(
            Self::accept_loop(runtime, listener, conns)            
        );
        self.accept_handle = Some(handle);

        Ok(())
    }

    async fn accept_loop(
        runtime: Arc<Runtime>,
        listener: Arc<dyn Listener + Sync + Send>,
        conns: Arc<StdRwLock<Vec<Option<DtlsConn>>>>
    ) -> anyhow::Result<()> {
        loop {
            let (conn, addr) = listener.accept().await?;
            info!("conn from {addr} accepted");

            let c = conn.clone();
            let cs = conns.clone();
            let mut w = conns.write().unwrap();
            let idx = w.len();
            let handle = runtime.spawn(Self::message_loop(idx, c, cs));
            w.push(Some(DtlsConn{
                conn,
                message_handle: handle
            }));
        }
    }

    async fn message_loop(
        index: usize,
        conn: Arc<dyn Conn + Sync + Send>,
        conns: Arc<StdRwLock<Vec<Option<DtlsConn>>>>
    ) -> anyhow::Result<()> {
        let mut buff = vec![0u8; 1024];

        loop {
            let (n, addr) = match conn.recv_from(&mut buff).await {
                Ok(na) => na,
                Err(e) => {
                    let mut w = conns.write().unwrap();
                    w[index] = None;
                    bail!(e);
                }
            };

            let msg = String::from_utf8(buff[..n].to_vec())?;
            info!("message from {addr}: {msg}");
        }
    }
} 

pub struct DtlsServerPlugin {
    pub listen_addr: &'static str,
    pub server_name: &'static str
}

impl Plugin for DtlsServerPlugin {
    fn build(&self, app: &mut App) {
        let mut dtls_server = match DtlsServer::new() {
            Ok(s) => s,
            Err(e) => panic!("{e}")
        };

        if let Err(e) = dtls_server.build_dtls(DtlsServerConfig{
            listen_addr: self.listen_addr,
            server_names: vec![self.server_name.to_string()]
        }) {
            panic!("{e}");
        }
        if let Err(e) = dtls_server.start_accept_loop() {
            panic!("{e}");
        }

        app.insert_resource(dtls_server);
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
            server_name: "localhost"
        }
    ))
    .run();
}

