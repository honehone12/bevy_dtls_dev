use std::{
    collections::HashMap, 
    sync::{Arc, RwLock as StdRwLock}
};
use anyhow::bail;
use bevy::{
    prelude::*, 
    tasks::futures_lite::future, 
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
    unbounded as crossbeam_channel, 
    Receiver as CrossbeamRx, 
    Sender as CrossbeamTx, TryRecvError
};

pub struct DtlsServerConfig {
    pub listen_addr: &'static str,
    pub server_names: Vec<String>
}

#[derive(Clone, Copy)]
pub struct ConnIndex(usize);

impl ConnIndex {
    pub fn index(&self) -> usize {
        self.0
    }
}

pub struct DtlsConn {
    pub(super) conn: Arc<dyn Conn + Sync + Send>,
    pub(super) recv_handle: Option<JoinHandle<anyhow::Result<()>>>
}

#[derive(Resource)]
pub struct DtlsServer {
    pub(super) runtime: Arc<Runtime>,
    
    pub(super) listener: Option<Arc<dyn Listener + Sync + Send>>,
    pub(super) accept_handle: Option<JoinHandle<anyhow::Result<()>>>,
    pub(super) accept_tx: CrossbeamTx<ConnIndex>,
    pub(super) accept_rx: CrossbeamRx<ConnIndex>,
    
    pub(super) conn_map: Arc<StdRwLock<HashMap<usize, DtlsConn>>>,
    pub(super) recv_buf_size: usize,
    pub(super) recv_tx: CrossbeamTx<(ConnIndex, Bytes)>,
    pub(super) recv_rx: CrossbeamRx<(ConnIndex, Bytes)>,
}

impl DtlsServer {
    pub fn new(recv_buf_size: usize) 
    -> anyhow::Result<Self> {
        let rt = runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

        let (accept_tx, accept_rx) = crossbeam_channel::<ConnIndex>();
        let (recv_tx, recv_rx) = crossbeam_channel::<(ConnIndex, Bytes)>();

        Ok(Self { 
            runtime: Arc::new(rt),
            listener: None, 
            accept_handle: None,
            accept_tx,
            accept_rx,
            conn_map: default(),
            recv_buf_size,
            recv_tx,
            recv_rx
        })
    }

    pub fn try_recv(&self) -> Result<(ConnIndex, Bytes), TryRecvError> {
        self.recv_rx.try_recv()
    }

    pub fn start(&mut self, config: DtlsServerConfig)
    -> anyhow::Result<()> {
        self.start_listen(config)?;
        self.start_accept_loop()
    }

    pub(super) fn start_listen(&mut self, config: DtlsServerConfig) 
    -> anyhow::Result<()> {
        let listener = future::block_on(
            self.runtime.spawn(Self::listen(config))
        )??;
        self.listener = Some(listener);
        Ok(())
    }

    async fn listen(config: DtlsServerConfig)
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

    pub(super) fn start_accept_loop(&mut self)
    -> anyhow::Result<()> {
        let listener = match self.listener {
            Some(ref l) => l.clone(),
            None => bail!("listener is none")
        };
        let conns = self.conn_map.clone();
        let accept_tx = self.accept_tx.clone();
        let handle = self.runtime.spawn(
            Self::accept_loop(listener, conns, accept_tx)            
        );
        
        self.accept_handle = Some(handle);
        Ok(())
    }

    async fn accept_loop(
        listener: Arc<dyn Listener + Sync + Send>,
        conn_map: Arc<StdRwLock<HashMap<usize, DtlsConn>>>,
        accepted_tx: CrossbeamTx<ConnIndex>
    ) -> anyhow::Result<()> {
        let mut index = 0;

        loop {
            let idx = index;
            index += 1;
            let (conn, addr) = listener.accept().await?;
            info!("conn from {addr} accepted");

            let mut w = conn_map.write().unwrap();
            debug_assert!(!w.contains_key(&idx));

            w.insert(idx, DtlsConn{
                conn,
                recv_handle: None,
            });
            accepted_tx.send(ConnIndex(idx))?;
        }
    }

    pub(super) fn start_recv_loop(&self, conn_idx: ConnIndex) 
    -> anyhow::Result<()> {
        let mut w = self.conn_map.write().unwrap();
        let dtls_conn = match w.get_mut(&conn_idx.0) {
            None => bail!("dtls conn is None"),
            Some(c) => c
        };
        
        let conn = dtls_conn.conn.clone();
        let recv_tx = self.recv_tx.clone();
        let handle = self.runtime.spawn(Self::recv_loop(
            conn_idx,
            conn,
            recv_tx,
            self.recv_buf_size
        ));
        dtls_conn.recv_handle = Some(handle);
        
        Ok(())
    }

    async fn recv_loop(
        conn_idx: ConnIndex,
        conn: Arc<dyn Conn + Sync + Send>,
        recv_tx: CrossbeamTx<(ConnIndex, Bytes)>, 
        buf_size: usize
    ) -> anyhow::Result<()> {
        let mut buf = BytesMut::zeroed(buf_size);

        loop {
            let (n, addr) = conn.recv_from(&mut buf).await?;
            let recved = buf.split_to(n)
            .freeze();
            recv_tx.send((conn_idx, recved))?;

            buf.resize(buf_size, 0);
            info!("received {n}bytes from {addr}");
        }
    }
}
