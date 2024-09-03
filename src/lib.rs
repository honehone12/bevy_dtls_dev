pub mod cert {
    pub mod loader;
}
pub mod server {
    pub mod cert_option;
    pub mod dtls_server;
    pub mod plugin;
}
pub mod client {
    pub mod cert_option;
    pub mod dtls_client;
    pub mod plugin;
}

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        panic!("gas panic!");
    }
}
