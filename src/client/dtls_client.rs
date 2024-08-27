use std::sync::Arc;
use anyhow::{anyhow, bail};
use bevy::{
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

pub struct DtlsClientConfig {
    pub server_addr: &'static str,
    pub client_addr: &'static str,
    pub server_name: &'static str
}

pub struct DtlsClientHealth {
    pub sender: Option<anyhow::Result<()>>,
    pub recver: Option<anyhow::Result<()>>
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

    pub fn send(&self, message: Bytes) -> anyhow::Result<()> {
        let send_tx = match self.send_tx {
            Some(ref tx) => tx,
            None => bail!("send tx is None")
        };
        
        send_tx.send(message)?;
        Ok(())
    }

    pub fn health_check(&mut self) -> DtlsClientHealth {
        DtlsClientHealth{
            sender: self.sender_health_check(),
            recver: None
        }
    }

    pub(super) fn sender_health_check(&mut self) 
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

    pub fn start(&mut self, config: DtlsClientConfig) 
    -> anyhow::Result<()> {
        self.start_connect(config)?;
        self.start_send_loop()
    }

    pub(super) fn start_connect(&mut self, config: DtlsClientConfig) 
    -> anyhow::Result<()> {
        let conn = future::block_on(self.runtime.spawn(
            Self::connect(config)
        ))??;
        self.conn = Some(conn);
        Ok(())
    }

    async fn connect(config: DtlsClientConfig) 
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

    pub(super) fn start_send_loop(&mut self) 
    -> anyhow::Result<()> {
        let c = match self.conn {
            Some(ref c) => c.clone(),
            None => bail!("conn is none")
        };

        let (send_tx, send_rx) = tokio_channel::<Bytes>();
        self.send_tx = Some(send_tx);
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
}
