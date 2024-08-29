use bevy::prelude::*;
use super::dtls_client::*;

pub struct DtlsClientPlugin {
    pub server_addr: &'static str,
    pub client_addr: &'static str,
    pub server_name: &'static str,
    pub buf_size: usize
}

impl Plugin for DtlsClientPlugin {
    fn build(&self, app: &mut App) {
        let mut dtls_client = match DtlsClient::new(self.buf_size) {
            Ok(c) => c,
            Err(e) => panic!("{e}")
        };

        if let Err(e) = dtls_client.start(DtlsClientConfig{ 
            server_addr: self.server_addr, 
            client_addr: self.client_addr, 
            server_name: self.server_name 
        }) {
            panic!("{e}")
        }

        app.insert_resource(dtls_client);
    }
}
