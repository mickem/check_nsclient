//! Connecting to an NRPE server and running one query against it.

use crate::debug;
use crate::nrpe::packet::{
    self, MAX_PAYLOAD_LENGTH, TYPE_MORE_RESPONSE, TYPE_QUERY, TYPE_RESPONSE, read_packet,
};
use anyhow::{Context, anyhow, bail};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};
use rustls_platform_verifier::Verifier;
use std::io::ErrorKind;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

/// The result code a query carries. The field means nothing in a request;
/// `check_nrpe` fills it with UNKNOWN and so do we.
const QUERY_RESULT_CODE: i16 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtocolVersion {
    V2,
    V4,
}

/// How to secure the connection.
#[derive(Clone, Debug, Default)]
pub struct TlsOptions {
    /// Accept whatever certificate the server presents.
    pub insecure: bool,
    /// Extra CA certificates (PEM) to trust besides the system's.
    pub ca: Option<String>,
    /// Client certificate chain (PEM), for servers that ask for one.
    pub certificate: Option<String>,
    /// Private key (PEM) of `certificate`.
    pub certificate_key: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ConnectionOptions {
    pub host: String,
    pub port: u16,
    /// For the whole exchange: connect, handshake, query and answer.
    pub timeout: Duration,
    pub version: ProtocolVersion,
    /// Buffer size of a version 2 packet; has to match the server's.
    pub payload_length: usize,
    /// `None` talks plain text.
    pub tls: Option<TlsOptions>,
}

/// What the server answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    /// 0 OK, 1 WARNING, 2 CRITICAL, 3 UNKNOWN.
    pub result_code: i16,
    /// Plugin output (`message|perfdata`); may span several lines.
    pub output: String,
}

/// Accepts any server certificate. The handshake signature is still checked,
/// so the peer does hold the key of the certificate it showed -- it is just
/// that nobody vouches for that certificate.
#[derive(Debug)]
struct NoVerification(Arc<CryptoProvider>);

impl ServerCertVerifier for NoVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

fn read_certificates(path: &str) -> anyhow::Result<Vec<CertificateDer<'static>>> {
    let certificates = CertificateDer::pem_file_iter(path)
        .and_then(|certificates| certificates.collect::<Result<Vec<_>, _>>())
        .map_err(|e| anyhow!("failed to read certificates from {path}: {e}"))?;
    if certificates.is_empty() {
        bail!("no certificate found in {path}");
    }
    Ok(certificates)
}

fn tls_config(tls: &TlsOptions) -> anyhow::Result<ClientConfig> {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let verifier: Arc<dyn ServerCertVerifier> = if tls.insecure {
        Arc::new(NoVerification(provider.clone()))
    } else {
        let extra_roots = match &tls.ca {
            Some(path) => read_certificates(path)?,
            None => Vec::new(),
        };
        Arc::new(
            Verifier::new_with_extra_roots(extra_roots, provider.clone())
                .context("failed to set up certificate verification")?,
        )
    };
    let builder = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(verifier);
    match (&tls.certificate, &tls.certificate_key) {
        (Some(certificate), Some(key)) => {
            let chain = read_certificates(certificate)?;
            let key = PrivateKeyDer::from_pem_file(key)
                .map_err(|e| anyhow!("failed to read the private key from {key}: {e}"))?;
            builder
                .with_client_auth_cert(chain, key)
                .context("the client certificate and its key cannot be used")
        }
        (None, None) => Ok(builder.with_no_client_auth()),
        _ => bail!("a client certificate and its key have to be given together"),
    }
}

/// Send `request` and collect the answer, following the NSClient++ "more
/// packets follow" chain to its end.
async fn exchange<S>(
    mut stream: S,
    request: &[u8],
    payload_length: usize,
) -> anyhow::Result<Response>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    stream.write_all(request).await?;
    stream.flush().await?;
    debug::log(2, format!("Sent a {} byte query", request.len()));

    let mut output = Vec::new();
    let mut packets = 0;
    loop {
        let packet = read_packet(&mut stream, payload_length)
            .await
            .map_err(|e| {
                if packets == 0 {
                    explain_no_answer(e)
                } else {
                    e
                }
            })?;
        packets += 1;
        debug::log(
            2,
            format!(
                "Received packet {packets}: version {}, type {}, result {}, {} bytes",
                packet.version,
                packet.packet_type,
                packet.result_code,
                packet.payload.len()
            ),
        );
        output.extend_from_slice(&packet.payload);
        if output.len() > MAX_PAYLOAD_LENGTH {
            bail!("the answer is longer than {MAX_PAYLOAD_LENGTH} bytes");
        }
        match packet.packet_type {
            TYPE_MORE_RESPONSE => continue,
            TYPE_RESPONSE => {
                return Ok(Response {
                    result_code: packet.result_code,
                    output: String::from_utf8_lossy(&output).into_owned(),
                });
            }
            other => bail!("the answer is not a response (packet type {other})"),
        }
    }
}

/// A server that does not like a connection says so by closing it. Replace the
/// bare "unexpected end of file" that leaves us with by what is usually behind
/// it. Only an error straight from the socket qualifies: one that already
/// carries an explanation keeps it.
fn explain_no_answer(error: anyhow::Error) -> anyhow::Error {
    let closed = error.chain().len() == 1
        && error.downcast_ref::<std::io::Error>().is_some_and(|e| {
            matches!(
                e.kind(),
                ErrorKind::UnexpectedEof
                    | ErrorKind::ConnectionReset
                    | ErrorKind::ConnectionAborted
            )
        });
    // With TLS 1.3 a server that wants a client certificate lets the handshake
    // through and refuses afterwards, so this arrives with the first read.
    if format!("{error:#}").contains("CertificateRequired") {
        return anyhow!(
            "the server demands a client certificate (--certificate and --certificate-key)"
        );
    }
    if closed {
        anyhow!(
            "the server closed the connection without answering. It does so when this host is \
             not in its `allowed hosts`, when only one side uses TLS (--no-tls), and when the \
             payload length differs (--payload-length)"
        )
    } else {
        error
    }
}

/// Run one query: `payload` is the command, with any arguments appended after
/// `!` separators.
pub async fn query(options: &ConnectionOptions, payload: &str) -> anyhow::Result<Response> {
    let request = match options.version {
        ProtocolVersion::V2 => packet::encode_v2(
            TYPE_QUERY,
            QUERY_RESULT_CODE,
            payload.as_bytes(),
            options.payload_length,
        )
        .context("use --protocol-version 4 or a larger --payload-length for a longer query")?,
        ProtocolVersion::V4 => {
            packet::encode_v4(TYPE_QUERY, QUERY_RESULT_CODE, payload.as_bytes())?
        }
    };
    // Built before connecting, so a bad certificate file is reported as such
    // and not as whatever the server makes of an abandoned connection.
    let tls = match &options.tls {
        Some(tls) => {
            let config = tls_config(tls)?;
            let name = ServerName::try_from(options.host.clone())
                .with_context(|| format!("'{}' is not a valid host name", options.host))?;
            Some((TlsConnector::from(Arc::new(config)), name))
        }
        None => None,
    };
    let address = format!("{}:{}", options.host, options.port);

    let conversation = async {
        debug::log(1, format!("Connecting to {address}"));
        let tcp = TcpStream::connect((options.host.as_str(), options.port))
            .await
            .with_context(|| format!("failed to connect to {address}"))?;
        // One small request, one answer: nothing to gain from batching writes.
        let _ = tcp.set_nodelay(true);
        match tls {
            Some((connector, name)) => {
                let stream = connector.connect(name, tcp).await.with_context(|| {
                    format!(
                        "TLS handshake with {address} failed (--no-tls talks to a server without \
                         TLS, --insecure accepts a certificate that cannot be verified; legacy \
                         servers that only offer anonymous ADH ciphers are not supported)"
                    )
                })?;
                debug::log(1, "TLS session established");
                exchange(stream, &request, options.payload_length).await
            }
            None => exchange(tcp, &request, options.payload_length).await,
        }
    };
    match tokio::time::timeout(options.timeout, conversation).await {
        Ok(result) => result,
        Err(_) => bail!(
            "no answer from {address} within {} seconds",
            options.timeout.as_secs()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nrpe::packet::{Packet, encode_v2, encode_v4};
    use rustls::RootCertStore;
    use rustls::server::WebPkiClientVerifier;
    use rustls::{ServerConfig, pki_types::PrivatePkcs8KeyDer};
    use std::io::Write;
    use tempfile::NamedTempFile;
    use tokio::net::TcpListener;
    use tokio_rustls::TlsAcceptor;

    /// What the fake server does with the query it received.
    type Handler = fn(Packet) -> Vec<Vec<u8>>;

    /// Echo the query back in a single packet of the version it came in.
    fn echo(query: Packet) -> Vec<Vec<u8>> {
        let text = [b"OK: ".as_slice(), &query.payload].concat();
        vec![match query.version {
            2 => encode_v2(TYPE_RESPONSE, 0, &text, 1024).unwrap(),
            _ => encode_v4(TYPE_RESPONSE, 0, &text).unwrap(),
        }]
    }

    async fn serve<S: AsyncRead + AsyncWrite + Unpin>(mut stream: S, handler: Handler) {
        let Ok(query) = read_packet(&mut stream, 1024).await else {
            return;
        };
        assert_eq!(query.packet_type, TYPE_QUERY);
        for packet in handler(query) {
            stream.write_all(&packet).await.unwrap();
        }
        let _ = stream.shutdown().await;
    }

    /// A one-connection NRPE server on a free local port.
    async fn server(acceptor: Option<TlsAcceptor>, handler: Handler) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            match acceptor {
                Some(acceptor) => {
                    if let Ok(stream) = acceptor.accept(tcp).await {
                        serve(stream, handler).await
                    }
                }
                None => serve(tcp, handler).await,
            }
        });
        port
    }

    fn options(port: u16, version: ProtocolVersion, tls: Option<TlsOptions>) -> ConnectionOptions {
        ConnectionOptions {
            host: "localhost".into(),
            port,
            timeout: Duration::from_secs(10),
            version,
            payload_length: 1024,
            tls,
        }
    }

    /// A self-signed certificate for `localhost`, as PEM files and in the form
    /// rustls takes.
    struct Identity {
        certificate: CertificateDer<'static>,
        key: PrivateKeyDer<'static>,
        certificate_file: NamedTempFile,
        key_file: NamedTempFile,
    }

    fn pem_file(content: &str) -> NamedTempFile {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(content.as_bytes()).unwrap();
        file
    }

    fn path(file: &NamedTempFile) -> Option<String> {
        Some(file.path().to_str().unwrap().to_string())
    }

    fn identity() -> Identity {
        let generated = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
        Identity {
            certificate: generated.cert.der().clone(),
            key: PrivatePkcs8KeyDer::from(generated.key_pair.serialize_der()).into(),
            certificate_file: pem_file(&generated.cert.pem()),
            key_file: pem_file(&generated.key_pair.serialize_pem()),
        }
    }

    /// A TLS acceptor presenting `server`, which insists on the certificate of
    /// `client` when one is given.
    fn acceptor(server: &Identity, client: Option<&Identity>) -> TlsAcceptor {
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let builder = ServerConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .unwrap();
        let builder = match client {
            Some(client) => {
                let mut roots = RootCertStore::empty();
                roots.add(client.certificate.clone()).unwrap();
                builder.with_client_cert_verifier(
                    WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider)
                        .build()
                        .unwrap(),
                )
            }
            None => builder.with_no_client_auth(),
        };
        let config = builder
            .with_single_cert(vec![server.certificate.clone()], server.key.clone_key())
            .unwrap();
        TlsAcceptor::from(Arc::new(config))
    }

    #[tokio::test]
    async fn plain_text_v2_query() {
        let port = server(None, echo).await;
        let response = query(&options(port, ProtocolVersion::V2, None), "check_cpu!80!90")
            .await
            .unwrap();
        assert_eq!(
            response,
            Response {
                result_code: 0,
                output: "OK: check_cpu!80!90".into()
            }
        );
    }

    #[tokio::test]
    async fn plain_text_v4_query() {
        let port = server(None, echo).await;
        let long = "x".repeat(3000);
        let response = query(&options(port, ProtocolVersion::V4, None), &long)
            .await
            .unwrap();
        assert_eq!(response.output, format!("OK: {long}"));
    }

    #[tokio::test]
    async fn joins_an_extended_response() {
        let port = server(None, |_| {
            vec![
                encode_v2(TYPE_MORE_RESPONSE, 2, b"CRITICAL: one, ", 1024).unwrap(),
                encode_v2(TYPE_MORE_RESPONSE, 2, b"two, ", 1024).unwrap(),
                encode_v2(TYPE_RESPONSE, 2, b"three|'n'=3", 1024).unwrap(),
            ]
        })
        .await;
        let response = query(&options(port, ProtocolVersion::V2, None), "check_long")
            .await
            .unwrap();
        assert_eq!(
            response,
            Response {
                result_code: 2,
                output: "CRITICAL: one, two, three|'n'=3".into()
            }
        );
    }

    #[tokio::test]
    async fn rejects_a_query_that_does_not_fit_v2() {
        // Nothing listens here, and nothing has to: this fails before connecting.
        let err = query(&options(1, ProtocolVersion::V2, None), &"x".repeat(1024))
            .await
            .unwrap_err();
        let err = format!("{err:#}");
        assert!(err.contains("--protocol-version 4"), "{err}");
        assert!(err.contains("at most 1023"), "{err}");
    }

    #[tokio::test]
    async fn explains_a_closed_connection() {
        // The server hangs up without a word, as NSClient++ does to a host that
        // is not allowed.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let _ = read_packet(&mut { tcp }, 1024).await;
        });
        let err = query(&options(port, ProtocolVersion::V2, None), "check_cpu")
            .await
            .unwrap_err();
        let err = format!("{err:#}");
        assert!(
            err.contains("closed the connection without answering"),
            "{err}"
        );
        assert!(err.contains("allowed hosts"), "{err}");
    }

    #[tokio::test]
    async fn reports_a_refused_connection() {
        // Bind and drop to get a port that is closed right now.
        let port = {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            listener.local_addr().unwrap().port()
        };
        let err = query(&options(port, ProtocolVersion::V2, None), "check_cpu")
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .starts_with("failed to connect to localhost:"),
            "{err:#}"
        );
    }

    #[tokio::test]
    async fn times_out_on_a_silent_server() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (_tcp, _) = listener.accept().await.unwrap();
            tokio::time::sleep(Duration::from_secs(30)).await;
        });
        let mut options = options(port, ProtocolVersion::V2, None);
        options.timeout = Duration::from_millis(200);
        let err = query(&options, "check_cpu").await.unwrap_err();
        assert!(
            err.to_string().starts_with("no answer from localhost:"),
            "{err:#}"
        );
    }

    #[tokio::test]
    async fn tls_query_with_a_trusted_certificate() {
        let identity = identity();
        let port = server(Some(acceptor(&identity, None)), echo).await;
        let tls = TlsOptions {
            ca: path(&identity.certificate_file),
            ..TlsOptions::default()
        };
        let response = query(&options(port, ProtocolVersion::V2, Some(tls)), "check_cpu")
            .await
            .unwrap();
        assert_eq!(response.output, "OK: check_cpu");
    }

    #[tokio::test]
    async fn tls_rejects_an_unknown_certificate() {
        let identity = identity();
        let port = server(Some(acceptor(&identity, None)), echo).await;
        let err = query(
            &options(port, ProtocolVersion::V2, Some(TlsOptions::default())),
            "check_cpu",
        )
        .await
        .unwrap_err();
        let err = format!("{err:#}");
        assert!(err.starts_with("TLS handshake with localhost:"), "{err}");
        assert!(err.contains("--insecure"), "{err}");
    }

    #[tokio::test]
    async fn insecure_accepts_an_unknown_certificate() {
        let identity = identity();
        let port = server(Some(acceptor(&identity, None)), echo).await;
        let tls = TlsOptions {
            insecure: true,
            ..TlsOptions::default()
        };
        let response = query(&options(port, ProtocolVersion::V4, Some(tls)), "check_cpu")
            .await
            .unwrap();
        assert_eq!(response.output, "OK: check_cpu");
    }

    #[tokio::test]
    async fn tls_presents_a_client_certificate() {
        let server_identity = identity();
        let client_identity = identity();
        let port = server(
            Some(acceptor(&server_identity, Some(&client_identity))),
            echo,
        )
        .await;
        let tls = TlsOptions {
            insecure: false,
            ca: path(&server_identity.certificate_file),
            certificate: path(&client_identity.certificate_file),
            certificate_key: path(&client_identity.key_file),
        };
        let response = query(&options(port, ProtocolVersion::V2, Some(tls)), "check_cpu")
            .await
            .unwrap();
        assert_eq!(response.output, "OK: check_cpu");
    }

    #[tokio::test]
    async fn tls_without_the_demanded_client_certificate_fails() {
        let server_identity = identity();
        let client_identity = identity();
        let port = server(
            Some(acceptor(&server_identity, Some(&client_identity))),
            echo,
        )
        .await;
        let tls = TlsOptions {
            ca: path(&server_identity.certificate_file),
            ..TlsOptions::default()
        };
        assert!(
            query(&options(port, ProtocolVersion::V2, Some(tls)), "check_cpu")
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn reports_unusable_certificate_files() {
        let empty = pem_file("");
        let tls = TlsOptions {
            ca: path(&empty),
            ..TlsOptions::default()
        };
        let err = query(&options(1, ProtocolVersion::V2, Some(tls)), "x")
            .await
            .unwrap_err();
        assert!(
            err.to_string().starts_with("no certificate found in "),
            "{err:#}"
        );

        let tls = TlsOptions {
            ca: Some("/definitely/not/here.pem".into()),
            ..TlsOptions::default()
        };
        let err = query(&options(1, ProtocolVersion::V2, Some(tls)), "x")
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .starts_with("failed to read certificates from /definitely/not/here.pem"),
            "{err:#}"
        );

        let identity = identity();
        let tls = TlsOptions {
            insecure: true,
            certificate: path(&identity.certificate_file),
            ..TlsOptions::default()
        };
        let err = query(&options(1, ProtocolVersion::V2, Some(tls)), "x")
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("have to be given together"),
            "{err:#}"
        );
    }
}
