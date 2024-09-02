use std::path::PathBuf;
use rustls::RootCertStore;
use webrtc_dtls::{
    config::{ClientAuthType, Config, ExtendedMasterSecretType}, 
    crypto::Certificate
};
use crate::cert::loader::CertificateLoader;

#[derive(Clone)]
pub enum ServerCertOption {
    GenerateSelfSigned {
        subject_alt_name: String
    },
    Load {
        priv_key_path: PathBuf,
        certificate_path: PathBuf
    }
}

impl ServerCertOption {
    pub fn to_dtls_config(self)
    -> anyhow::Result<Config> {
        let config = match self {
            ServerCertOption::GenerateSelfSigned { 
                subject_alt_name 
            } => {
                let cert = Certificate::generate_self_signed(
                    vec![subject_alt_name]    
                )?;

                Config{
                    certificates: vec![cert],
                    extended_master_secret: ExtendedMasterSecretType::Require,
                    ..Default::default()
                }
            }
            ServerCertOption::Load { 
                priv_key_path, 
                certificate_path 
            } => {
                let loader = CertificateLoader{
                    priv_key_path,
                    certificate_path
                };
                let cert = loader.load_key_and_certificate()?;
                
                let mut cert_store = RootCertStore::empty();
                for c in cert.certificate.iter() {
                    cert_store.add(c.clone())?;
                }

                Config{
                    certificates: vec![cert],
                    extended_master_secret: ExtendedMasterSecretType::Require,
                    client_auth: ClientAuthType::RequireAndVerifyClientCert,
                    client_cas: cert_store,
                    ..Default::default()
                }
            }
        };

        Ok(config)
    }
}
