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
use bytes::{Bytes, BytesMut};
use crossbeam::channel::{
    unbounded as crossbeam_channel, Receiver as CrossbeamRx, Sender as CrossbeamTx, TryRecvError
};

const BUF_SIZE: usize = 1024;

pub struct DtlsServerConfig {
    pub listen_addr: &'static str,
    pub server_names: Vec<String>
}

pub struct DtlsConn {
    conn: Arc<dyn Conn + Sync + Send>,
    recv_rx: Option<CrossbeamRx<Bytes>>, 
    recv_handle: Option<JoinHandle<anyhow::Result<()>>>
}

pub struct AcceptedConn(usize);

#[derive(Resource)]
pub struct DtlsServer {
    runtime: Arc<Runtime>,
    listener: Option<Arc<dyn Listener + Sync + Send>>,
    conns: Arc<StdRwLock<Vec<Option<DtlsConn>>>>,
    accept_handle: Option<JoinHandle<anyhow::Result<()>>>,
    accept_rx: Option<CrossbeamRx<AcceptedConn>>,
    recv_buf_size: usize
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
            accept_handle: None,
            accept_rx: None,
            recv_buf_size: BUF_SIZE

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
        let listener = match self.listener {
            Some(ref l) => l.clone(),
            None => bail!("listener is none")
        };
        let conns = self.conns.clone();
        let (accept_tx, accept_rx) = crossbeam_channel::<AcceptedConn>();
        let handle = self.runtime.spawn(
            Self::accept_loop(listener, conns, accept_tx)            
        );
        
        self.accept_handle = Some(handle);
        self.accept_rx = Some(accept_rx);
        Ok(())
    }

    async fn accept_loop(
        listener: Arc<dyn Listener + Sync + Send>,
        conns: Arc<StdRwLock<Vec<Option<DtlsConn>>>>,
        accepted_tx: CrossbeamTx<AcceptedConn>
    ) -> anyhow::Result<()> {
        loop {
            let (conn, addr) = listener.accept().await?;
            info!("conn from {addr} accepted");

            let mut w = conns.write().unwrap();
            let idx = w.len();
            w.push(Some(DtlsConn{
                conn,
                recv_rx: None,
                recv_handle: None,
            }));
            accepted_tx.send(AcceptedConn(idx))?;
        }
    }

    pub fn start_recv_loop(
        &self, 
        accepted_conn: AcceptedConn,
        recv_chan: (CrossbeamTx<Bytes>, CrossbeamRx<Bytes>)
    )-> anyhow::Result<()> {
        let idx = accepted_conn.0;
        let (recv_tx, recv_rx) = recv_chan;
        let mut w = self.conns.write().unwrap();
        let dtls_conn = match w[idx] {
            None => bail!("dtls conn is None"),
            Some(ref mut c) => c
        };
        dtls_conn.recv_rx = Some(recv_rx);
        let conn = dtls_conn.conn.clone();
        let handle = self.runtime.spawn(Self::recv_loop(
            conn,
            recv_tx,
            self.recv_buf_size
        ));
        dtls_conn.recv_handle = Some(handle);
        
        Ok(())
    }

    async fn recv_loop(
        conn: Arc<dyn Conn + Sync + Send>,
        recv_tx: CrossbeamTx<Bytes>, 
        buf_size: usize
    ) -> anyhow::Result<()> {
        let mut buf = BytesMut::zeroed(buf_size);

        loop {
            let (n, addr) = conn.recv_from(&mut buf).await?;
            let recved = buf.split_to(n)
            .freeze();
            recv_tx.send(recved)?;

            buf.resize(buf_size, 0);
            info!("received {n}bytes from {addr}");
        }
    }
} 

fn accept_system(dtls_server: Res<DtlsServer>) {
    let accept_rx = match dtls_server.accept_rx {
        Some(ref rx) => rx,
        None => panic!("accept rx is none")
    };
    let accepted = match accept_rx.try_recv() {
        Ok(a) => a,
        Err(TryRecvError::Empty) => return,
        Err(e) => panic!("{e}") 
    };

    let recv_chan = crossbeam_channel::<Bytes>();
    if let Err(e) = dtls_server.start_recv_loop(accepted, recv_chan) {
        panic!("{e}");
    }
}

fn recv_system(dtls_server: Res<DtlsServer>) {
    let r = dtls_server.conns.read().unwrap();
    for (idx, op_conn) in r.iter()
    .enumerate() {
        let Some(ref conn) = op_conn else {
            continue;
        };

        let Some(ref recv_rx) = conn.recv_rx else {
            continue;
        };

        let raw = match recv_rx.try_recv() {
            Ok(r) => r,
            Err(TryRecvError::Empty) => continue,
            Err(e) => panic!("{e}")
        };

        let msg = String::from_utf8(raw.to_vec()).unwrap();
        info!("message from conn: {idx}, {msg}")
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

        app.insert_resource(dtls_server)
        .add_systems(Update, (
            accept_system,
            recv_system
        ));
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

