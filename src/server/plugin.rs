use bevy::prelude::*;
use rustls::crypto::aws_lc_rs;
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
    pub buf_size: usize
}

impl Plugin for DtlsServerPlugin {
    fn build(&self, app: &mut App) {
        if aws_lc_rs::default_provider()
        .install_default()
        .is_err() {
            panic!("failed to setup crypto provider")
        }

        let dtls_server = match DtlsServer::new(self.buf_size) {
            Ok(s) => s,
            Err(e) => panic!("{e}")
        };

        app.insert_resource(dtls_server)
        .add_systems(Update, accept_system);
    }
}
