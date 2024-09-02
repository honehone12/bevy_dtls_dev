use std::{
    fs::File, 
    io::{BufReader, Read}, 
    path::PathBuf
};
use rcgen::KeyPair;
use rustls::pki_types::CertificateDer;
use webrtc_dtls::crypto::{Certificate, CryptoPrivateKey};

pub struct CertificateLoader {
    pub priv_key_path: PathBuf,
    pub certificate_path: PathBuf
}

impl CertificateLoader {
    pub fn load_key(path: PathBuf) 
    -> anyhow::Result<CryptoPrivateKey> {
        let file = File::open(path)?;
        let mut reader = BufReader::new(file);
        
        let mut buf = vec![];
        reader.read_to_end(&mut buf)?;
        let txt = String::from_utf8(buf)?;

        let key_pair = KeyPair::from_pem(txt.as_str())?;
        let priv_key = CryptoPrivateKey::from_key_pair(&key_pair)?;
        Ok(priv_key)
    } 

    pub fn load_certtificate(path: PathBuf)
    -> anyhow::Result<Vec<CertificateDer<'static>>> {
        let file = File::open(path)?;
        let mut reader = BufReader::new(file);

        let cert = rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<CertificateDer<'static>>, _>>()?;
        Ok(cert)
    }

    pub fn load_key_and_certificate(self) 
    -> anyhow::Result<Certificate> {
        let private_key = Self::load_key(self.priv_key_path)?;
        let certificate = Self::load_certtificate(self.certificate_path)?;

        Ok(Certificate{
            certificate,
            private_key
        })
    }
}
