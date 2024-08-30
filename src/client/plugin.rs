use bevy::prelude::*;
use super::dtls_client::*;

pub struct DtlsClientPlugin {
    pub buf_size: usize
}

impl Plugin for DtlsClientPlugin {
    fn build(&self, app: &mut App) {
        let dtls_client = match DtlsClient::new(self.buf_size) {
            Ok(c) => c,
            Err(e) => panic!("{e}")
        };

        app.insert_resource(dtls_client);
    }
}
