// The upstream half of the :443 gateway.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpStream};
use std::sync::Arc;

use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection};

pub const PREAMBLE_PREFIX: &[u8] = b"VPOD-CONNECT ";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpstreamMode {
    TerminateHere,
    HostTerminates,
}

impl UpstreamMode {
    pub fn from_env() -> Self {
        match std::env::var("VPOD_HOST_TLS") {
            Ok(value) if value != "0" && !value.is_empty() => Self::HostTerminates,
            _ => Self::TerminateHere,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpstreamStatus {
    Open,
    Closed,
    Failed,
}

enum Wire {
    Tls(Box<ClientConnection>),
    Plain { outbound: VecDeque<u8> },
}

pub struct Upstream {
    socket: TcpStream,
    wire: Wire,
    inbound: VecDeque<u8>,
    delivered_plaintext: bool,
}

impl Upstream {
    pub fn connect(
        mode: UpstreamMode,
        client_config: Arc<ClientConfig>,
        destination_ip: [u8; 4],
        port: u16,
        host: &str,
    ) -> Result<Self, String> {
        let server_name = ServerName::try_from(host.to_string()).map_err(|e| e.to_string())?;

        let address = SocketAddrV4::new(Ipv4Addr::from(destination_ip), port);

        #[cfg(target_family = "wasm")]
        let socket = TcpStream::connect(address);

        #[cfg(not(target_family = "wasm"))]
        let socket =
            TcpStream::connect_timeout(&address.into(), std::time::Duration::from_secs(10));

        let socket = socket.map_err(|e| e.to_string())?;
        socket.set_nonblocking(true).ok();
        socket.set_nodelay(true).ok();

        let wire = match mode {
            UpstreamMode::TerminateHere => {
                let mut client =
                    ClientConnection::new(client_config, server_name).map_err(|e| e.to_string())?;

                client.set_buffer_limit(None);
                Wire::Tls(Box::new(client))
            }
            UpstreamMode::HostTerminates => {
                let mut outbound = VecDeque::new();
                outbound.extend(PREAMBLE_PREFIX);
                outbound.extend(format!("{host} {port}\n").as_bytes());
                Wire::Plain { outbound }
            }
        };

        Ok(Self {
            socket,
            wire,
            inbound: VecDeque::new(),
            delivered_plaintext: false,
        })
    }

    pub fn handshaking(&self) -> bool {
        match &self.wire {
            Wire::Tls(client) => client.is_handshaking(),
            Wire::Plain { .. } => false,
        }
    }

    pub fn send_plaintext(&mut self, bytes: &[u8]) -> Result<(), String> {
        match &mut self.wire {
            Wire::Tls(client) => client
                .writer()
                .write_all(bytes)
                .map_err(|e| format!("upstream write: {e}")),
            Wire::Plain { outbound, .. } => {
                outbound.extend(bytes);
                Ok(())
            }
        }
    }

    pub fn read_plaintext(&mut self, buf: &mut [u8]) -> usize {
        let n = self.inbound.len().min(buf.len());
        for slot in buf.iter_mut().take(n) {
            *slot = self.inbound.pop_front().unwrap();
        }
        n
    }

    pub fn service(&mut self) -> UpstreamStatus {
        match &mut self.wire {
            Wire::Tls(_) => self.service_tls(),
            Wire::Plain { .. } => self.service_plain(),
        }
    }

    fn service_tls(&mut self) -> UpstreamStatus {
        let Wire::Tls(client) = &mut self.wire else {
            unreachable!("service_tls on a plaintext upstream");
        };

        let handshook = !client.is_handshaking();

        while client.wants_write() {
            match client.write_tls(&mut self.socket) {
                Ok(0) => break,
                Ok(_) => {}
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => {
                    return Self::fault(handshook, "write", e);
                }
            }
        }

        let mut buf = [0u8; 16384];
        let mut decrypt_into = |client: &mut ClientConnection, inbound: &mut VecDeque<u8>| {
            while let Ok(n) = client.reader().read(&mut buf) {
                if n == 0 {
                    break;
                }
                inbound.extend(&buf[..n]);
            }
        };

        let status = loop {
            decrypt_into(client.as_mut(), &mut self.inbound);

            match client.read_tls(&mut self.socket) {
                Ok(0) => break UpstreamStatus::Closed,
                Ok(_) => {
                    if let Err(e) = client.process_new_packets() {
                        if handshook {
                            log::debug!("upstream: closed uncleanly: {e}");
                            break UpstreamStatus::Closed;
                        }
                        log::warn!("upstream: tls error: {e}");
                        return UpstreamStatus::Failed;
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    break UpstreamStatus::Open;
                }
                Err(e) => {
                    let fault = Self::fault(handshook, "read", e);
                    if fault == UpstreamStatus::Failed {
                        return fault;
                    }
                    break fault;
                }
            }
        };

        decrypt_into(client.as_mut(), &mut self.inbound);
        self.delivered_plaintext |= !self.inbound.is_empty();

        status
    }

    fn service_plain(&mut self) -> UpstreamStatus {
        let Wire::Plain { outbound } = &mut self.wire else {
            unreachable!("service_plain on a tls upstream");
        };

        while !outbound.is_empty() {
            let (front, _) = outbound.as_slices();
            if front.is_empty() {
                break;
            }

            match self.socket.write(front) {
                Ok(0) => break,
                Ok(n) => {
                    outbound.drain(..n);
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => {
                    return Self::fault(self.delivered_plaintext, "write", e);
                }
            }
        }

        let mut buf = [0u8; 16384];
        loop {
            match self.socket.read(&mut buf) {
                Ok(0) => return UpstreamStatus::Closed,
                Ok(n) => {
                    self.inbound.extend(&buf[..n]);
                    self.delivered_plaintext = true;
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => {
                    return Self::fault(self.delivered_plaintext, "read", e);
                }
            }
        }

        UpstreamStatus::Open
    }

    fn fault(usable: bool, direction: &str, error: std::io::Error) -> UpstreamStatus {
        if usable {
            log::debug!("upstream: {direction} ended: {error}");
            return UpstreamStatus::Closed;
        }
        log::warn!("upstream: {direction} error: {error}");
        UpstreamStatus::Failed
    }
}
