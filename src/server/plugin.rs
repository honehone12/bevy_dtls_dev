use bevy::prelude::*;
use super::dtls_server::*;
use tokio::sync::mpsc::error::TryRecvError;

pub(super) fn accept_system(mut dtls_server: ResMut<DtlsServer>) {
    let Some(ref mut acpt_rx) = dtls_server.acpt_rx else {
        return;
    };

    let accepted = match acpt_rx.try_recv() {
        Ok(a) => a,
        Err(TryRecvError::Empty) => return,
        Err(e) => panic!("{e}") 
    };

    debug!("starting recv for conn: {}", accepted.index());
    if let Err(e) = dtls_server.start_conn(accepted) {
        panic!("{e}");
    }
}

pub struct DtlsServerPlugin {
    pub listen_addr: &'static str,
    pub server_name: &'static str,
    pub buf_size: usize
}

impl Plugin for DtlsServerPlugin {
    fn build(&self, app: &mut App) {
        let mut dtls_server = match DtlsServer::new(self.buf_size) {
            Ok(s) => s,
            Err(e) => panic!("{e}")
        };

        if let Err(e) = dtls_server.start(DtlsServerConfig{
            listen_addr: self.listen_addr,
            server_names: vec![self.server_name.to_string()]
        }) {
            panic!("{e}");
        }

        app.insert_resource(dtls_server)
        .add_systems(Update, accept_system);
    }
}
