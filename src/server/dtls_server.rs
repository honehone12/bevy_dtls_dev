use std::{
    collections::HashMap, 
    sync::{Arc, RwLock as StdRwLock}
};
use anyhow::{anyhow, bail};
use bevy::{
    prelude::*, 
    tasks::futures_lite::future, 
};
use tokio::{
    runtime::{self, Runtime}, 
    sync::mpsc::{
        unbounded_channel as tokio_channel, 
        UnboundedSender as TokioTx,
        UnboundedReceiver as TokioRx,
        error::TryRecvError
    },
    task::JoinHandle
};
use webrtc_dtls::{
    config::{Config, ExtendedMasterSecretType}, 
    crypto::Certificate, listener,
};
use webrtc_util::conn::{Listener, Conn};
use bytes::{Bytes, BytesMut};

pub struct DtlsServerConfig {
    pub listen_addr: &'static str,
    pub server_names: Vec<String>
}

pub struct DtlsClientClose;

pub struct DtlsServerHealth {
    pub listener: Option<anyhow::Result<()>>,
    pub sender: Vec<(usize, anyhow::Result<()>)>,
    pub recver: Vec<(usize, anyhow::Result<()>)>
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
    pub(super) accept_tx: TokioTx<ConnIndex>,
    pub(super) accept_rx: TokioRx<ConnIndex>,
    
    pub(super) conn_map: Arc<StdRwLock<HashMap<usize, DtlsConn>>>,
    pub(super) recv_buf_size: usize,
    pub(super) recv_tx: TokioTx<(ConnIndex, Bytes)>,
    pub(super) recv_rx: TokioRx<(ConnIndex, Bytes)>,
}

impl DtlsServer {
    pub fn new(recv_buf_size: usize) 
    -> anyhow::Result<Self> {
        let rt = runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

        let (accept_tx, accept_rx) = tokio_channel::<ConnIndex>();
        let (recv_tx, recv_rx) = tokio_channel::<(ConnIndex, Bytes)>();

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

    pub fn try_recv(&mut self) -> Result<(ConnIndex, Bytes), TryRecvError> {
        self.recv_rx.try_recv()
    }

    pub fn health_check(&mut self) -> DtlsServerHealth {
        DtlsServerHealth{
            listener: self.listener_health_check(),
            sender: vec![],
            recver: self.recver_health_check()
        }
    }

    pub(super) fn listener_health_check(&mut self) 
    -> Option<anyhow::Result<()>> {
        let handle_ref = self.accept_handle.as_ref()?;

        if !handle_ref.is_finished() {
            return None;
        }

        let handle = self.accept_handle.take()?;
        match future::block_on(handle) {
            Ok(r) => Some(r),
            Err(e) => Some(Err(anyhow!(e)))
        }
    }

    pub(super) fn recver_health_check(&mut self)
    -> Vec<(usize, anyhow::Result<()>)> {
        let finished = {
            let mut v = vec![];
            let mut w = self.conn_map.write().unwrap();
            for (idx, dtls_conn) in w.iter_mut() {
                let Some(ref handle_ref) = dtls_conn.recv_handle else {
                    continue;
                };
                
                if !handle_ref.is_finished() {
                    continue;
                }

                let handle = dtls_conn.recv_handle.take()
                .unwrap();
                v.push((*idx, handle));
            }
            v
        };

        let mut results = vec![];
        for (idx, handle) in finished {
            let r = match future::block_on(handle) {
                Ok(r) => r,
                Err(e) => Err(anyhow!(e))
            };
            results.push((idx, r));
        }
        results
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
        accepted_tx: TokioTx<ConnIndex>,
        //close_accept_rx: TokioRx<DtlsClientClose>
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
        recv_tx: TokioTx<(ConnIndex, Bytes)>, 
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
