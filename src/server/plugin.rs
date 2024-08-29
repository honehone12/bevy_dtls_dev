use bevy::prelude::*;
use super::dtls_server::*;

fn accept_system(mut dtls_server: ResMut<DtlsServer>) {
    let Some(acpted) = dtls_server.acpt() else {
        return;
    };

    if let Err(e) = dtls_server.start_conn(acpted) {
        panic!("{e}");
    }

    debug!("conn: {} has been started from system", acpted.0);
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
