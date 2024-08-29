use std::sync::Arc;
use anyhow::{anyhow, bail};
use bevy::{
    prelude::*, 
    tasks::futures_lite::future
};
use bytes::{Bytes, BytesMut};
use tokio::{
    net::UdpSocket as TokioUdpSocket, 
    runtime::{self, Runtime},
    select,
    sync::mpsc::{
        unbounded_channel as tokio_channel, 
        UnboundedSender as TokioTx,
        UnboundedReceiver as TokioRx,
        error::TryRecvError
    }, 
    task::JoinHandle,
};
use webrtc_dtls::{
    config::{Config, ExtendedMasterSecretType}, 
    conn::DTLSConn, 
    crypto::Certificate
};
use webrtc_util::Conn;

pub struct DtlsClientConfig {
    pub server_addr: &'static str,
    pub client_addr: &'static str,
    pub server_name: &'static str
}

pub struct DtlsClientHealth {
    pub sender: Option<anyhow::Result<()>>,
    pub recver: Option<anyhow::Result<()>>
}

struct DtlsClientClose;

#[derive(Resource)]
pub struct DtlsClient {
    runtime: Arc<Runtime>,

    conn: Option<Arc<dyn Conn + Sync + Send>>,

    send_handle: Option<JoinHandle<anyhow::Result<()>>>,
    send_tx: Option<TokioTx<Bytes>>,
    close_send_tx: Option<TokioTx<DtlsClientClose>>,

    recv_handle: Option<JoinHandle<anyhow::Result<()>>>,
    recv_buf_size: usize,
    recv_rx: Option<TokioRx<Bytes>>,
    close_recv_tx: Option<TokioTx<DtlsClientClose>>
}

impl DtlsClient {
    pub fn new(recv_buf_size: usize) -> anyhow::Result<Self> {
        let rt = runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?; 

        Ok(Self{
            runtime: Arc::new(rt),

            conn: None,
            
            send_handle: None,
            send_tx: None,
            close_send_tx: None,
            
            recv_handle: None,
            recv_buf_size,
            recv_rx: None,
            close_recv_tx: None
        })
    }

    pub fn start(&mut self, config: DtlsClientConfig) 
    -> anyhow::Result<()> {
        self.start_connect(config)?;
        self.start_send_loop()?;
        self.start_recv_loop()
    }

    pub fn send(&self, message: Bytes) -> anyhow::Result<()> {
        let Some(ref send_tx) = self.send_tx else {
            bail!("send tx is None");
        };

        if let Err(e) = send_tx.send(message) {
            bail!("conn is not started or disconnected: {e}");
        }
        Ok(())
    }

    pub fn recv(&mut self) -> Option<Bytes> {
        let Some(ref mut recv_rx) = self.recv_rx else {
            return None;
        };

        match recv_rx.try_recv() {
            Ok(b) => Some(b),
            Err(e) => {
                if matches!(e, TryRecvError::Disconnected) {
                    warn!("recv rx is closed before set to None: {e}");
                }
                None
            }
        }
    }

    pub fn health_check(&mut self) -> DtlsClientHealth {
        DtlsClientHealth{
            sender: self.health_check_send(),
            recver: self.health_check_recv()
        }
    }

    pub fn close(&mut self) {
        self.close_send_loop();
        self.close_recv_loop();
        self.conn = None;
    }

    fn start_connect(&mut self, config: DtlsClientConfig) 
    -> anyhow::Result<()> {
        let conn = future::block_on(self.runtime.spawn(
            Self::connect(config)
        ))??;
        self.conn = Some(conn);
        debug!("dtls client has connected");
        Ok(())
    }

    async fn connect(config: DtlsClientConfig) 
    -> anyhow::Result<Arc<impl Conn + Sync + Send>> {
        let socket = TokioUdpSocket::bind(config.client_addr).await?;
        socket.connect(config.server_addr).await?;
        debug!("connecting to {}", config.server_addr);

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

        Ok(Arc::new(dtls_conn))
    }

    fn start_send_loop(&mut self) -> anyhow::Result<()> {
        let c = match self.conn {
            Some(ref c) => c.clone(),
            None => bail!("conn is none")
        };

        let (send_tx, send_rx) = tokio_channel::<Bytes>();
        let(close_send_tx, close_send_rx) = tokio_channel::<DtlsClientClose>();

        self.send_tx = Some(send_tx);
        self.close_send_tx = Some(close_send_tx);

        let handle = self.runtime.spawn(
            Self::send_loop(c, send_rx, close_send_rx)
        );
        self.send_handle = Some(handle);

        debug!("send loop has started");
        Ok(())
    }

    async fn send_loop(
        conn: Arc<dyn Conn + Sync + Send>,
        mut send_rx: TokioRx<Bytes>,
        mut close_send_rx: TokioRx<DtlsClientClose>
    )-> anyhow::Result<()> {
        loop {
            select! {
                biased;

                Some(msg) = send_rx.recv() => {
                    conn.send(&msg).await?;
                }
                Some(_) = close_send_rx.recv() => break,
                else => {
                    warn!("close send tx is closed before rx is closed");
                    break;
                }
            }
        }

        conn.close().await?;
        debug!("dtls client send loop is closed");
        Ok(())
    }

    fn health_check_send(&mut self) 
    -> Option<anyhow::Result<()>> {
        let handle_ref = self.send_handle.as_ref()?;

        if !handle_ref.is_finished() {
            return None;
        }

        let handle = self.send_handle.take()
        .unwrap();
        match future::block_on(handle) {
            Ok(r) => Some(r),
            Err(e) => Some(Err(anyhow!(e)))
        }
    }

    fn close_send_loop(&mut self) {
        let Some(ref close_send_tx) = self.close_send_tx else {
            return;
        };

        if let Err(e) = close_send_tx.send(DtlsClientClose) {
            error!("close send tx is closed before set to None: {e}");
        }

        self.close_send_tx = None;
        self.send_tx = None;
    }

    fn start_recv_loop(&mut self) -> anyhow::Result<()> {
        let conn = match self.conn {
            Some(ref c) => c.clone(),
            None => bail!("dtls conn is None")
        };

        let (recv_tx, recv_rx) = tokio_channel::<Bytes>();
        let (close_recv_tx, close_recv_rx) = tokio_channel::<DtlsClientClose>();
        self.recv_rx = Some(recv_rx);
        self.close_recv_tx = Some(close_recv_tx);

        let handle = self.runtime.spawn(Self::recv_loop(
            self.recv_buf_size,
            conn,
            recv_tx,
            close_recv_rx
        ));
        self.recv_handle = Some(handle);

        debug!("recv loop has started");
        Ok(())
    }

    async fn recv_loop(
        buf_size: usize,
        conn: Arc<dyn Conn + Sync + Send>,
        recv_tx: TokioTx<Bytes>,
        mut close_recv_rx: TokioRx<DtlsClientClose>
    ) -> anyhow::Result<()> {
        let mut buf = BytesMut::zeroed(buf_size);

        loop {
            let n = select! {
                biased;

                result = conn.recv(&mut buf) => result?,
                Some(_) = close_recv_rx.recv() => break,
                else => {
                    error!("close recv tx is closed before rx is closed");
                    break;
                }
            };

            let receved = buf.split_to(n)
            .freeze();
            recv_tx.send(receved)?;

            buf.resize(buf_size, 0);
            debug!("received {n}bytes from");
        }

        conn.close().await?;
        debug!("dtls client recv loop is closed");
        Ok(())
    }

    fn health_check_recv(&mut self) 
    -> Option<anyhow::Result<()>> {
        let handle_ref = self.recv_handle.as_ref()?;

        if !handle_ref.is_finished() {
            return None;
        }

        let handle = self.recv_handle.take()
        .unwrap();
        match future::block_on(handle) {
            Ok(r) => Some(r),
            Err(e) => Some(Err(anyhow!(e)))
        }
    }

    fn close_recv_loop(&mut self) {
        let Some(ref close_recv_tx) = self.close_recv_tx else {
            return;
        };

        if let Err(e) = close_recv_tx.send(DtlsClientClose) {
            error!("close recv tx is closed before set to None: {e}");
        }

        self.close_recv_tx = None;
        self.recv_rx = None;   
    }
}
