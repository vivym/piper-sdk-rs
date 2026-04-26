//! GS-USB bridge v2 client.
//!
//! The bridge is a non-realtime debug/record/replay transport over UnixStream
//! or TCP. It is intentionally separate from the realtime CAN control path.

pub mod protocol;

use crate::{CanDeviceError, CanDeviceErrorKind};
use protocol::{
    ClientRequest, ServerMessage, ServerResponse, decode_server_message, encode_client_request,
    read_framed, write_framed,
};
use rustls::pki_types::{PrivateKeyDer, ServerName};
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};
use std::collections::VecDeque;
use std::fs;
use std::io::Cursor;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

pub use protocol::{BridgeEvent, BridgeRole, BridgeStatus, ErrorCode, SessionToken};

#[derive(Debug)]
pub enum BridgeError {
    Io(std::io::Error),
    Protocol(protocol::ProtocolError),
    Config(String),
    Tls(String),
    Remote { code: ErrorCode, message: String },
    UnexpectedMessage(&'static str),
    NotConnected,
}

impl std::fmt::Display for BridgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(err) => write!(f, "{err}"),
            Self::Protocol(err) => write!(f, "{err}"),
            Self::Config(message) => write!(f, "{message}"),
            Self::Tls(message) => write!(f, "{message}"),
            Self::Remote { code, message } => write!(f, "remote {code:?}: {message}"),
            Self::UnexpectedMessage(message) => write!(f, "{message}"),
            Self::NotConnected => write!(f, "bridge client is not connected"),
        }
    }
}

impl std::error::Error for BridgeError {}

impl From<std::io::Error> for BridgeError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<protocol::ProtocolError> for BridgeError {
    fn from(value: protocol::ProtocolError) -> Self {
        Self::Protocol(value)
    }
}

impl From<rustls::Error> for BridgeError {
    fn from(value: rustls::Error) -> Self {
        Self::Tls(value.to_string())
    }
}

impl BridgeError {
    pub fn as_can_device_error(&self) -> Option<CanDeviceError> {
        match self {
            Self::Remote { code, message } => {
                let kind = match code {
                    ErrorCode::DeviceNotFound => CanDeviceErrorKind::NotFound,
                    ErrorCode::DeviceBusy | ErrorCode::Busy => CanDeviceErrorKind::Busy,
                    ErrorCode::InvalidMessage | ErrorCode::ProtocolError => {
                        CanDeviceErrorKind::InvalidResponse
                    },
                    ErrorCode::PermissionDenied => CanDeviceErrorKind::Backend,
                    ErrorCode::Timeout => CanDeviceErrorKind::Backend,
                    ErrorCode::DeviceError | ErrorCode::Unknown | ErrorCode::NotConnected => {
                        CanDeviceErrorKind::Backend
                    },
                };
                Some(CanDeviceError::new(kind, message.clone()))
            },
            Self::Config(message) | Self::Tls(message) => Some(CanDeviceError::new(
                CanDeviceErrorKind::Backend,
                message.clone(),
            )),
            _ => None,
        }
    }
}

pub type BridgeResult<T> = Result<T, BridgeError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BridgeEndpoint {
    Unix(PathBuf),
    TcpTls(SocketAddr),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeTlsClientConfig {
    pub ca_cert_pem: PathBuf,
    pub client_cert_pem: PathBuf,
    pub client_key_pem: PathBuf,
    pub server_name: String,
}

#[derive(Debug, Clone)]
pub struct BridgeClientOptions {
    pub session_token: SessionToken,
    pub filters: Vec<protocol::CanIdFilter>,
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
    pub tcp_tls: Option<BridgeTlsClientConfig>,
}

impl Default for BridgeClientOptions {
    fn default() -> Self {
        Self {
            session_token: SessionToken::random(),
            filters: Vec::new(),
            connect_timeout: Duration::from_secs(5),
            request_timeout: Duration::from_millis(100),
            tcp_tls: None,
        }
    }
}

enum BridgeStream {
    #[cfg(unix)]
    Unix(UnixStream),
    TcpTls(Box<StreamOwned<ClientConnection, TcpStream>>),
}

impl BridgeStream {
    fn connect(endpoint: &BridgeEndpoint, options: &BridgeClientOptions) -> BridgeResult<Self> {
        match endpoint {
            #[cfg(unix)]
            BridgeEndpoint::Unix(path) => Ok(Self::Unix(UnixStream::connect(path)?)),
            BridgeEndpoint::TcpTls(addr) => Self::connect_tcp_tls(*addr, options),
            #[cfg(not(unix))]
            BridgeEndpoint::Unix(_) => Err(BridgeError::UnexpectedMessage(
                "unix bridge endpoints are not supported on this platform",
            )),
        }
    }

    fn connect_tcp_tls(addr: SocketAddr, options: &BridgeClientOptions) -> BridgeResult<Self> {
        let tls = options.tcp_tls.as_ref().ok_or_else(|| {
            BridgeError::Config(
                "tcp tls endpoint requires BridgeClientOptions::tcp_tls client credentials"
                    .to_string(),
            )
        })?;

        let tcp_stream = TcpStream::connect_timeout(&addr, options.connect_timeout)?;
        tcp_stream.set_nodelay(true)?;
        tcp_stream.set_read_timeout(Some(options.connect_timeout))?;
        tcp_stream.set_write_timeout(Some(options.connect_timeout))?;

        let mut roots = RootCertStore::empty();
        let root_certs = load_cert_chain(&tls.ca_cert_pem)?;
        let (added, _) = roots.add_parsable_certificates(root_certs);
        if added == 0 {
            return Err(BridgeError::Config(format!(
                "no valid CA certificates found in {}",
                tls.ca_cert_pem.display()
            )));
        }

        let client_certs = load_cert_chain(&tls.client_cert_pem)?;
        let client_key = load_private_key(&tls.client_key_pem)?;
        let config = ClientConfig::builder()
            .with_root_certificates(roots)
            .with_client_auth_cert(client_certs, client_key)
            .map_err(|err| BridgeError::Tls(format!("failed to configure client TLS: {err}")))?;
        let server_name = ServerName::try_from(tls.server_name.clone()).map_err(|_| {
            BridgeError::Config(format!("invalid TLS server name: {}", tls.server_name))
        })?;
        let connection = ClientConnection::new(Arc::new(config), server_name)
            .map_err(|err| BridgeError::Tls(format!("failed to create TLS connection: {err}")))?;
        let mut tls_stream = StreamOwned::new(connection, tcp_stream);
        while tls_stream.conn.is_handshaking() {
            tls_stream
                .conn
                .complete_io(&mut tls_stream.sock)
                .map_err(|err| BridgeError::Tls(format!("TLS handshake failed: {err}")))?;
        }
        Ok(Self::TcpTls(Box::new(tls_stream)))
    }

    fn set_read_timeout(&self, timeout: Option<Duration>) -> BridgeResult<()> {
        match self {
            #[cfg(unix)]
            Self::Unix(stream) => stream.set_read_timeout(timeout)?,
            Self::TcpTls(stream) => stream.sock.set_read_timeout(timeout)?,
        }
        Ok(())
    }

    fn set_write_timeout(&self, timeout: Option<Duration>) -> BridgeResult<()> {
        match self {
            #[cfg(unix)]
            Self::Unix(stream) => stream.set_write_timeout(timeout)?,
            Self::TcpTls(stream) => stream.sock.set_write_timeout(timeout)?,
        }
        Ok(())
    }

    fn shutdown(&self) {
        match self {
            #[cfg(unix)]
            Self::Unix(stream) => {
                let _ = stream.shutdown(std::net::Shutdown::Both);
            },
            Self::TcpTls(stream) => {
                let _ = stream.sock.shutdown(std::net::Shutdown::Both);
            },
        }
    }
}

impl Read for BridgeStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            #[cfg(unix)]
            Self::Unix(stream) => stream.read(buf),
            Self::TcpTls(stream) => stream.read(buf),
        }
    }
}

impl Write for BridgeStream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            #[cfg(unix)]
            Self::Unix(stream) => stream.write(buf),
            Self::TcpTls(stream) => stream.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            #[cfg(unix)]
            Self::Unix(stream) => stream.flush(),
            Self::TcpTls(stream) => stream.flush(),
        }
    }
}

pub struct GsUsbBridgeClient {
    stream: BridgeStream,
    endpoint: BridgeEndpoint,
    session_token: SessionToken,
    session_id: u32,
    role_granted: BridgeRole,
    request_timeout: Duration,
    next_request_id: u32,
    event_buffer: VecDeque<BridgeEvent>,
    writer_lease_held: bool,
    connected: bool,
}

impl GsUsbBridgeClient {
    pub fn connect(endpoint: BridgeEndpoint, options: BridgeClientOptions) -> BridgeResult<Self> {
        let mut stream = BridgeStream::connect(&endpoint, &options)?;
        stream.set_read_timeout(Some(options.request_timeout))?;
        stream.set_write_timeout(Some(options.request_timeout))?;

        let hello = ClientRequest::Hello {
            request_id: 1,
            session_token: options.session_token,
            filters: options.filters,
        };
        let encoded = encode_client_request(&hello)?;
        write_framed(&mut stream, &encoded)?;
        let payload = read_framed(&mut stream)?;
        let message = decode_server_message(&payload)?;
        let ServerMessage::Response(response) = message else {
            return Err(BridgeError::UnexpectedMessage(
                "expected hello response during connect",
            ));
        };

        let (session_id, role_granted) = match response {
            ServerResponse::HelloAck {
                request_id: 1,
                session_id,
                role_granted,
            } => (session_id, role_granted),
            ServerResponse::Error {
                request_id: 1,
                code,
                message,
            } => return Err(BridgeError::Remote { code, message }),
            _ => {
                return Err(BridgeError::UnexpectedMessage(
                    "unexpected response during connect",
                ));
            },
        };

        Ok(Self {
            stream,
            endpoint,
            session_token: options.session_token,
            session_id,
            role_granted,
            request_timeout: options.request_timeout,
            next_request_id: 2,
            event_buffer: VecDeque::new(),
            writer_lease_held: false,
            connected: true,
        })
    }

    pub fn endpoint(&self) -> &BridgeEndpoint {
        &self.endpoint
    }

    pub fn session_token(&self) -> SessionToken {
        self.session_token
    }

    pub fn session_id(&self) -> u32 {
        self.session_id
    }

    pub fn role_granted(&self) -> BridgeRole {
        self.role_granted
    }

    pub fn recv_event(&mut self, timeout: Duration) -> BridgeResult<BridgeEvent> {
        self.ensure_connected()?;
        if let Some(event) = self.pop_event() {
            return Ok(event);
        }

        self.stream.set_read_timeout(Some(timeout))?;
        match self.read_server_message() {
            Ok(ServerMessage::Event(event)) => Ok(self.handle_event(event)),
            Ok(ServerMessage::Response(_)) => {
                self.connected = false;
                Err(BridgeError::UnexpectedMessage(
                    "unexpected response while waiting for event",
                ))
            },
            Err(error) => Err(error),
        }
    }

    pub fn get_status(&mut self) -> BridgeResult<BridgeStatus> {
        self.ensure_connected()?;
        let request_id = self.next_request_id();
        self.send_request(ClientRequest::GetStatus { request_id })?;
        match self.wait_for_response(request_id)? {
            ServerResponse::StatusResponse { status, .. } => Ok(status),
            response => Err(self.unexpected_response("status response", response)),
        }
    }

    pub fn set_filters(&mut self, filters: Vec<protocol::CanIdFilter>) -> BridgeResult<()> {
        self.ensure_connected()?;
        let request_id = self.next_request_id();
        self.send_request(ClientRequest::SetFilters {
            request_id,
            filters,
        })?;
        match self.wait_for_response(request_id)? {
            ServerResponse::Ok { .. } => Ok(()),
            response => Err(self.unexpected_response("ok response", response)),
        }
    }

    pub fn set_raw_frame_tap(&mut self, enabled: bool) -> BridgeResult<()> {
        self.ensure_connected()?;
        let request_id = self.next_request_id();
        self.send_request(ClientRequest::SetRawFrameTap {
            request_id,
            enabled,
        })?;
        match self.wait_for_response(request_id)? {
            ServerResponse::Ok { .. } => Ok(()),
            response => Err(self.unexpected_response("ok response", response)),
        }
    }

    pub fn ping(&mut self) -> BridgeResult<()> {
        self.ensure_connected()?;
        let request_id = self.next_request_id();
        self.send_request(ClientRequest::Ping { request_id })?;
        match self.wait_for_response(request_id)? {
            ServerResponse::Ok { .. } => Ok(()),
            response => Err(self.unexpected_response("ok response", response)),
        }
    }

    pub fn acquire_writer_lease(&mut self, timeout: Duration) -> BridgeResult<WriterLease<'_>> {
        self.ensure_connected()?;
        let request_id = self.next_request_id();
        let timeout_ms = timeout.as_millis().min(u32::MAX as u128) as u32;
        self.send_request(ClientRequest::AcquireWriterLease {
            request_id,
            timeout_ms,
        })?;
        match self.wait_for_response(request_id)? {
            ServerResponse::LeaseGranted { .. } => {
                self.writer_lease_held = true;
                Ok(WriterLease {
                    client: self,
                    released: false,
                })
            },
            ServerResponse::LeaseDenied {
                holder_session_id, ..
            } => Err(BridgeError::Remote {
                code: ErrorCode::Busy,
                message: match holder_session_id {
                    Some(holder) => format!("writer lease held by session {holder}"),
                    None => "writer lease is busy".to_string(),
                },
            }),
            response => Err(self.unexpected_response("lease response", response)),
        }
    }

    pub fn disconnect(&mut self) {
        self.connected = false;
        self.writer_lease_held = false;
        self.stream.shutdown();
    }

    fn release_writer_lease_internal(&mut self) -> BridgeResult<()> {
        if !self.writer_lease_held || !self.connected {
            self.writer_lease_held = false;
            return Ok(());
        }

        let request_id = self.next_request_id();
        self.send_request(ClientRequest::ReleaseWriterLease { request_id })?;
        match self.wait_for_response(request_id)? {
            ServerResponse::Ok { .. } => {
                self.writer_lease_held = false;
                Ok(())
            },
            response => Err(self.unexpected_response("ok response", response)),
        }
    }

    fn send_frame_internal(&mut self, frame: crate::PiperFrame) -> BridgeResult<()> {
        self.ensure_connected()?;
        if !self.writer_lease_held {
            return Err(BridgeError::Remote {
                code: ErrorCode::PermissionDenied,
                message: "writer lease required".to_string(),
            });
        }
        let request_id = self.next_request_id();
        self.send_request(ClientRequest::SendFrame { request_id, frame })?;
        match self.wait_for_response(request_id)? {
            ServerResponse::Ok { .. } => Ok(()),
            response => Err(self.unexpected_response("ok response", response)),
        }
    }

    fn send_request(&mut self, request: ClientRequest) -> BridgeResult<()> {
        let encoded = encode_client_request(&request)?;
        self.stream.set_write_timeout(Some(self.request_timeout))?;
        write_framed(&mut self.stream, &encoded)?;
        Ok(())
    }

    fn wait_for_response(&mut self, request_id: u32) -> BridgeResult<ServerResponse> {
        self.stream.set_read_timeout(Some(self.request_timeout))?;
        loop {
            match self.read_server_message()? {
                ServerMessage::Event(event) => {
                    self.handle_and_buffer_event(event);
                },
                ServerMessage::Response(response) => {
                    let response_id = response_request_id(&response);
                    if response_id != request_id {
                        self.connected = false;
                        return Err(BridgeError::UnexpectedMessage(
                            "mismatched response request id",
                        ));
                    }
                    if let ServerResponse::Error { code, message, .. } = &response {
                        if *code == ErrorCode::PermissionDenied {
                            self.writer_lease_held = false;
                        }
                        return Err(BridgeError::Remote {
                            code: *code,
                            message: message.clone(),
                        });
                    }
                    return Ok(response);
                },
            }
        }
    }

    fn read_server_message(&mut self) -> BridgeResult<ServerMessage> {
        let payload = read_framed(&mut self.stream)?;
        Ok(decode_server_message(&payload)?)
    }

    fn handle_and_buffer_event(&mut self, event: BridgeEvent) {
        let event = self.handle_event(event);
        self.event_buffer.push_back(event);
    }

    fn handle_event(&mut self, event: BridgeEvent) -> BridgeEvent {
        match event {
            BridgeEvent::LeaseRevoked => {
                self.writer_lease_held = false;
                BridgeEvent::LeaseRevoked
            },
            BridgeEvent::SessionReplaced => {
                self.writer_lease_held = false;
                self.connected = false;
                BridgeEvent::SessionReplaced
            },
            other => other,
        }
    }

    fn pop_event(&mut self) -> Option<BridgeEvent> {
        self.event_buffer.pop_front()
    }

    fn next_request_id(&mut self) -> u32 {
        let request_id = self.next_request_id;
        self.next_request_id = self.next_request_id.wrapping_add(1).max(1);
        request_id
    }

    fn ensure_connected(&self) -> BridgeResult<()> {
        if self.connected {
            Ok(())
        } else {
            Err(BridgeError::NotConnected)
        }
    }

    fn unexpected_response(
        &mut self,
        expected: &'static str,
        _response: ServerResponse,
    ) -> BridgeError {
        self.connected = false;
        BridgeError::UnexpectedMessage(expected)
    }
}

impl Drop for GsUsbBridgeClient {
    fn drop(&mut self) {
        let _ = self.release_writer_lease_internal();
        self.disconnect();
    }
}

pub struct WriterLease<'a> {
    client: &'a mut GsUsbBridgeClient,
    released: bool,
}

impl<'a> WriterLease<'a> {
    pub fn send_frame(&mut self, frame: crate::PiperFrame) -> BridgeResult<()> {
        self.client.send_frame_internal(frame)
    }

    pub fn release(mut self) -> BridgeResult<()> {
        let result = self.client.release_writer_lease_internal();
        self.released = true;
        result
    }
}

impl Drop for WriterLease<'_> {
    fn drop(&mut self) {
        if !self.released {
            let _ = self.client.release_writer_lease_internal();
            self.released = true;
        }
    }
}

fn response_request_id(response: &ServerResponse) -> u32 {
    match response {
        ServerResponse::HelloAck { request_id, .. }
        | ServerResponse::Ok { request_id }
        | ServerResponse::Error { request_id, .. }
        | ServerResponse::StatusResponse { request_id, .. }
        | ServerResponse::LeaseGranted { request_id, .. }
        | ServerResponse::LeaseDenied { request_id, .. } => *request_id,
    }
}

fn load_cert_chain(
    path: &std::path::Path,
) -> BridgeResult<Vec<rustls::pki_types::CertificateDer<'static>>> {
    let bytes = fs::read(path).map_err(BridgeError::Io)?;
    let mut cursor = Cursor::new(bytes);
    rustls_pemfile::certs(&mut cursor)
        .collect::<Result<Vec<_>, _>>()
        .map_err(BridgeError::Io)
}

fn load_private_key(path: &std::path::Path) -> BridgeResult<PrivateKeyDer<'static>> {
    let bytes = fs::read(path).map_err(BridgeError::Io)?;
    let mut cursor = Cursor::new(bytes);
    rustls_pemfile::private_key(&mut cursor)
        .map_err(BridgeError::Io)?
        .ok_or_else(|| BridgeError::Config(format!("no private key found in {}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PiperFrame;
    use protocol::{CanIdFilter, SESSION_TOKEN_LEN, decode_client_request, encode_server_message};
    use rcgen::{BasicConstraints, CertificateParams, IsCa, KeyPair};
    use rustls::server::{ServerConfig, ServerConnection, WebPkiClientVerifier};
    use std::fs;
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread;

    static NEXT_TLS_FIXTURE_ID: AtomicU64 = AtomicU64::new(1);

    struct TestTlsFixture {
        dir: PathBuf,
        server_config: Arc<ServerConfig>,
        ca_cert_pem: PathBuf,
        client_cert_pem: PathBuf,
        client_key_pem: PathBuf,
    }

    impl TestTlsFixture {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "piper-bridge-client-tls-test-{}-{}",
                std::process::id(),
                NEXT_TLS_FIXTURE_ID.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&dir).unwrap();

            let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
            ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
            let ca_key = KeyPair::generate().unwrap();
            let ca_cert = ca_params.self_signed(&ca_key).unwrap();

            let server_key = KeyPair::generate().unwrap();
            let server_params = CertificateParams::new(vec!["localhost".to_string()]).unwrap();
            let server_cert = server_params.signed_by(&server_key, &ca_cert, &ca_key).unwrap();

            let client_key = KeyPair::generate().unwrap();
            let client_params = CertificateParams::new(vec!["bridge-client".to_string()]).unwrap();
            let client_cert = client_params.signed_by(&client_key, &ca_cert, &ca_key).unwrap();

            let ca_cert_pem = dir.join("ca.pem");
            let server_cert_pem = dir.join("server-cert.pem");
            let server_key_pem = dir.join("server-key.pem");
            let client_cert_pem = dir.join("client-cert.pem");
            let client_key_pem = dir.join("client-key.pem");

            fs::write(&ca_cert_pem, ca_cert.pem()).unwrap();
            fs::write(&server_cert_pem, server_cert.pem()).unwrap();
            fs::write(&server_key_pem, server_key.serialize_pem()).unwrap();
            fs::write(&client_cert_pem, client_cert.pem()).unwrap();
            fs::write(&client_key_pem, client_key.serialize_pem()).unwrap();

            let mut roots = RootCertStore::empty();
            let (added, _) =
                roots.add_parsable_certificates(load_cert_chain(&ca_cert_pem).unwrap());
            assert!(added > 0);

            let verifier = WebPkiClientVerifier::builder(Arc::new(roots)).build().unwrap();
            let server_config = ServerConfig::builder()
                .with_client_cert_verifier(verifier)
                .with_single_cert(
                    load_cert_chain(&server_cert_pem).unwrap(),
                    load_private_key(&server_key_pem).unwrap(),
                )
                .unwrap();

            Self {
                dir,
                server_config: Arc::new(server_config),
                ca_cert_pem,
                client_cert_pem,
                client_key_pem,
            }
        }

        fn client_tls_config(&self) -> BridgeTlsClientConfig {
            BridgeTlsClientConfig {
                ca_cert_pem: self.ca_cert_pem.clone(),
                client_cert_pem: self.client_cert_pem.clone(),
                client_key_pem: self.client_key_pem.clone(),
                server_name: "localhost".to_string(),
            }
        }

        fn spawn_server<F>(&self, handler: F) -> std::net::SocketAddr
        where
            F: FnOnce(StreamOwned<ServerConnection, TcpStream>) + Send + 'static,
        {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let server_config = self.server_config.clone();
            thread::spawn(move || {
                let (tcp_stream, _) = listener.accept().unwrap();
                tcp_stream.set_nodelay(true).unwrap();
                tcp_stream.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
                tcp_stream.set_write_timeout(Some(Duration::from_secs(1))).unwrap();
                let connection = ServerConnection::new(server_config).unwrap();
                let mut stream = StreamOwned::new(connection, tcp_stream);
                while stream.conn.is_handshaking() {
                    stream.conn.complete_io(&mut stream.sock).unwrap();
                }
                handler(stream);
            });
            addr
        }
    }

    impl Drop for TestTlsFixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn test_connect_roundtrip_tcp_tls() {
        let tls = TestTlsFixture::new();
        let addr = tls.spawn_server(move |mut stream| {
            let payload = read_framed(&mut stream).unwrap();
            let request = decode_client_request(&payload).unwrap();
            match request {
                ClientRequest::Hello {
                    request_id,
                    session_token,
                    filters,
                } => {
                    assert_eq!(session_token, SessionToken::new([7; SESSION_TOKEN_LEN]));
                    assert_eq!(filters, vec![CanIdFilter::new(0x100, 0x200)]);
                    let response = ServerMessage::Response(ServerResponse::HelloAck {
                        request_id,
                        session_id: 42,
                        role_granted: BridgeRole::WriterCandidate,
                    });
                    let encoded = encode_server_message(&response).unwrap();
                    write_framed(&mut stream, &encoded).unwrap();
                },
                other => panic!("unexpected request: {other:?}"),
            }
        });

        let options = BridgeClientOptions {
            session_token: SessionToken::new([7; SESSION_TOKEN_LEN]),
            filters: vec![CanIdFilter::new(0x100, 0x200)],
            connect_timeout: Duration::from_secs(1),
            request_timeout: Duration::from_secs(1),
            tcp_tls: Some(tls.client_tls_config()),
        };
        let client = GsUsbBridgeClient::connect(BridgeEndpoint::TcpTls(addr), options).unwrap();
        assert_eq!(client.session_id(), 42);
        assert_eq!(client.role_granted(), BridgeRole::WriterCandidate);
    }

    #[test]
    fn test_recv_event_handles_session_replaced() {
        let tls = TestTlsFixture::new();
        let addr = tls.spawn_server(move |mut stream| {
            let _ = read_framed(&mut stream).unwrap();
            let hello_ack = ServerMessage::Response(ServerResponse::HelloAck {
                request_id: 1,
                session_id: 9,
                role_granted: BridgeRole::Observer,
            });
            let encoded = encode_server_message(&hello_ack).unwrap();
            write_framed(&mut stream, &encoded).unwrap();
            let event = ServerMessage::Event(BridgeEvent::SessionReplaced);
            let encoded = encode_server_message(&event).unwrap();
            write_framed(&mut stream, &encoded).unwrap();
        });

        let options = BridgeClientOptions {
            session_token: SessionToken::random(),
            filters: vec![],
            connect_timeout: Duration::from_secs(1),
            request_timeout: Duration::from_secs(1),
            tcp_tls: Some(tls.client_tls_config()),
        };
        let mut client = GsUsbBridgeClient::connect(BridgeEndpoint::TcpTls(addr), options).unwrap();
        let event = client.recv_event(Duration::from_secs(1)).unwrap();
        assert_eq!(event, BridgeEvent::SessionReplaced);
        assert!(matches!(
            client.get_status(),
            Err(BridgeError::NotConnected)
        ));
    }

    #[test]
    fn test_wait_response_buffers_event() {
        let tls = TestTlsFixture::new();
        let addr = tls.spawn_server(move |mut stream| {
            let _ = read_framed(&mut stream).unwrap();
            let hello_ack = ServerMessage::Response(ServerResponse::HelloAck {
                request_id: 1,
                session_id: 9,
                role_granted: BridgeRole::Observer,
            });
            write_framed(&mut stream, &encode_server_message(&hello_ack).unwrap()).unwrap();

            let payload = read_framed(&mut stream).unwrap();
            let request = decode_client_request(&payload).unwrap();
            let ClientRequest::Ping { request_id } = request else {
                panic!("expected ping");
            };
            let event = ServerMessage::Event(BridgeEvent::ReceiveFrame(
                PiperFrame::new_standard(0x111, &[1, 2, 3, 4]).unwrap(),
            ));
            write_framed(&mut stream, &encode_server_message(&event).unwrap()).unwrap();
            let ok = ServerMessage::Response(ServerResponse::Ok { request_id });
            write_framed(&mut stream, &encode_server_message(&ok).unwrap()).unwrap();
        });

        let options = BridgeClientOptions {
            session_token: SessionToken::random(),
            filters: vec![],
            connect_timeout: Duration::from_secs(1),
            request_timeout: Duration::from_secs(1),
            tcp_tls: Some(tls.client_tls_config()),
        };
        let mut client = GsUsbBridgeClient::connect(BridgeEndpoint::TcpTls(addr), options).unwrap();
        client.ping().unwrap();
        let event = client.recv_event(Duration::from_secs(1)).unwrap();
        assert!(matches!(event, BridgeEvent::ReceiveFrame(_)));
    }
}
