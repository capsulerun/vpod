use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;

use rustls::{ClientConfig, ClientConnection, StreamOwned, RootCertStore};
use rustls::pki_types::ServerName;

const OP_TLS_CONNECT: u8 = 0x10;
const OP_TLS_READ: u8 = 0x11;
const OP_TLS_WRITE: u8 = 0x12;
const OP_TLS_SHUTDOWN: u8 = 0x13;

const STATUS_OK: u8 = 0x00;
const STATUS_ERR: u8 = 0x01;
const STATUS_EOF: u8 = 0x02;

type TlsStream = StreamOwned<ClientConnection, TcpStream>;

pub struct TlsHandler {
    sessions: HashMap<u32, TlsStream>,
    next_id: u32,
    tls_config: Arc<ClientConfig>,
}

impl TlsHandler {
    pub fn new() -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        #[cfg(target_arch = "wasm32")]
        let _ = rustls::crypto::CryptoProvider::install_default(rustls_rustcrypto::provider());

        let mut root_store = RootCertStore::empty();
        root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

        let config = ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth();

        Self {
            sessions: HashMap::new(),
            next_id: 1,
            tls_config: Arc::new(config),
        }
    }

    pub fn handle_request(&mut self, data: &[u8]) -> Vec<u8> {
        if data.is_empty() {
            return vec![STATUS_ERR];
        }

        eprintln!("[tls] op=0x{:02x} req_len={}", data[0], data.len());
        let result = match data[0] {
            OP_TLS_CONNECT => self.tls_connect(&data[1..]),
            OP_TLS_READ => self.tls_read(&data[1..]),
            OP_TLS_WRITE => self.tls_write(&data[1..]),
            OP_TLS_SHUTDOWN => self.tls_shutdown(&data[1..]),
            _ => return vec![STATUS_ERR],
        };
        eprintln!("[tls] op=0x{:02x} result={} resp_len={}", data[0],
            if result[0] == STATUS_OK { "OK" } else if result[0] == STATUS_EOF { "EOF" } else { "ERR" },
            result.len());
        result
    }

    pub fn is_tls_op(op: u8) -> bool {
        matches!(op, OP_TLS_CONNECT | OP_TLS_READ | OP_TLS_WRITE | OP_TLS_SHUTDOWN)
    }

    fn tls_connect(&mut self, data: &[u8]) -> Vec<u8> {
        // Wire: [hostname_len:u16le][hostname][port:u16le]
        if data.len() < 4 {
            return vec![STATUS_ERR];
        }

        let hlen = u16::from_le_bytes(data[0..2].try_into().unwrap()) as usize;
        if data.len() < 2 + hlen + 2 {
            return vec![STATUS_ERR];
        }

        let hostname = match std::str::from_utf8(&data[2..2 + hlen]) {
            Ok(s) => s.to_string(),
            Err(_) => return vec![STATUS_ERR],
        };
        let port = u16::from_le_bytes(data[2 + hlen..4 + hlen].try_into().unwrap());

        eprintln!("[tls] connecting to {}:{}", hostname, port);

        let server_name = match ServerName::try_from(hostname.clone()) {
            Ok(n) => n,
            Err(_) => return vec![STATUS_ERR],
        };

        let tcp = match TcpStream::connect(format!("{}:{}", hostname, port)) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[tls] TCP connect failed: {}", e);
                return vec![STATUS_ERR];
            }
        };

        let conn = match ClientConnection::new(self.tls_config.clone(), server_name) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("[tls] TLS handshake setup failed: {}", e);
                return vec![STATUS_ERR];
            }
        };

        let stream = StreamOwned::new(conn, tcp);

        let session_id = self.next_id;
        self.next_id += 1;
        self.sessions.insert(session_id, stream);

        let mut out = vec![STATUS_OK];
        out.extend_from_slice(&session_id.to_le_bytes());
        out
    }

    fn tls_read(&mut self, data: &[u8]) -> Vec<u8> {
        // Wire: [session_id:u32le][max_len:u32le]
        if data.len() < 8 {
            return vec![STATUS_ERR];
        }

        let session_id = u32::from_le_bytes(data[0..4].try_into().unwrap());
        let max_len = u32::from_le_bytes(data[4..8].try_into().unwrap()) as usize;

        let stream = match self.sessions.get_mut(&session_id) {
            Some(s) => s,
            None => return vec![STATUS_ERR],
        };

        let mut buf = vec![0u8; max_len.min(65536)];
        match stream.read(&mut buf) {
            Ok(0) => vec![STATUS_EOF],
            Ok(n) => {
                let mut out = Vec::with_capacity(1 + n);
                out.push(STATUS_OK);
                out.extend_from_slice(&buf[..n]);
                out
            }
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionAborted => vec![STATUS_EOF],
            Err(_) => vec![STATUS_ERR],
        }
    }

    fn tls_write(&mut self, data: &[u8]) -> Vec<u8> {
        // Wire: [session_id:u32le][data...]
        if data.len() < 4 {
            return vec![STATUS_ERR];
        }

        let session_id = u32::from_le_bytes(data[0..4].try_into().unwrap());
        let payload = &data[4..];

        let stream = match self.sessions.get_mut(&session_id) {
            Some(s) => s,
            None => return vec![STATUS_ERR],
        };

        match stream.write(payload) {
            Ok(n) => {
                let _ = stream.flush();
                let mut out = vec![STATUS_OK];
                out.extend_from_slice(&(n as u32).to_le_bytes());
                out
            }
            Err(_) => vec![STATUS_ERR],
        }
    }

    fn tls_shutdown(&mut self, data: &[u8]) -> Vec<u8> {
        if data.len() < 4 {
            return vec![STATUS_ERR];
        }

        let session_id = u32::from_le_bytes(data[0..4].try_into().unwrap());
        self.sessions.remove(&session_id);
        vec![STATUS_OK]
    }
}
