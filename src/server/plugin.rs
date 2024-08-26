use bevy::prelude::*;
use super::dtls_server::*;
use crossbeam::channel::TryRecvError;

pub(super) fn accept_system(dtls_server: Res<DtlsServer>) {
    let accepted = match dtls_server.accept_rx.try_recv() {
        Ok(a) => a,
        Err(TryRecvError::Empty) => return,
        Err(e) => panic!("{e}") 
    };

    if let Err(e) = dtls_server.start_recv_loop(accepted) {
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
