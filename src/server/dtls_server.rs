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
    select, 
    sync::mpsc::{
        error::TryRecvError, 
        unbounded_channel as tokio_channel, 
        UnboundedReceiver as TokioRx, 
        UnboundedSender as TokioTx
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

pub struct DtlsServerClose;

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
    pub(super) recv_handle: Option<JoinHandle<anyhow::Result<()>>>,
    pub(super) close_recv_tx: Option<TokioTx<DtlsServerClose>>
}

#[derive(Resource)]
pub struct DtlsServer {
    pub(super) runtime: Arc<Runtime>,
    
    pub(super) listener: Option<Arc<dyn Listener + Sync + Send>>,
    pub(super) acpt_handle: Option<JoinHandle<anyhow::Result<()>>>,
    pub(super) acpt_rx: Option<TokioRx<ConnIndex>>,
    pub(super) close_acpt_tx: Option<TokioTx<DtlsServerClose>>,
    
    pub(super) conn_map: Arc<StdRwLock<HashMap<usize, DtlsConn>>>,
    pub(super) recv_buf_size: usize,
    pub(super) recv_tx: Option<TokioTx<(ConnIndex, Bytes)>>,
    pub(super) recv_rx: Option<TokioRx<(ConnIndex, Bytes)>>,
}

impl DtlsServer {
    pub fn new(recv_buf_size: usize) 
    -> anyhow::Result<Self> {
        let rt = runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

        Ok(Self { 
            runtime: Arc::new(rt),

            listener: None, 
            acpt_handle: None,
            acpt_rx: None,
            close_acpt_tx: None,
            
            conn_map: default(),
            recv_buf_size,
            recv_tx: None,
            recv_rx: None,
        })
    }

    pub fn close_all(&mut self) {
        let ks: Vec<usize> = {
            self.conn_map.read()
            .unwrap()
            .keys()
            .cloned()
            .collect()
        };

        for k in ks {
            self.close_conn(k);
        }

        self.close_listener();
        self.recv_tx = None;
        self.recv_rx = None;
    }

    fn close_listener(&mut self) {
        let Some(ref close_acpt_tx) = self.close_acpt_tx else {
            return;
        };

        if let Err(e) = close_acpt_tx.send(DtlsServerClose) {
            error!("close listener tx is closed before set to None: {e}");
        }

        self.close_acpt_tx = None;
        self.acpt_rx = None;
        self.listener = None;
    }

    pub fn close_conn(&mut self, index: usize) {
        let mut w = self.conn_map.write()
        .unwrap();
        let Some(dtls_conn) = w.remove(&index) else {
            return;
        };
        let Some(close_recv_tx) = dtls_conn.close_recv_tx else {
            return;
        };

        if let Err(e) = close_recv_tx.send(DtlsServerClose) {
            error!("close recv tx is closed before set to None: {e}");
        }
    }

    pub fn recv(&mut self) -> Option<(ConnIndex, Bytes)> {
        let Some(ref mut recv_rx) = self.recv_rx else {
            return None;
        };

        match recv_rx.try_recv() {
            Ok(ib) => Some(ib),
            Err(e) => {
                if matches!(e, TryRecvError::Disconnected) {
                    warn!("recv rx is closed before set to None");
                }
                None
            }
        }
    }

    pub fn health_check(&mut self) -> DtlsServerHealth {
        DtlsServerHealth{
            listener: self.listener_health_check(),
            sender: vec![],
            recver: self.recver_health_check()
        }
    }

    fn listener_health_check(&mut self) 
    -> Option<anyhow::Result<()>> {
        let handle_ref = self.acpt_handle.as_ref()?;

        if !handle_ref.is_finished() {
            return None;
        }

        let handle = self.acpt_handle.take()?;
        match future::block_on(handle) {
            Ok(r) => Some(r),
            Err(e) => Some(Err(anyhow!(e)))
        }
    }

    fn recver_health_check(&mut self)
    -> Vec<(usize, anyhow::Result<()>)> {
        let finished = {
            let mut v = vec![];
            let mut w = self.conn_map.write()
            .unwrap();
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

    fn start_listen(&mut self, config: DtlsServerConfig) 
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

        debug!("dtls server listening at {}", config.listen_addr);
        Ok(Arc::new(listener))
    }

    fn start_accept_loop(&mut self)
    -> anyhow::Result<()> {
        let listener = match self.listener {
            Some(ref l) => l.clone(),
            None => bail!("listener is None")
        };
        let conns = self.conn_map.clone();
        
        let (acpt_tx, acpt_rx) = tokio_channel::<ConnIndex>();
        let (close_acpt_tx, close_acpt_rx) = tokio_channel::<DtlsServerClose>();
        let (recv_tx, recv_rx) = tokio_channel::<(ConnIndex, Bytes)>();
    
        self.acpt_rx = Some(acpt_rx);
        self.close_acpt_tx = Some(close_acpt_tx);
        self.recv_tx = Some(recv_tx);
        self.recv_rx = Some(recv_rx);
        
        let handle = self.runtime.spawn(
            Self::accept_loop(listener, conns, acpt_tx, close_acpt_rx)            
        );
        
        self.acpt_handle = Some(handle);
        Ok(())
    }

    async fn accept_loop(
        listener: Arc<dyn Listener + Sync + Send>,
        conn_map: Arc<StdRwLock<HashMap<usize, DtlsConn>>>,
        acpt_tx: TokioTx<ConnIndex>,
        mut close_acpt_rx: TokioRx<DtlsServerClose>
    ) -> anyhow::Result<()> {
        let mut index = 0;

        loop {
            let (conn, addr) = select! {
                biased;

                result = listener.accept() => result?,
                Some(_) = close_acpt_rx.recv() => break,
                else => {
                    error!("close acpt tx is dropped before rx is closed");
                    break;
                }
            };

            let idx = index;
            index += 1;
            
            let mut w = conn_map.write()
            .unwrap();
            debug_assert!(!w.contains_key(&idx));
            w.insert(idx, DtlsConn{
                conn,
                recv_handle: None,
                close_recv_tx: None,
            });

            acpt_tx.send(ConnIndex(idx))?;
            debug!("conn from {addr} accepted");
        }

        listener.close().await?;
        debug!("dtls server listener is closed");
        Ok(())
    }

    pub(super) fn start_recv_loop(&self, conn_idx: ConnIndex) 
    -> anyhow::Result<()> {
        let mut w = self.conn_map.write()
        .unwrap();
        let dtls_conn = match w.get_mut(&conn_idx.0) {
            Some(c) => c,
            None => bail!("dtls conn is None")
        };
        let conn = dtls_conn.conn.clone();
        let recv_tx = match self.recv_tx {
            Some(ref tx) => tx.clone(),
            None => bail!("recv tx is None")
        };

        let (close_recv_tx, close_recv_rx) = tokio_channel::<DtlsServerClose>();
        dtls_conn.close_recv_tx = Some(close_recv_tx);

        let handle = self.runtime.spawn(Self::recv_loop(
            conn_idx,
            conn,
            recv_tx,
            close_recv_rx,
            self.recv_buf_size
        ));
        dtls_conn.recv_handle = Some(handle);
        
        Ok(())
    }

    async fn recv_loop(
        conn_idx: ConnIndex,
        conn: Arc<dyn Conn + Sync + Send>,
        recv_tx: TokioTx<(ConnIndex, Bytes)>, 
        mut close_recv_rx: TokioRx<DtlsServerClose>,
        buf_size: usize
    ) -> anyhow::Result<()> {
        let mut buf = BytesMut::zeroed(buf_size);

        loop {
            let (n, addr) = select! {
                biased;

                result = conn.recv_from(&mut buf) => result?,
                Some(_) = close_recv_rx.recv() => break,
                else => {
                    error!("close recv tx is closed before rx is closed");
                    break;
                }
            };

            let recved = buf.split_to(n)
            .freeze();
            recv_tx.send((conn_idx, recved))?;

            buf.resize(buf_size, 0);
            debug!("received {n}bytes from {addr}");
        }

        conn.close().await?;
        debug!("dtls conn {} is closed", conn_idx.index());
        Ok(())
    }
}
