// Transparent TLS-terminating proxy and prepare transfer to the host

use std::collections::HashMap;
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpStream};
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use const_oid::db::rfc5280::ID_KP_SERVER_AUTH;
use der::asn1::Ia5String;
use der::{DecodePem, Encode};
use p256::ecdsa::{DerSignature, SigningKey};
use p256::pkcs8::{DecodePrivateKey, EncodePrivateKey};
use rand_core::{OsRng, RngCore};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use rustls::{ClientConfig, ClientConnection, RootCertStore, ServerConfig, ServerConnection};
use x509_cert::Certificate;
use x509_cert::builder::{Builder, CertificateBuilder, Profile};
use x509_cert::ext::pkix::name::GeneralName;
use x509_cert::ext::pkix::{ExtendedKeyUsage, SubjectAltName};
use x509_cert::name::Name;
use x509_cert::serial_number::SerialNumber;
use x509_cert::spki::SubjectPublicKeyInfoOwned;
use x509_cert::time::{Time, Validity};

const CA_KEY_PEM: &str = include_str!("../../assets/tls/vpod-ca-key.pem");
pub const CA_CERT_PEM: &str = include_str!("../../assets/tls/vpod-ca-cert.pem");

#[derive(Clone)]
pub struct TlsContext {
    server_config: Arc<ServerConfig>,
    client_config: Arc<ClientConfig>,
}

impl TlsContext {
    pub fn new() -> Result<Self, String> {
        let provider = Arc::new(rustls_rustcrypto::provider());

        let resolver = Arc::new(SniResolver::from_pems(
            provider.clone(),
            CA_KEY_PEM,
            CA_CERT_PEM,
        )?);

        let server_config = ServerConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .map_err(|e| e.to_string())?
            .with_no_client_auth()
            .with_cert_resolver(resolver);

        let mut roots = RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let client_config = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|e| e.to_string())?
            .with_root_certificates(roots)
            .with_no_client_auth();

        Ok(Self {
            server_config: Arc::new(server_config),
            client_config: Arc::new(client_config),
        })
    }

    pub(crate) fn upstream_config(&self) -> Arc<ClientConfig> {
        self.client_config.clone()
    }
}

struct SniResolver {
    provider: Arc<CryptoProvider>,
    ca_key: SigningKey,
    ca_issuer: Name,
    cache: Mutex<HashMap<String, Arc<CertifiedKey>>>,
}

impl std::fmt::Debug for SniResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SniResolver").finish()
    }
}

impl SniResolver {
    fn from_pems(
        provider: Arc<CryptoProvider>,
        ca_key_pem: &str,
        ca_cert_pem: &str,
    ) -> Result<Self, String> {
        let ca_key = SigningKey::from_pkcs8_pem(ca_key_pem).map_err(|e| e.to_string())?;
        let ca_cert = Certificate::from_pem(ca_cert_pem).map_err(|e| e.to_string())?;
        Ok(Self {
            provider,
            ca_key,
            ca_issuer: ca_cert.tbs_certificate.subject,
            cache: Mutex::new(HashMap::new()),
        })
    }

    fn certified_key_for(&self, sni: &str) -> Result<Arc<CertifiedKey>, String> {
        if let Some(hit) = self.cache.lock().unwrap().get(sni) {
            return Ok(hit.clone());
        }

        let (leaf_der, key_der) = self.mint_leaf(sni)?;
        let signing_key = self
            .provider
            .key_provider
            .load_private_key(PrivateKeyDer::try_from(key_der).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        let certified = Arc::new(CertifiedKey::new(
            vec![CertificateDer::from(leaf_der)],
            signing_key,
        ));

        self.cache
            .lock()
            .unwrap()
            .insert(sni.to_string(), certified.clone());
        Ok(certified)
    }

    fn mint_leaf(&self, sni: &str) -> Result<(Vec<u8>, Vec<u8>), String> {
        let leaf_key = SigningKey::random(&mut OsRng);
        let spki = SubjectPublicKeyInfoOwned::from_key(*leaf_key.verifying_key())
            .map_err(|e| e.to_string())?;
        let subject = Name::from_str(&format!("CN={sni}")).map_err(|e| e.to_string())?;

        let now = SystemTime::now();
        let not_before =
            Time::try_from(now - Duration::from_secs(24 * 3600)).map_err(|e| e.to_string())?;
        let not_after = Time::try_from(now + Duration::from_secs(825 * 24 * 3600))
            .map_err(|e| e.to_string())?;
        let validity = Validity {
            not_before,
            not_after,
        };

        let serial = SerialNumber::from(rand_serial());

        let mut builder = CertificateBuilder::new(
            Profile::Leaf {
                issuer: self.ca_issuer.clone(),
                enable_key_agreement: false,
                enable_key_encipherment: false,
            },
            serial,
            validity,
            subject,
            spki,
            &self.ca_key,
        )
        .map_err(|e| e.to_string())?;

        builder
            .add_extension(&SubjectAltName(vec![GeneralName::DnsName(
                Ia5String::new(sni).map_err(|e| e.to_string())?,
            )]))
            .map_err(|e| e.to_string())?;
        builder
            .add_extension(&ExtendedKeyUsage(vec![ID_KP_SERVER_AUTH]))
            .map_err(|e| e.to_string())?;

        let leaf: Certificate = builder.build::<DerSignature>().map_err(|e| e.to_string())?;
        let leaf_der = leaf.to_der().map_err(|e| e.to_string())?;
        let key_der = leaf_key
            .to_pkcs8_der()
            .map_err(|e| e.to_string())?
            .as_bytes()
            .to_vec();
        Ok((leaf_der, key_der))
    }
}

impl ResolvesServerCert for SniResolver {
    fn resolve(&self, client_hello: ClientHello) -> Option<Arc<CertifiedKey>> {
        let sni = client_hello.server_name()?;
        match self.certified_key_for(sni) {
            Ok(ck) => Some(ck),
            Err(e) => {
                log::warn!("tls_proxy: leaf minting failed for {sni}: {e}");
                None
            }
        }
    }
}

fn rand_serial() -> u64 {
    let mut b = [0u8; 8];
    OsRng.fill_bytes(&mut b);
    (u64::from_be_bytes(b) >> 1) | 1
}

pub struct TlsProxy {
    server: ServerConnection,
    client: Option<ClientConnection>,
    upstream: Option<TcpStream>,
    dst_ip: [u8; 4],
    upstream_port: u16,
    client_config: Arc<ClientConfig>,
    to_guest: VecDeque<u8>,
    failed: bool,
    upstream_closed: bool,
    close_notified: bool,
    timing: Option<Timing>,
}

pub(crate) struct Timing {
    start: Instant,
    first_guest_bytes: bool,
    serverhello_sent: bool,
    guest_hs_done: bool,
    upstream_connected: bool,
    upstream_hs_done: bool,
    first_reply: bool,
}

impl Timing {
    pub(crate) fn new() -> Option<Self> {
        match std::env::var("VPOD_TLS_TIMING") {
            Ok(v) if v != "0" && !v.is_empty() => Some(Self {
                start: Instant::now(),
                first_guest_bytes: false,
                serverhello_sent: false,
                guest_hs_done: false,
                upstream_connected: false,
                upstream_hs_done: false,
                first_reply: false,
            }),
            _ => None,
        }
    }

    pub(crate) fn mark(&self, label: &str) {
        eprintln!(
            "[tls-timing] +{:>6.1}ms {label}",
            self.start.elapsed().as_secs_f64() * 1000.0
        );
    }
}

impl Drop for TlsProxy {
    fn drop(&mut self) {
        if let Some(t) = &self.timing {
            t.mark("connection closed (proxy dropped)");
        }
    }
}

impl TlsProxy {
    pub fn new(ctx: &TlsContext, dst_ip: [u8; 4]) -> Result<Self, String> {
        Self::with_timing(ctx, dst_ip, Timing::new())
    }

    pub(crate) fn with_timing(
        ctx: &TlsContext,
        dst_ip: [u8; 4],
        timing: Option<Timing>,
    ) -> Result<Self, String> {
        let mut server = ServerConnection::new(ctx.server_config.clone()).map_err(|e| e.to_string())?;
        server.set_buffer_limit(None);

        Ok(Self {
            server,
            client: None,
            upstream: None,
            dst_ip,
            upstream_port: 443,
            client_config: ctx.client_config.clone(),
            to_guest: VecDeque::new(),
            failed: false,
            upstream_closed: false,
            close_notified: false,
            timing,
        })
    }

    pub fn failed(&self) -> bool {
        self.failed
    }

    pub fn push_from_guest(&mut self, mut bytes: &[u8]) {
        if let Some(t) = &mut self.timing
            && !t.first_guest_bytes
            && !bytes.is_empty()
        {
            t.first_guest_bytes = true;
            t.mark("first guest bytes (ClientHello)");
        }
        while !bytes.is_empty() {
            match self.server.read_tls(&mut bytes) {
                Ok(0) => break,
                Ok(_) => {}
                Err(_) => {
                    self.failed = true;
                    return;
                }
            }
        }
        self.pump();
    }

    pub fn pull_to_guest(&mut self, buf: &mut [u8]) -> Option<usize> {
        self.pump();
        if self.to_guest.is_empty() {
            return None;
        }
        let n = self.to_guest.len().min(buf.len());
        for slot in buf.iter_mut().take(n) {
            *slot = self.to_guest.pop_front().unwrap();
        }
        Some(n)
    }

    pub fn has_pending(&self) -> bool {
        !self.to_guest.is_empty() || self.server.wants_write()
    }

    fn pump(&mut self) {
        if self.failed {
            return;
        }

        if let Err(e) = self.server.process_new_packets() {
            eprintln!("[tls-proxy] guest-side handshake failed: {e}");
            self.failed = true;
            return;
        }

        if let Some(t) = &mut self.timing
            && !t.guest_hs_done
            && !self.server.is_handshaking()
        {
            t.guest_hs_done = true;
            t.mark("guest handshake done");
        }

        if self.client.is_none()
            && !self.server.is_handshaking()
            && let Some(sni) = self.server.server_name().map(|s| s.to_string())
        {
            self.connect_upstream(&sni);

            if let (Some(t), true) = (&mut self.timing, self.client.is_some())
                && !t.upstream_connected
            {
                t.upstream_connected = true;
                t.mark("upstream TCP connected");
            }
        }

        if let Some(client) = self.client.as_mut() {
            let mut buf = [0u8; 16384];

            loop {
                match self.server.reader().read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if client.writer().write_all(&buf[..n]).is_err() {
                            self.failed = true;
                            return;
                        }
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(_) => break,
                }
            }
        }

        self.service_upstream();

        if let Some(client) = self.client.as_mut() {
            let mut buf = [0u8; 16384];
            loop {
                match client.reader().read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if self.server.writer().write_all(&buf[..n]).is_err() {
                            self.failed = true;
                            return;
                        }
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(_) => break,
                }
            }
        }

        if self.upstream_closed && !self.close_notified {
            self.server.send_close_notify();
            self.close_notified = true;
        }

        let mut wrote_to_guest = false;
        while self.server.wants_write() {
            let mut out = Vec::new();
            match self.server.write_tls(&mut out) {
                Ok(0) => break,
                Ok(_) => {
                    wrote_to_guest = true;
                    self.to_guest.extend(out);
                }
                Err(_) => {
                    self.failed = true;
                    break;
                }
            }
        }

        if let Some(t) = &mut self.timing
            && !t.serverhello_sent
            && wrote_to_guest
            && self.server.is_handshaking()
        {
            t.serverhello_sent = true;
            t.mark("ServerHello flight sent to guest");
        }

        if let Some(t) = &mut self.timing
            && t.upstream_hs_done
            && !t.first_reply
            && !self.to_guest.is_empty()
        {
            t.first_reply = true;
            t.mark("first reply byte to guest");
        }
    }

    fn connect_upstream(&mut self, sni: &str) {
        let addr = SocketAddrV4::new(Ipv4Addr::from(self.dst_ip), self.upstream_port);

        #[cfg(target_family = "wasm")]
        let stream = TcpStream::connect(addr);

        #[cfg(not(target_family = "wasm"))]
        let stream = TcpStream::connect_timeout(&addr.into(), Duration::from_secs(10));

        let stream = match stream {
            Ok(s) => s,
            Err(_) => {
                self.failed = true;
                return;
            }
        };
        stream.set_nonblocking(true).ok();
        stream.set_nodelay(true).ok();

        let server_name = match ServerName::try_from(sni.to_string()) {
            Ok(n) => n,
            Err(_) => {
                self.failed = true;
                return;
            }
        };

        match ClientConnection::new(self.client_config.clone(), server_name) {
            Ok(mut c) => {
                c.set_buffer_limit(None);
                self.client = Some(c);
                self.upstream = Some(stream);
            }
            Err(_) => self.failed = true,
        }
    }

    fn service_upstream(&mut self) {
        if self.client.is_none() || self.upstream.is_none() {
            return;
        }

        let handshook = !self.client.as_ref().unwrap().is_handshaking();
        while !self.upstream_closed && self.client.as_ref().unwrap().wants_write() {
            let sock = self.upstream.as_mut().unwrap();
            match self.client.as_mut().unwrap().write_tls(sock) {
                Ok(0) => break,
                Ok(_) => {}
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(_) => {
                    if handshook {
                        self.upstream_closed = true;
                        break;
                    }

                    self.failed = true;
                    return;
                }
            }
        }

        while !self.upstream_closed {
            let sock = self.upstream.as_mut().unwrap();
            match self.client.as_mut().unwrap().read_tls(sock) {
                Ok(0) => {
                    self.upstream_closed = true;
                }
                Ok(_) => {
                    if self.client.as_mut().unwrap().process_new_packets().is_err() {
                        if handshook {
                            self.upstream_closed = true;
                        } else {
                            self.failed = true;
                            return;
                        }
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(_) => {
                    if handshook {
                        self.upstream_closed = true;
                    } else {
                        self.failed = true;
                        return;
                    }
                }
            }

            let mut buf = [0u8; 16384];
            loop {
                match self.client.as_mut().unwrap().reader().read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if self.server.writer().write_all(&buf[..n]).is_err() {
                            self.failed = true;
                            return;
                        }
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(_) => break,
                }
            }
        }

        let upstream_handshaking = self
            .client
            .as_ref()
            .map(|c| c.is_handshaking())
            .unwrap_or(true);
        if let Some(t) = &mut self.timing
            && t.upstream_connected
            && !t.upstream_hs_done
            && !upstream_handshaking
        {
            t.upstream_hs_done = true;
            t.mark("upstream handshake done");
        }
    }
}

pub fn ca_cert_pem() -> &'static str {
    CA_CERT_PEM
}

#[cfg(test)]
impl TlsProxy {
    fn new_test(
        server_config: Arc<ServerConfig>,
        client_config: Arc<ClientConfig>,
        dst_ip: [u8; 4],
        upstream_port: u16,
    ) -> Self {
        let mut server = ServerConnection::new(server_config).unwrap();
        server.set_buffer_limit(None); // match with_timing so tests exercise the real relay

        Self {
            server,
            client: None,
            upstream: None,
            dst_ip,
            upstream_port,
            client_config,
            to_guest: VecDeque::new(),
            failed: false,
            upstream_closed: false,
            close_notified: false,
            timing: None,
        }
    }
}

#[cfg(test)]
pub(crate) fn mint_leaf_with_ca(
    ca_key_pem: &str,
    ca_cert_pem: &str,
    sni: &str,
) -> (Vec<u8>, Vec<u8>) {
    let provider = Arc::new(rustls_rustcrypto::provider());
    let resolver = SniResolver::from_pems(provider, ca_key_pem, ca_cert_pem).unwrap();
    resolver.mint_leaf(sni).unwrap()
}

#[cfg(test)]
pub(crate) fn generate_ca_pems() -> (String, String) {
    use der::pem::LineEnding;
    use x509_cert::der::EncodePem;

    let key = SigningKey::random(&mut OsRng);
    let spki = SubjectPublicKeyInfoOwned::from_key(*key.verifying_key()).unwrap();
    let name = Name::from_str("CN=vpod local CA").unwrap();
    let validity = Validity::from_now(Duration::from_secs(10 * 365 * 24 * 3600)).unwrap();
    let builder = CertificateBuilder::new(
        Profile::Root,
        SerialNumber::from(1u32),
        validity,
        name,
        spki,
        &key,
    )
    .unwrap();
    let cert: Certificate = builder.build::<DerSignature>().unwrap();
    (
        key.to_pkcs8_pem(Default::default()).unwrap().to_string(),
        cert.to_pem(LineEnding::LF).unwrap(),
    )
}

#[cfg(test)]
pub(crate) fn spawn_test_upstream(
    reply: &'static [u8],
) -> (u16, String, std::thread::JoinHandle<()>) {
    use std::net::TcpListener;

    let (up_ca_key, up_ca_cert) = generate_ca_pems();
    let (leaf_der, leaf_key) = mint_leaf_with_ca(&up_ca_key, &up_ca_cert, "localhost");

    let up_server_cfg =
        ServerConfig::builder_with_provider(Arc::new(rustls_rustcrypto::provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(leaf_der)],
                PrivateKeyDer::try_from(leaf_key).unwrap(),
            )
            .unwrap();
    let up_server_cfg = Arc::new(up_server_cfg);

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();

    let handle = std::thread::spawn(move || {
        listener.set_nonblocking(false).ok();
        let (mut sock, _) = listener.accept().unwrap();

        sock.set_read_timeout(Some(Duration::from_secs(10))).ok();
        let mut conn = ServerConnection::new(up_server_cfg).unwrap();
        let mut req = Vec::new();

        loop {
            if conn.wants_write() {
                conn.write_tls(&mut sock).unwrap();
                continue;
            }

            if conn.is_handshaking() {
                conn.read_tls(&mut sock).unwrap();
                conn.process_new_packets().unwrap();
                continue;
            }

            let mut buf = [0u8; 1024];
            match conn.reader().read(&mut buf) {
                Ok(n) if n > 0 => {
                    req.extend_from_slice(&buf[..n]);
                    if req.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                    continue;
                }
                _ => {}
            }
            conn.read_tls(&mut sock).unwrap();
            conn.process_new_packets().unwrap();
        }
        conn.writer().write_all(reply).unwrap();
        while conn.wants_write() {
            conn.write_tls(&mut sock).unwrap();
        }
        conn.send_close_notify();
        while conn.wants_write() {
            conn.write_tls(&mut sock).unwrap();
        }
    });

    (port, up_ca_cert, handle)
}

#[cfg(test)]
pub(crate) fn spawn_test_upstream_streaming(
    body_len: usize,
) -> (u16, String, std::thread::JoinHandle<()>) {
    use std::net::TcpListener;

    let (up_ca_key, up_ca_cert) = generate_ca_pems();
    let (leaf_der, leaf_key) = mint_leaf_with_ca(&up_ca_key, &up_ca_cert, "localhost");
    let up_server_cfg =
        ServerConfig::builder_with_provider(Arc::new(rustls_rustcrypto::provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(leaf_der)],
                PrivateKeyDer::try_from(leaf_key).unwrap(),
            )
            .unwrap();
    let up_server_cfg = Arc::new(up_server_cfg);

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();

    let handle = std::thread::spawn(move || {
        listener.set_nonblocking(false).ok();

        let (mut sock, _) = listener.accept().unwrap();
        sock.set_read_timeout(Some(Duration::from_secs(10))).ok();

        let mut conn = ServerConnection::new(up_server_cfg).unwrap();
        let mut req = Vec::new();

        loop {
            if conn.wants_write() {
                conn.write_tls(&mut sock).unwrap();
                continue;
            }

            if conn.is_handshaking() {
                conn.read_tls(&mut sock).unwrap();
                conn.process_new_packets().unwrap();
                continue;
            }

            let mut buf = [0u8; 1024];
            match conn.reader().read(&mut buf) {
                Ok(n) if n > 0 => {
                    req.extend_from_slice(&buf[..n]);
                    if req.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                    continue;
                }
                _ => {}
            }

            conn.read_tls(&mut sock).unwrap();
            conn.process_new_packets().unwrap();
        }

        let header = format!("HTTP/1.0 200 OK\r\nContent-Length: {body_len}\r\n\r\n");
        conn.writer().write_all(header.as_bytes()).unwrap();
        let mut sent = 0usize;
        let chunk = vec![b'x'; 16384];
        while sent < body_len {
            let n = chunk.len().min(body_len - sent);
            conn.writer().write_all(&chunk[..n]).unwrap();
            sent += n;

            while conn.wants_write() {
                conn.write_tls(&mut sock).unwrap();
            }
        }

        conn.send_close_notify();
        while conn.wants_write() {
            conn.write_tls(&mut sock).unwrap();
        }
    });

    (port, up_ca_cert, handle)
}

#[cfg(test)]
pub(crate) fn spawn_test_upstream_rst(
    reply: &'static [u8],
) -> (u16, String, std::thread::JoinHandle<()>) {
    use std::net::TcpListener;

    let (up_ca_key, up_ca_cert) = generate_ca_pems();
    let (leaf_der, leaf_key) = mint_leaf_with_ca(&up_ca_key, &up_ca_cert, "localhost");
    let up_server_cfg =
        ServerConfig::builder_with_provider(Arc::new(rustls_rustcrypto::provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(leaf_der)],
                PrivateKeyDer::try_from(leaf_key).unwrap(),
            )
            .unwrap();
    let up_server_cfg = Arc::new(up_server_cfg);

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();

    let handle = std::thread::spawn(move || {
        listener.set_nonblocking(false).ok();

        let (mut sock, _) = listener.accept().unwrap();
        sock.set_read_timeout(Some(Duration::from_secs(10))).ok();

        let mut conn = ServerConnection::new(up_server_cfg).unwrap();
        let mut req = Vec::new();
        loop {
            if conn.wants_write() {
                conn.write_tls(&mut sock).unwrap();
                continue;
            }

            if conn.is_handshaking() {
                conn.read_tls(&mut sock).unwrap();
                conn.process_new_packets().unwrap();
                continue;
            }

            let mut buf = [0u8; 1024];
            match conn.reader().read(&mut buf) {
                Ok(n) if n > 0 => {
                    req.extend_from_slice(&buf[..n]);
                    if req.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                    continue;
                }
                _ => {}
            }

            conn.read_tls(&mut sock).unwrap();
            conn.process_new_packets().unwrap();
        }

        conn.writer().write_all(reply).unwrap();
        while conn.wants_write() {
            conn.write_tls(&mut sock).unwrap();
        }

        std::thread::sleep(Duration::from_millis(500));

        {
            use std::os::fd::AsRawFd;
            let linger = libc::linger {
                l_onoff: 1,
                l_linger: 0,
            };
            unsafe {
                libc::setsockopt(
                    sock.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_LINGER,
                    &linger as *const libc::linger as *const libc::c_void,
                    std::mem::size_of::<libc::linger>() as libc::socklen_t,
                );
            }
        }
        drop(sock);
    });

    (port, up_ca_cert, handle)
}

#[cfg(test)]
pub(crate) fn client_config_trusting(ca_pem: &str) -> Arc<ClientConfig> {
    let mut roots = RootCertStore::empty();
    let ca_der = Certificate::from_pem(ca_pem).unwrap().to_der().unwrap();

    roots.add(CertificateDer::from(ca_der)).unwrap();
    Arc::new(
        ClientConfig::builder_with_provider(Arc::new(rustls_rustcrypto::provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;

    const UPSTREAM_REPLY: &[u8] = b"HTTP/1.0 200 OK\r\nContent-Length: 5\r\n\r\nhello";

    fn provider() -> Arc<CryptoProvider> {
        Arc::new(rustls_rustcrypto::provider())
    }

    #[test]
    fn resolver_mints_loadable_leaf_for_sni() {
        let ctx = TlsContext::new().expect("terminator init");
        let _ = ServerConnection::new(ctx.server_config.clone()).unwrap();
    }

    #[test]
    fn end_to_end_bridge_delivers_upstream_reply() {
        // its own CA + a localhost leaf the proxy
        let (up_ca_key, up_ca_cert) = generate_ca_pems();
        let (leaf_der, leaf_key) = mint_leaf_with_ca(&up_ca_key, &up_ca_cert, "localhost");

        let up_server_cfg = ServerConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(leaf_der)],
                PrivateKeyDer::try_from(leaf_key).unwrap(),
            )
            .unwrap();
        let up_server_cfg = Arc::new(up_server_cfg);

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let up = thread::spawn(move || {
            listener.set_nonblocking(false).ok();
            let (mut sock, _) = listener.accept().unwrap();
            sock.set_read_timeout(Some(Duration::from_secs(10))).ok();
            let mut conn = ServerConnection::new(up_server_cfg).unwrap();

            let mut req = Vec::new();
            loop {
                if conn.wants_write() {
                    conn.write_tls(&mut sock).unwrap();
                    continue;
                }

                if conn.is_handshaking() {
                    conn.read_tls(&mut sock).unwrap();
                    conn.process_new_packets().unwrap();
                    continue;
                }

                let mut buf = [0u8; 1024];
                match conn.reader().read(&mut buf) {
                    Ok(n) if n > 0 => {
                        req.extend_from_slice(&buf[..n]);
                        if req.windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                        continue;
                    }
                    _ => {}
                }
                conn.read_tls(&mut sock).unwrap();
                conn.process_new_packets().unwrap();
            }

            conn.writer().write_all(UPSTREAM_REPLY).unwrap();
            while conn.wants_write() {
                conn.write_tls(&mut sock).unwrap();
            }

            conn.send_close_notify();
            while conn.wants_write() {
                conn.write_tls(&mut sock).unwrap();
            }
        });

        let ctx = TlsContext::new().unwrap();
        let mut up_roots = RootCertStore::empty();
        let up_ca_der = Certificate::from_pem(&up_ca_cert)
            .unwrap()
            .to_der()
            .unwrap();

        up_roots.add(CertificateDer::from(up_ca_der)).unwrap();
        let proxy_client_cfg = Arc::new(
            ClientConfig::builder_with_provider(provider())
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_root_certificates(up_roots)
                .with_no_client_auth(),
        );

        let mut proxy = TlsProxy::new_test(
            ctx.server_config.clone(),
            proxy_client_cfg,
            [127, 0, 0, 1],
            port,
        );

        let mut guest_roots = RootCertStore::empty();
        let vpod_ca_der = Certificate::from_pem(CA_CERT_PEM)
            .unwrap()
            .to_der()
            .unwrap();
        guest_roots.add(CertificateDer::from(vpod_ca_der)).unwrap();
        let guest_cfg = Arc::new(
            ClientConfig::builder_with_provider(provider())
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_root_certificates(guest_roots)
                .with_no_client_auth(),
        );

        let mut guest =
            ClientConnection::new(guest_cfg, ServerName::try_from("localhost").unwrap()).unwrap();

        let mut request_sent = false;
        let mut got = Vec::new();
        for _ in 0..2000 {
            let mut out = Vec::new();
            while guest.wants_write() {
                guest.write_tls(&mut out).unwrap();
            }
            if !out.is_empty() {
                proxy.push_from_guest(&out);
            }

            let mut buf = [0u8; 16384];
            while let Some(n) = proxy.pull_to_guest(&mut buf) {
                let mut slice = &buf[..n];
                while !slice.is_empty() {
                    guest.read_tls(&mut slice).unwrap();
                }
                guest.process_new_packets().unwrap();
            }

            if !request_sent && !guest.is_handshaking() {
                guest.writer().write_all(b"GET / HTTP/1.0\r\n\r\n").unwrap();
                request_sent = true;
            }

            let mut buf = [0u8; 1024];
            if let Ok(n) = guest.reader().read(&mut buf)
                && n > 0
            {
                got.extend_from_slice(&buf[..n]);
            }

            if got.windows(5).any(|w| w == b"hello") {
                break;
            }

            if proxy.failed() {
                panic!("proxy failed before delivering reply");
            }
            thread::sleep(Duration::from_millis(1));
        }

        let _ = up.join();
        assert!(
            got.windows(5).any(|w| w == b"hello"),
            "did not receive upstream reply, got {got:?}"
        );
    }

    #[test]
    fn large_response_delivered_in_full_without_truncation() {
        const BODY: usize = 1_000_000;
        let (port, up_ca, up) = spawn_test_upstream_streaming(BODY);

        let ctx = TlsContext::new().unwrap();
        let mut proxy = TlsProxy::new_test(
            ctx.server_config.clone(),
            client_config_trusting(&up_ca),
            [127, 0, 0, 1],
            port,
        );

        let mut guest = ClientConnection::new(
            client_config_trusting(CA_CERT_PEM),
            ServerName::try_from("localhost").unwrap(),
        )
        .unwrap();

        guest.set_buffer_limit(None);

        let mut request_sent = false;
        let mut got = Vec::new();
        for _ in 0..40000 {
            let mut out = Vec::new();
            while guest.wants_write() {
                guest.write_tls(&mut out).unwrap();
            }
            if !out.is_empty() {
                proxy.push_from_guest(&out);
            }

            let mut buf = [0u8; 16384];
            while let Some(n) = proxy.pull_to_guest(&mut buf) {
                let mut slice = &buf[..n];
                while !slice.is_empty() {
                    guest.read_tls(&mut slice).unwrap();
                    // Process + drain after every read_tls so neither the record
                    // deframer nor the plaintext buffer overflows on a big body.
                    guest.process_new_packets().unwrap();
                    let mut pt = [0u8; 16384];
                    loop {
                        match guest.reader().read(&mut pt) {
                            Ok(m) if m > 0 => got.extend_from_slice(&pt[..m]),
                            _ => break,
                        }
                    }
                }
            }

            if !request_sent && !guest.is_handshaking() {
                guest.writer().write_all(b"GET / HTTP/1.0\r\n\r\n").unwrap();
                request_sent = true;
            }

            let body = got
                .windows(4)
                .position(|w| w == b"\r\n\r\n")
                .map(|i| got.len() - (i + 4));
            if body == Some(BODY) {
                break;
            }

            assert!(!proxy.failed(), "proxy failed on large response");
            thread::sleep(Duration::from_millis(1));
        }

        let _ = up.join();
        let body = got
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .map(|i| got.len() - (i + 4));
        assert_eq!(body, Some(BODY), "large body truncated: {body:?} of {BODY}");
    }
}
