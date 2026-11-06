//! TLS1.3 with an explicitly paired certificate identity, not public web PKI.
//! Application tokens authenticate clients after the pinned TLS handshake.
use rustls::{
    DigitallySignedStruct, SignatureScheme,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime},
};
use std::{sync::Arc, time::Duration};
use subtle::ConstantTimeEq;
use tokio::{net::TcpStream, time::timeout};
use tokio_rustls::{TlsAcceptor, TlsConnector};
use zeroize::Zeroizing;

pub const ALPN: &[u8] = b"usb-most/1";
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidIdentity,
    Random,
    Network,
    Timeout,
    IdentityRejected,
    Peer,
    Protocol,
}
pub fn random<const N: usize>() -> Result<[u8; N], Error> {
    use ring::rand::SecureRandom;
    let mut bytes = [0; N];
    ring::rand::SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| Error::Random)?;
    Ok(bytes)
}
pub struct Identity {
    certificate: Vec<u8>,
    private: Zeroizing<Vec<u8>>,
}
impl Identity {
    pub fn generate() -> Result<Self, Error> {
        let issued = rcgen::generate_simple_self_signed(vec!["usb-most".into()])
            .map_err(|_| Error::InvalidIdentity)?;
        Self::from_parts(
            issued.cert.der().to_vec(),
            issued.signing_key.serialize_der(),
        )
    }
    pub fn from_parts(certificate: Vec<u8>, private: Vec<u8>) -> Result<Self, Error> {
        let identity = Self {
            certificate,
            private: Zeroizing::new(private),
        };
        if identity.certificate.is_empty()
            || identity.certificate.len() > 16384
            || identity.private.is_empty()
            || identity.private.len() > 8192
        {
            return Err(Error::InvalidIdentity);
        }
        identity.server_config()?;
        Ok(identity)
    }
    pub fn pin(&self) -> String {
        crate::config::encode_pin(&self.certificate)
    }
    pub fn certificate(&self) -> &[u8] {
        &self.certificate
    }
    /// Caller must store only in an OS-protected private file or encrypted vault.
    pub fn private_key(&self) -> Zeroizing<Vec<u8>> {
        Zeroizing::new(self.private.to_vec())
    }
    pub fn server_config(&self) -> Result<Arc<rustls::ServerConfig>, Error> {
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(self.private.to_vec()));
        let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|_| Error::InvalidIdentity)?
        .with_no_client_auth()
        .with_single_cert(vec![CertificateDer::from(self.certificate.clone())], key)
        .map_err(|_| Error::InvalidIdentity)?;
        config.alpn_protocols = vec![ALPN.to_vec()];
        config.max_early_data_size = 0;
        config.send_tls13_tickets = 0;
        Ok(Arc::new(config))
    }
}
#[derive(Debug)]
struct Pinned {
    pin: [u8; 32],
    provider: Arc<rustls::crypto::CryptoProvider>,
}
impl ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        end: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if end.len() > 16384 {
            return Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::ApplicationVerificationFailure,
            ));
        }
        let digest = ring::digest::digest(&ring::digest::SHA256, end.as_ref());
        if !bool::from(self.pin.as_slice().ct_eq(digest.as_ref())) {
            return Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::ApplicationVerificationFailure,
            ));
        }
        // This product pins the exact local identity certificate (SSH-like trust),
        // not a DNS name or a system CA. Handshake signatures are verified below.
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            signature,
            &self.provider.signature_verification_algorithms,
        )
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            signature,
            &self.provider.signature_verification_algorithms,
        )
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}
pub fn client_config(pin: &str) -> Result<Arc<rustls::ClientConfig>, Error> {
    let pin = crate::config::decode_pin(pin).map_err(|_| Error::InvalidIdentity)?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|_| Error::InvalidIdentity)?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(Pinned { pin, provider }))
        .with_no_client_auth();
    config.alpn_protocols = vec![ALPN.to_vec()];
    config.enable_early_data = false;
    Ok(Arc::new(config))
}
pub async fn connect(
    endpoint: &crate::config::Endpoint,
    pin: &str,
) -> Result<tokio_rustls::client::TlsStream<TcpStream>, Error> {
    endpoint.validate().map_err(|_| Error::Protocol)?;
    let config = client_config(pin)?;
    let socket = timeout(
        Duration::from_secs(15),
        TcpStream::connect((endpoint.host.as_str(), endpoint.port)),
    )
    .await
    .map_err(|_| Error::Timeout)?
    .map_err(|_| Error::Network)?;
    socket.set_nodelay(true).map_err(|_| Error::Network)?;
    let stream = timeout(
        Duration::from_secs(15),
        TlsConnector::from(config).connect(
            ServerName::try_from("usb-most").map_err(|_| Error::Protocol)?,
            socket,
        ),
    )
    .await
    .map_err(|_| Error::Timeout)?
    .map_err(client_handshake_error)?;
    if stream.get_ref().1.alpn_protocol() != Some(ALPN) {
        return Err(Error::Protocol);
    }
    Ok(stream)
}
fn client_handshake_error(error: std::io::Error) -> Error {
    // Preserve the TLS verifier's structured result. A changed pinned
    // identity is not an unavailable network; an EOF without a certificate
    // verdict must not be presented as a changed identity either.
    match error
        .get_ref()
        .and_then(|source| source.downcast_ref::<rustls::Error>())
    {
        Some(rustls::Error::InvalidCertificate(_) | rustls::Error::NoCertificatesPresented) => {
            Error::IdentityRejected
        }
        Some(_) => Error::Peer,
        None => Error::Network,
    }
}
pub async fn accept(
    socket: TcpStream,
    config: Arc<rustls::ServerConfig>,
) -> Result<tokio_rustls::server::TlsStream<TcpStream>, Error> {
    socket.set_nodelay(true).map_err(|_| Error::Network)?;
    let stream = timeout(
        Duration::from_secs(15),
        TlsAcceptor::from(config).accept(socket),
    )
    .await
    .map_err(|_| Error::Timeout)?
    .map_err(|_| Error::Peer)?;
    if stream.get_ref().1.alpn_protocol() != Some(ALPN) {
        return Err(Error::Protocol);
    }
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    #[tokio::test]
    async fn handshake_transport_loss_and_wrong_protocol_do_not_claim_changed_identity() {
        for plaintext in [false, true] {
            let pin = Identity::generate().unwrap().pin();
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = crate::config::Endpoint {
                host: "127.0.0.1".into(),
                port: listener.local_addr().unwrap().port(),
            };
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                if plaintext {
                    socket
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                        .await
                        .unwrap();
                    let mut hello = [0; 4096];
                    let _ = timeout(Duration::from_secs(2), socket.read(&mut hello)).await;
                }
            });
            let result = connect(&endpoint, &pin).await;
            assert!(
                matches!(result, Err(e) if e == if plaintext { Error::Peer } else { Error::Network })
            );
            server.await.unwrap();
        }
    }
    #[tokio::test]
    async fn tls13_pinned_identity_roundtrip_and_mismatched_pin_rejection() {
        for good in [true, false] {
            let id = Identity::generate().unwrap();
            let pin = if good {
                id.pin()
            } else {
                Identity::generate().unwrap().pin()
            };
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (socket, _) = listener.accept().await.unwrap();
                let result = accept(socket, id.server_config().unwrap()).await;
                if !good {
                    assert!(result.is_err());
                    return;
                }
                let mut stream = result.unwrap();
                assert_eq!(
                    stream.get_ref().1.protocol_version(),
                    Some(rustls::ProtocolVersion::TLSv1_3)
                );
                let mut bytes = [0; 256];
                stream.read_exact(&mut bytes).await.unwrap();
                assert_eq!(bytes, std::array::from_fn(|n| n as u8));
                stream.write_all(&bytes).await.unwrap();
                stream.shutdown().await.unwrap();
            });
            let result = connect(
                &crate::config::Endpoint {
                    host: "127.0.0.1".into(),
                    port: address.port(),
                },
                &pin,
            )
            .await;
            if good {
                let mut stream = result.unwrap();
                let expected: [u8; 256] = std::array::from_fn(|n| n as u8);
                stream.write_all(&expected).await.unwrap();
                let mut bytes = [0; 256];
                stream.read_exact(&mut bytes).await.unwrap();
                assert_eq!(bytes, expected);
            } else {
                assert!(matches!(result, Err(Error::IdentityRejected)));
            }
            server.await.unwrap();
        }
    }
    #[test]
    fn private_key_is_bound_to_certificate_and_bounded() {
        let a = Identity::generate().unwrap();
        let b = Identity::generate().unwrap();
        assert!(Identity::from_parts(a.certificate().to_vec(), b.private_key().to_vec()).is_err());
        assert!(Identity::from_parts(vec![1; 16385], vec![1]).is_err());
        assert!(Identity::from_parts(vec![], vec![]).is_err());
        let restored =
            Identity::from_parts(a.certificate().to_vec(), a.private_key().to_vec()).unwrap();
        assert_eq!(a.pin(), restored.pin());
    }
}
