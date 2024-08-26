pub mod server {
    pub mod dtls_server;
    pub mod plugin;
}
pub mod client {
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
