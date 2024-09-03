use std::{
    collections::HashMap, net::IpAddr, sync::{Arc, RwLock as StdRwLock}, time::Duration
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
    task::JoinHandle,
    time::{timeout, sleep}
};
use webrtc_dtls::listener;
use webrtc_util::conn::{Listener, Conn};
use bytes::{Bytes, BytesMut};
use super::cert_option::ServerCertOption;

#[derive(Clone, Copy)]
pub struct ConnIndex(pub(crate) usize);

impl ConnIndex {
    pub fn index(&self) -> usize {
        self.0
    }
}

pub struct DtlsServerConfig {
    pub listen_addr: IpAddr,
    pub listen_port: u16,
    pub cert_option: ServerCertOption
}

struct DtlsServerClose;

pub struct DtlsServerHealth {
    pub listener: Option<anyhow::Result<()>>,
    pub sender: Vec<(usize, anyhow::Result<()>)>,
    pub recver: Vec<(usize, anyhow::Result<()>)>
}

pub(super) struct DtlsConn {
    conn: Arc<dyn Conn + Sync + Send>,
    
    recv_handle: Option<JoinHandle<anyhow::Result<()>>>,
    close_recv_tx: Option<TokioTx<DtlsServerClose>>,

    send_handle: Option<JoinHandle<anyhow::Result<()>>>,
    send_tx: Option<TokioTx<Bytes>>,
    close_send_tx: Option<TokioTx<DtlsServerClose>>
}

impl DtlsConn {
    pub(super) fn new(conn: Arc<dyn Conn + Sync + Send>) -> Self {
        Self{
            conn,
            recv_handle: None,
            close_recv_tx: None,
            send_handle: None,
            send_tx: None,
            close_send_tx: None,
        }
    }
}

#[derive(Resource)]
pub struct DtlsServer {
    runtime: Arc<Runtime>,
    
    listener: Option<Arc<dyn Listener + Sync + Send>>,
    acpt_handle: Option<JoinHandle<anyhow::Result<()>>>,
    acpt_rx: Option<TokioRx<ConnIndex>>,
    close_acpt_tx: Option<TokioTx<DtlsServerClose>>,
    
    conn_map: Arc<StdRwLock<HashMap<usize, DtlsConn>>>,

    send_timeout: u64,

    recv_buf_size: usize,
    recv_timeout: Option<u64>,
    recv_tx: Option<TokioTx<(ConnIndex, Bytes)>>,
    recv_rx: Option<TokioRx<(ConnIndex, Bytes)>>,
}

impl DtlsServer {
    pub fn new(
        recv_buf_size: usize, 
        send_timeout: u64,
        recv_timeout: Option<u64>
    ) -> anyhow::Result<Self> {
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

            send_timeout,

            recv_timeout,
            recv_buf_size,
            recv_tx: None,
            recv_rx: None,
        })
    }

    pub fn clients_len(&self) -> usize {
        let r = self.conn_map.read().unwrap();
        r.len()
    }

    pub fn start(&mut self, config: DtlsServerConfig)
    -> anyhow::Result<()> {
        self.start_listen(config)?;
        self.start_acpt_loop()
    }

    pub(super) fn start_conn(&mut self, conn_index: ConnIndex)
    -> anyhow::Result<()> {
        self.start_recv_loop(conn_index)?;
        self.start_send_loop(conn_index)
    }

    pub(super) fn acpt(&mut self) -> Option<ConnIndex> {
        let Some(ref mut acpt_rx) = self.acpt_rx else {
            return None;
        };

        match acpt_rx.try_recv() {
            Ok(a) => Some(a),
            Err(TryRecvError::Empty) => None,
            Err(e) => {
                error!("acpt rx is closed before set to None: {e}");
                None
            }
        }
    }

    pub fn send(&self, conn_index: usize, message: Bytes) 
    -> anyhow::Result<()> {
        let r = self.conn_map.read()
        .unwrap();
        let Some(ref dtls_conn) = r.get(&conn_index) else {
            bail!("dtls conn: {conn_index} is None");
        };
        let Some(ref send_tx) = dtls_conn.send_tx else {
            bail!("send tx: {conn_index} is None");
        };

        if let Err(e) = send_tx.send(message) {
            bail!("conn: {conn_index} is not started or disconnected: {e}");
        }
        Ok(())
    }

    pub fn broadcast(&self, message: Bytes) -> anyhow::Result<()> {
        let r = self.conn_map.read()
        .unwrap();

        for (idx, ref dtls_conn) in r.iter() {
            let Some(ref send_tx) = dtls_conn.send_tx else {
                warn!("send tx: {idx} is None");
                continue;
            };
    
            if let Err(e) = send_tx.send(message.clone()) {
                warn!("conn: {idx} is not started or disconnected: {e}");
                continue;
            }
        }

        Ok(())
    }

    pub fn recv(&mut self) -> Option<(ConnIndex, Bytes)> {
        let Some(ref mut recv_rx) = self.recv_rx else {
            return None;
        };

        match recv_rx.try_recv() {
            Ok(ib) => Some(ib),
            Err(e) => {
                if matches!(e, TryRecvError::Disconnected) {
                    warn!("recv rx is closed before set to None: {e}");
                }
                None
            }
        }
    }

    pub fn health_check(&mut self) -> DtlsServerHealth {
        DtlsServerHealth{
            listener: self.health_check_acpt(),
            sender: self.health_check_send(),
            recver: self.health_check_recv()
        }
    }

    pub fn close_conn(&mut self, conn_index: usize) {
        let mut w = self.conn_map.write()
        .unwrap();
        let Some(dtls_conn) = w.remove(&conn_index) else {
            return;
        };
        
        if let Some(close_recv_tx) = dtls_conn.close_recv_tx {
            if let Err(e) = close_recv_tx.send(DtlsServerClose) {
                error!("close recv tx: {conn_index} is closed before set to None: {e}");
            }    
        };

        if let Some(close_send_tx) = dtls_conn.close_send_tx {
            if let Err(e) = close_send_tx.send(DtlsServerClose) {
                error!("close recv tx: {conn_index} is closed before set to None: {e}");
            }
        }
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

        self.close_acpt_loop();
        self.recv_tx = None;
        self.recv_rx = None;
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
        let listener = listener::listen(
            (config.listen_addr, config.listen_port), 
            config.cert_option.to_dtls_config()?
        ).await?;

        debug!("dtls server listening at {}", config.listen_addr);
        Ok(Arc::new(listener))
    }

    fn start_acpt_loop(&mut self)
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
            Self::acpt_loop(listener, conns, acpt_tx, close_acpt_rx)            
        );
        
        self.acpt_handle = Some(handle);
        debug!("acpt loop is started");
        Ok(())
    }

    async fn acpt_loop(
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
            w.insert(idx, DtlsConn::new(conn));

            acpt_tx.send(ConnIndex(idx))?;
            debug!("conn from {addr} accepted");
        }

        listener.close().await?;
        debug!("dtls server listener is closed");
        Ok(())
    }

    fn health_check_acpt(&mut self) 
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

    fn close_acpt_loop(&mut self) {
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

    fn start_recv_loop(&self, conn_idx: ConnIndex) 
    -> anyhow::Result<()> {
        let mut w = self.conn_map.write()
        .unwrap();
        let dtls_conn = match w.get_mut(&conn_idx.0) {
            Some(c) => c,
            None => bail!("dtls conn: {} is None", conn_idx.0)
        };
        let conn = dtls_conn.conn.clone();
        let recv_tx = match self.recv_tx {
            Some(ref tx) => tx.clone(),
            None => bail!("recv tx is still None")
        };

        let (close_recv_tx, close_recv_rx) = tokio_channel::<DtlsServerClose>();
        dtls_conn.close_recv_tx = Some(close_recv_tx);

        let handle = self.runtime.spawn(Self::recv_loop(
            conn_idx,
            self.recv_buf_size,
            conn,
            self.recv_timeout,
            recv_tx,
            close_recv_rx
        ));
        dtls_conn.recv_handle = Some(handle);
        
        debug!("recv loop: {} has started", conn_idx.0);
        Ok(())
    }

    async fn recv_loop(
        conn_idx: ConnIndex,
        buf_size: usize,
        conn: Arc<dyn Conn + Sync + Send>,
        timeout_secs: Option<u64>,
        recv_tx: TokioTx<(ConnIndex, Bytes)>, 
        mut close_recv_rx: TokioRx<DtlsServerClose>
    ) -> anyhow::Result<()> {
        let mut buf = BytesMut::zeroed(buf_size);

        loop {
            let timeout_dur = match timeout_secs {
                Some(t) => Duration::from_secs(t),
                None => Duration::MAX
            };

            let (n, addr) = select! {
                biased;

                result = conn.recv_from(&mut buf) => result?,
                Some(_) = close_recv_rx.recv() => break,
                () = sleep(timeout_dur) => bail!("conn: {} recv timeout", conn_idx.0),
                else => {
                    error!("close recv tx: {} is closed before rx is closed", conn_idx.0);
                    break;
                }
            };

            let recved = buf.split_to(n)
            .freeze();
            recv_tx.send((conn_idx, recved))?;

            buf.resize(buf_size, 0);
            debug!("received {n}bytes from {}:{addr}", conn_idx.0);
        }

        conn.close().await?;
        debug!("dtls server recv loop: {} is closed", conn_idx.0);
        Ok(())
    }

    fn health_check_recv(&mut self)
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

    fn start_send_loop(&mut self, conn_idx: ConnIndex) 
    -> anyhow::Result<()> {
        let mut w = self.conn_map.write()
        .unwrap();
        let Some(dtls_conn) = w.get_mut(&conn_idx.0) else {
            bail!("dtls conn: {} is None", conn_idx.0);
        };

        let conn = dtls_conn.conn.clone();

        let (send_tx, send_rx) = tokio_channel::<Bytes>();
        let (close_send_tx, close_send_rx) = tokio_channel::<DtlsServerClose>();
        dtls_conn.send_tx = Some(send_tx);
        dtls_conn.close_send_tx = Some(close_send_tx);

        let handle = self.runtime.spawn(Self::send_loop(
            conn_idx, 
            conn,
            self.send_timeout, 
            send_rx, 
            close_send_rx
        ));
        dtls_conn.send_handle = Some(handle);

        debug!("send loop: {} has started", conn_idx.0);
        Ok(())
    }

    async fn send_loop(
        conn_idx: ConnIndex,
        conn: Arc<dyn Conn + Sync + Send>,
        timeout_secs: u64,
        mut send_rx: TokioRx<Bytes>,
        mut close_send_rx: TokioRx<DtlsServerClose>
    ) -> anyhow::Result<()> {
        loop {
            select! {
                biased;

                Some(msg) = send_rx.recv() => {
                    timeout(
                        Duration::from_secs(timeout_secs),
                        conn.send(&msg)
                    ).await??;
                }
                Some(_) = close_send_rx.recv() => break,
                else => {
                    warn!("close send tx: {} is closed before rx is closed", conn_idx.0);
                    break;
                }
            }
        }

        conn.close().await?;
        debug!("dtls server send loop: {} is closed", conn_idx.0);
        Ok(())
    }

    fn health_check_send(&mut self)
    -> Vec<(usize, anyhow::Result<()>)> {
        let finished = {
            let mut v = vec![];
            let mut w = self.conn_map.write()
            .unwrap();
            for (idx, dtls_conn) in w.iter_mut() {
                let Some(ref handle_ref) = dtls_conn.send_handle else {
                    continue;
                };
                
                if !handle_ref.is_finished() {
                    continue;
                }

                let handle = dtls_conn.send_handle.take()
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
}
