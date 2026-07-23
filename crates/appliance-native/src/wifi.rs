//! Native raw-TCP connector for the Wi-Fi local device API proof profile.

use std::fs::{File, symlink_metadata};
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rand_core::{CryptoRng, RngCore};
use reticulum_device_client::{
    ActivatedCredential, ClientConfig, ClientError, ClientSessionProfile, DeviceClient,
};
use reticulum_lxmf_chat_app::DeviceClientSession;
use reticulum_lxmf_chat_runtime::{
    ConnectFailure, ConnectedSession, ConnectionMetadata, ConnectionTransport, Connector,
};

const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_IO_SLICE: Duration = Duration::from_millis(100);
const DEFAULT_OPERATION_TIMEOUT: Duration = Duration::from_secs(5);

/// One native TCP endpoint plus an app-private activated credential.
pub(crate) struct WifiConnector {
    endpoint: SocketAddr,
    credential_path: PathBuf,
    connect_timeout: Duration,
    io_slice: Duration,
    operation_timeout: Duration,
}

impl WifiConnector {
    pub(crate) const fn new(endpoint: SocketAddr, credential_path: PathBuf) -> Self {
        Self {
            endpoint,
            credential_path,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            io_slice: DEFAULT_IO_SLICE,
            operation_timeout: DEFAULT_OPERATION_TIMEOUT,
        }
    }
}

impl Connector for WifiConnector {
    fn connect(&mut self) -> Result<ConnectedSession, ConnectFailure> {
        let credential =
            read_credential(&self.credential_path).map_err(ConnectFailure::permanent)?;
        let stream =
            TcpStream::connect_timeout(&self.endpoint, self.connect_timeout).map_err(|error| {
                ConnectFailure::retryable(format!("Wi-Fi TCP connect failed: {error}"))
            })?;
        stream
            .set_read_timeout(Some(self.io_slice))
            .map_err(|error| {
                ConnectFailure::retryable(format!("could not set Wi-Fi read timeout: {error}"))
            })?;
        stream
            .set_write_timeout(Some(self.io_slice))
            .map_err(|error| {
                ConnectFailure::retryable(format!("could not set Wi-Fi write timeout: {error}"))
            })?;
        stream.set_nodelay(true).map_err(|error| {
            ConnectFailure::retryable(format!("could not configure Wi-Fi TCP stream: {error}"))
        })?;

        let client = DeviceClient::connect_with_profile(
            WifiStream(stream),
            credential,
            &mut HostRng,
            ClientConfig::new(
                self.operation_timeout,
                self.operation_timeout,
                512,
                16 * 1024 * 1024,
            ),
            ClientSessionProfile::WifiQualification,
        )
        .map_err(classify_client_failure)?;
        let device_label = hex::encode(client.device_id().as_bytes());
        Ok(ConnectedSession::new(
            DeviceClientSession::new(client),
            ConnectionMetadata::new(
                ConnectionTransport::Wifi,
                self.endpoint.to_string(),
                device_label,
            ),
        ))
    }
}

/// TCP reports an orderly peer shutdown as `Ok(0)`, while some other
/// byte-stream backends use zero-length reads to mean temporary no progress.
///
/// Keep that distinction at the concrete Wi-Fi boundary so the generic device
/// client can continue supporting both families without spinning until its
/// deadline after a board reset or rejected connection.
struct WifiStream(TcpStream);

impl Read for WifiStream {
    fn read(&mut self, destination: &mut [u8]) -> io::Result<usize> {
        match self.0.read(destination) {
            Ok(0) if !destination.is_empty() => Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "Wi-Fi TCP peer closed the connection",
            )),
            result => result,
        }
    }
}

impl Write for WifiStream {
    fn write(&mut self, source: &[u8]) -> io::Result<usize> {
        self.0.write(source)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

fn read_credential(path: &Path) -> Result<ActivatedCredential, String> {
    let metadata = symlink_metadata(path)
        .map_err(|error| format!("could not inspect app-private credential: {error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("app-private credential must be a regular non-symlink file".to_owned());
    }
    let mut file = File::open(path)
        .map_err(|error| format!("could not open app-private credential: {error}"))?;
    ActivatedCredential::read_from(&mut file)
        .map_err(|error| format!("could not decode app-private credential: {error}"))
}

fn classify_client_failure(error: ClientError) -> ConnectFailure {
    if matches!(&error, ClientError::Handshake(_)) {
        ConnectFailure::permanent(format!("Wi-Fi authentication failed: {error}"))
    } else {
        ConnectFailure::retryable(format!("Wi-Fi device session failed: {error}"))
    }
}

struct HostRng;

impl RngCore for HostRng {
    fn next_u32(&mut self) -> u32 {
        let mut bytes = [0_u8; 4];
        self.fill_bytes(&mut bytes);
        u32::from_le_bytes(bytes)
    }

    fn next_u64(&mut self) -> u64 {
        let mut bytes = [0_u8; 8];
        self.fill_bytes(&mut bytes);
        u64::from_le_bytes(bytes)
    }

    fn fill_bytes(&mut self, destination: &mut [u8]) {
        self.try_fill_bytes(destination)
            .unwrap_or_else(|error| panic!("operating-system randomness failed: {error}"));
    }

    fn try_fill_bytes(&mut self, destination: &mut [u8]) -> Result<(), rand_core::Error> {
        getrandom::fill(destination).map_err(|_| {
            NonZeroU32::new(rand_core::Error::CUSTOM_START)
                .expect("rand_core custom error base is nonzero")
                .into()
        })
    }
}

impl CryptoRng for HostRng {}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::fs;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    use reticulum_device_api_framing::{DecodeEvent, Record, StreamDecoder};
    use reticulum_device_api_session::{
        ActiveCredential, BearerBinding, ClientHello, CredentialGeneration, CredentialId, DeviceId,
        ServerHelloFlight, ServerParameters, SessionEpochAllocator, SessionSuite,
    };

    use super::*;

    const DEVICE_BYTES: [u8; 16] = [0x11; 16];
    const CREDENTIAL_BYTES: [u8; 16] = [0x22; 16];
    const GENERATION: u64 = 7;
    const PSK: [u8; 32] = [0x33; 32];
    static FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct FixedRng(u8);

    impl RngCore for FixedRng {
        fn next_u32(&mut self) -> u32 {
            u32::from_le_bytes([self.0; 4])
        }

        fn next_u64(&mut self) -> u64 {
            u64::from_le_bytes([self.0; 8])
        }

        fn fill_bytes(&mut self, destination: &mut [u8]) {
            destination.fill(self.0);
        }

        fn try_fill_bytes(&mut self, destination: &mut [u8]) -> Result<(), rand_core::Error> {
            self.fill_bytes(destination);
            Ok(())
        }
    }

    impl CryptoRng for FixedRng {}

    struct TestCredential(PathBuf);

    impl TestCredential {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time follows the Unix epoch")
                .as_nanos();
            let sequence = FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "reticulum-native-wifi-{}-{nonce}-{sequence}.rdpkey",
                std::process::id()
            ));
            let mut bytes = [0_u8; 96];
            bytes[..8].copy_from_slice(b"RDPKEY1\0");
            bytes[8..10].copy_from_slice(&1_u16.to_le_bytes());
            bytes[10] = 2;
            bytes[16..32].copy_from_slice(&DEVICE_BYTES);
            bytes[32..48].copy_from_slice(&CREDENTIAL_BYTES);
            bytes[48..56].copy_from_slice(&GENERATION.to_le_bytes());
            bytes[56..88].copy_from_slice(&PSK);
            fs::write(&path, bytes).expect("test credential writes");
            Self(path)
        }
    }

    impl Drop for TestCredential {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    #[test]
    fn native_wifi_connector_authenticates_over_a_partial_tcp_stream() {
        let credential = TestCredential::new();
        let listener = TcpListener::bind("127.0.0.1:0").expect("test listener binds");
        let endpoint = listener.local_addr().expect("listener has an endpoint");
        let server = thread::spawn(move || serve_handshake(listener));

        let mut connector = WifiConnector::new(endpoint, credential.0.clone());
        let connected = connector
            .connect()
            .expect("native Wi-Fi connector authenticates");
        drop(connected);
        server.join().expect("test server exits");
    }

    #[test]
    fn missing_app_private_credential_is_a_permanent_configuration_failure() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("test listener binds");
        let endpoint = listener.local_addr().expect("listener has an endpoint");
        let missing = std::env::temp_dir().join("reticulum-definitely-missing-wifi.rdpkey");
        let mut connector = WifiConnector::new(endpoint, missing);

        assert!(matches!(
            connector.connect(),
            Err(ConnectFailure::Permanent(reason))
                if reason.contains("app-private credential")
        ));
    }

    #[test]
    fn orderly_tcp_shutdown_becomes_a_prompt_transport_error() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("test listener binds");
        let endpoint = listener.local_addr().expect("listener has an endpoint");
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().expect("test client connects");
            drop(stream);
        });
        let stream = TcpStream::connect(endpoint).expect("test client connects");
        stream
            .set_read_timeout(Some(Duration::from_secs(1)))
            .expect("test read timeout configures");
        let mut stream = WifiStream(stream);
        server.join().expect("test server exits");

        let mut byte = [0_u8; 1];
        let error = stream
            .read(&mut byte)
            .expect_err("TCP EOF is not generic no-progress");
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
    }

    fn serve_handshake(listener: TcpListener) {
        let (mut stream, _) = listener.accept().expect("test client connects");
        stream
            .set_read_timeout(Some(Duration::from_secs(1)))
            .expect("test read timeout configures");
        let hello = ClientHello::from_record(read_record(&mut stream))
            .expect("client sends a canonical hello");
        assert_eq!(hello.suite(), SessionSuite::WifiQualification);
        assert_eq!(hello.bearer(), BearerBinding::Wifi);

        let mut epochs = SessionEpochAllocator::new();
        let mut rng = FixedRng(0x88);
        let mut hello_flight = ServerHelloFlight::begin(
            hello,
            ActiveCredential::new(
                CredentialId::new(CREDENTIAL_BYTES),
                CredentialGeneration::new(GENERATION),
                PSK,
            ),
            ServerParameters::new_for_suite(
                DeviceId::new(DEVICE_BYTES),
                BearerBinding::Wifi,
                SessionSuite::WifiQualification,
            ),
            &mut epochs,
            &mut rng,
        )
        .expect("server handshake begins");
        write_partial(&mut stream, hello_flight.remaining());
        let hello_length = hello_flight.remaining().len();
        hello_flight
            .advance(hello_length)
            .expect("server hello advances");
        let mut proof_flight = hello_flight
            .try_finish()
            .unwrap_or_else(|_| panic!("complete server hello finishes"));
        write_partial(&mut stream, proof_flight.remaining());
        let proof_length = proof_flight.remaining().len();
        proof_flight
            .advance(proof_length)
            .expect("server proof advances");
        let pending = proof_flight
            .try_finish()
            .unwrap_or_else(|_| panic!("complete server proof finishes"));
        let session = pending
            .authenticate(read_record(&mut stream))
            .expect("client proof authenticates");
        drop(session);
    }

    fn read_record(stream: &mut TcpStream) -> Record {
        let mut decoder = StreamDecoder::new();
        let mut byte = [0_u8; 1];
        loop {
            stream.read_exact(&mut byte).expect("framed byte arrives");
            match decoder.push(byte[0]) {
                DecodeEvent::Pending => {}
                DecodeEvent::Record(record) => return record,
                DecodeEvent::MalformedCobs
                | DecodeEvent::MalformedRecord(_)
                | DecodeEvent::Overflow => panic!("peer sent malformed framing"),
            }
        }
    }

    fn write_partial(stream: &mut TcpStream, bytes: &[u8]) {
        let mut pending: VecDeque<&[u8]> = bytes.chunks(2).collect();
        while let Some(chunk) = pending.pop_front() {
            stream
                .write_all(chunk)
                .expect("partial test write succeeds");
        }
    }
}
