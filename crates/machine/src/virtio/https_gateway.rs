//Gateway for guest connections to :443

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpStream};

use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection};
use std::sync::Arc;

use super::tls_proxy::{Timing, TlsContext, TlsProxy};

pub const PREAMBLE_PREFIX: &[u8] = b"VPOD-CONNECT ";
const PREAMBLE_MAX: usize = 280;

enum GatewayState {
    Sniffing(Vec<u8>),
    Tls(Box<TlsProxy>),
    Plain(PlainBridge),
    Failed,
}

pub struct HttpsGateway {
    state: GatewayState,
    ctx: TlsContext,
    upstream_config: Arc<ClientConfig>,
    dst_ip: [u8; 4],
    timing: Option<Timing>,
}

impl HttpsGateway {
    pub fn new(ctx: &TlsContext, dst_ip: [u8; 4]) -> Self {
        Self {
            state: GatewayState::Sniffing(Vec::new()),
            ctx: ctx.clone(),
            upstream_config: ctx.upstream_config(),
            dst_ip,
            timing: Timing::new(),
        }
    }

    #[cfg(test)]
    fn new_test(ctx: &TlsContext, dst_ip: [u8; 4], upstream_config: Arc<ClientConfig>) -> Self {
        Self {
            state: GatewayState::Sniffing(Vec::new()),
            ctx: ctx.clone(),
            upstream_config,
            dst_ip,
            timing: None,
        }
    }

    pub fn failed(&self) -> bool {
        match &self.state {
            GatewayState::Failed => true,
            GatewayState::Tls(p) => p.failed(),
            GatewayState::Plain(b) => b.failed,
            GatewayState::Sniffing(_) => false,
        }
    }

    pub fn eof(&self) -> bool {
        match &self.state {
            GatewayState::Plain(b) => b.upstream_closed && b.to_guest.is_empty(),
            _ => false,
        }
    }

    pub fn has_pending(&self) -> bool {
        match &self.state {
            GatewayState::Tls(p) => p.has_pending(),
            GatewayState::Plain(b) => !b.to_guest.is_empty(),
            _ => false,
        }
    }

    pub fn push_from_guest(&mut self, bytes: &[u8]) {
        match &mut self.state {
            GatewayState::Sniffing(buffered) => {
                buffered.extend_from_slice(bytes);
                self.decide_dialect();
            }
            GatewayState::Tls(p) => p.push_from_guest(bytes),
            GatewayState::Plain(b) => b.push_from_guest(bytes),
            GatewayState::Failed => {}
        }
    }

    pub fn pull_to_guest(&mut self, buf: &mut [u8]) -> Option<usize> {
        match &mut self.state {
            GatewayState::Tls(p) => p.pull_to_guest(buf),
            GatewayState::Plain(b) => b.pull_to_guest(buf),
            _ => None,
        }
    }

    pub fn shutdown_write(&mut self) {}

    fn decide_dialect(&mut self) {
        let GatewayState::Sniffing(buffered) = &self.state else {
            return;
        };
        if buffered.is_empty() {
            return;
        }

        let could_be_preamble = PREAMBLE_PREFIX
            .starts_with(&buffered[..buffered.len().min(PREAMBLE_PREFIX.len())])
            || buffered.starts_with(PREAMBLE_PREFIX);

        if !could_be_preamble {
            let GatewayState::Sniffing(buffered) =
                std::mem::replace(&mut self.state, GatewayState::Failed)
            else {
                unreachable!();
            };

            match TlsProxy::with_timing(&self.ctx, self.dst_ip, self.timing.take()) {
                Ok(mut proxy) => {
                    proxy.push_from_guest(&buffered);
                    self.state = GatewayState::Tls(Box::new(proxy));
                }
                Err(e) => {
                    log::warn!("https_gateway: TLS proxy init failed: {e}");
                }
            }

            return;
        }

        let Some(newline) = buffered.iter().position(|&b| b == b'\n') else {
            if buffered.len() > PREAMBLE_MAX {
                self.state = GatewayState::Failed;
            }

            return;
        };

        let GatewayState::Sniffing(buffered) =
            std::mem::replace(&mut self.state, GatewayState::Failed)
        else {
            unreachable!();
        };

        let (line, remainder) = buffered.split_at(newline + 1);

        let Some((host, port)) = parse_preamble(line) else {
            log::warn!("https_gateway: malformed preamble line");
            return;
        };

        if let Some(t) = &mut self.timing {
            t.mark(&format!(
                "preamble received (plaintext bridge to {host}:{port})"
            ));
        }

        match PlainBridge::connect(
            self.upstream_config.clone(),
            self.dst_ip,
            port,
            &host,
            self.timing.take(),
        ) {
            Ok(mut bridge) => {
                if !remainder.is_empty() {
                    bridge.push_from_guest(remainder);
                }
                self.state = GatewayState::Plain(bridge);
            }
            Err(e) => {
                log::warn!("https_gateway: plaintext bridge to {host}:{port} failed: {e}");
            }
        }
    }
}

fn parse_preamble(line: &[u8]) -> Option<(String, u16)> {
    let line = std::str::from_utf8(line).ok()?;
    let rest = line.strip_prefix(std::str::from_utf8(PREAMBLE_PREFIX).unwrap())?;
    let mut parts = rest.trim_end().split(' ');
    let host = parts.next()?.to_string();
    let port: u16 = parts.next()?.parse().ok()?;
    if host.is_empty() || parts.next().is_some() {
        return None;
    }
    Some((host, port))
}

struct PlainBridge {
    client: ClientConnection,
    upstream: TcpStream,
    to_guest: VecDeque<u8>,
    failed: bool,
    upstream_closed: bool,
    timing: Option<Timing>,
    upstream_hs_done: bool,
    marked_upstream_hs: bool,
    first_reply: bool,
}

impl PlainBridge {
    fn connect(
        client_config: Arc<ClientConfig>,
        dst_ip: [u8; 4],
        port: u16,
        host: &str,
        timing: Option<Timing>,
    ) -> Result<Self, String> {
        let addr = SocketAddrV4::new(Ipv4Addr::from(dst_ip), port);

        // See if better alternative for cfg
        #[cfg(target_family = "wasm")]
        let stream = TcpStream::connect(addr);

        #[cfg(not(target_family = "wasm"))]
        let stream = TcpStream::connect_timeout(&addr.into(), std::time::Duration::from_secs(10));

        let stream = stream.map_err(|e| e.to_string())?;
        stream.set_nonblocking(true).ok();
        stream.set_nodelay(true).ok();

        let server_name = ServerName::try_from(host.to_string()).map_err(|e| e.to_string())?;
        let client =
            ClientConnection::new(client_config, server_name).map_err(|e| e.to_string())?;

        let bridge = Self {
            client,
            upstream: stream,
            to_guest: VecDeque::new(),
            failed: false,
            upstream_closed: false,
            timing,
            upstream_hs_done: false,
            marked_upstream_hs: false,
            first_reply: false,
        };
        if let Some(t) = &bridge.timing {
            t.mark("upstream TCP connected");
        }
        Ok(bridge)
    }

    fn push_from_guest(&mut self, bytes: &[u8]) {
        if self.client.writer().write_all(bytes).is_err() {
            self.failed = true;
            return;
        }

        self.pump();
    }

    fn pull_to_guest(&mut self, buf: &mut [u8]) -> Option<usize> {
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

    fn pump(&mut self) {
        if self.failed {
            return;
        }

        if !self.client.is_handshaking() {
            self.upstream_hs_done = true;
        }

        while !self.upstream_closed && self.client.wants_write() {
            match self.client.write_tls(&mut self.upstream) {
                Ok(0) => break,
                Ok(_) => {}
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => {
                    if self.upstream_hs_done {
                        log::debug!("plain_bridge: upstream write ended: {e}");
                        self.upstream_closed = true;
                        break;
                    }

                    log::warn!("plain_bridge: upstream write error: {e}");
                    self.failed = true;
                    return;
                }
            }
        }

        while !self.upstream_closed {
            match self.client.read_tls(&mut self.upstream) {
                Ok(0) => {
                    self.upstream_closed = true;
                }
                Ok(_) => {
                    if let Err(e) = self.client.process_new_packets() {
                        if self.upstream_hs_done {
                            log::debug!("plain_bridge: upstream closed uncleanly: {e}");
                            self.upstream_closed = true;
                        } else {
                            log::warn!("plain_bridge: upstream tls error: {e}");
                            self.failed = true;
                            return;
                        }
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => {
                    if self.upstream_hs_done {
                        log::debug!("plain_bridge: upstream read ended: {e}");
                        self.upstream_closed = true;
                    } else {
                        log::warn!("plain_bridge: upstream read error: {e}");
                        self.failed = true;
                        return;
                    }
                }
            }
        }

        if let Some(t) = &mut self.timing {
            if self.upstream_hs_done && !self.marked_upstream_hs {
                self.marked_upstream_hs = true;
                t.mark("upstream handshake done");
            }
        }

        let mut buf = [0u8; 16384];
        loop {
            match self.client.reader().read(&mut buf) {
                Ok(0) => break,
                Ok(n) => self.to_guest.extend(&buf[..n]),
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(_) => break,
            }
        }

        if let Some(t) = &mut self.timing {
            if self.upstream_hs_done && !self.first_reply && !self.to_guest.is_empty() {
                self.first_reply = true;
                t.mark("first reply byte to guest (plaintext)");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tls_proxy::{
        ca_cert_pem, client_config_trusting, spawn_test_upstream, spawn_test_upstream_rst,
    };
    use super::*;
    use std::time::Duration;

    const UPSTREAM_REPLY: &[u8] = b"HTTP/1.0 200 OK\r\nContent-Length: 5\r\n\r\nhello";

    fn drain(gateway: &mut HttpsGateway, into: &mut Vec<u8>) {
        let mut buf = [0u8; 16384];
        while let Some(n) = gateway.pull_to_guest(&mut buf) {
            into.extend_from_slice(&buf[..n]);
        }
    }

    #[test]
    fn preamble_bridges_plaintext_to_real_tls_upstream() {
        let (port, up_ca, up) = spawn_test_upstream(UPSTREAM_REPLY);
        let ctx = TlsContext::new().unwrap();
        let mut gateway =
            HttpsGateway::new_test(&ctx, [127, 0, 0, 1], client_config_trusting(&up_ca));

        let wire = format!("VPOD-CONNECT localhost {port}\nGET / HTTP/1.0\r\n\r\n");
        for byte in wire.as_bytes() {
            gateway.push_from_guest(std::slice::from_ref(byte));
            assert!(!gateway.failed(), "gateway failed mid-preamble");
        }

        let mut got = Vec::new();
        for _ in 0..2000 {
            drain(&mut gateway, &mut got);
            if gateway.eof() {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        let _ = up.join();

        assert!(
            got.windows(5).any(|w| w == b"hello"),
            "expected upstream reply, got {got:?}"
        );
        assert!(gateway.eof(), "upstream close must surface as EOF");
    }

    #[test]
    fn abrupt_upstream_rst_still_delivers_full_reply() {
        const BODY_LEN: usize = 8000;
        static BIG_REPLY: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
        let reply: &'static [u8] = BIG_REPLY.get_or_init(|| {
            let mut v = format!("HTTP/1.0 200 OK\r\nContent-Length: {BODY_LEN}\r\n\r\n").into_bytes();
            v.extend(std::iter::repeat(b'x').take(BODY_LEN));
            v
        });

        let (port, up_ca, up) = spawn_test_upstream_rst(reply);
        let ctx = TlsContext::new().unwrap();
        let mut gateway =
            HttpsGateway::new_test(&ctx, [127, 0, 0, 1], client_config_trusting(&up_ca));

        let wire = format!("VPOD-CONNECT localhost {port}\nGET / HTTP/1.0\r\n\r\n");
        gateway.push_from_guest(wire.as_bytes());

        let mut got = Vec::new();
        for _ in 0..5000 {
            drain(&mut gateway, &mut got);
            if gateway.eof() {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        drain(&mut gateway, &mut got);
        let _ = up.join();

        assert!(!gateway.failed(), "RST after data must not fail the bridge");
        assert_eq!(
            got.len(),
            reply.len(),
            "reply truncated: got {} of {} bytes",
            got.len(),
            reply.len()
        );
    }

    #[test]
    fn client_hello_promotes_to_terminating_proxy() {
        use rustls::ClientConnection;
        use rustls::pki_types::ServerName;

        let ctx = TlsContext::new().unwrap();
        let mut gateway =
            HttpsGateway::new_test(&ctx, [127, 0, 0, 1], client_config_trusting(ca_cert_pem()));

        let guest_cfg = client_config_trusting(ca_cert_pem());
        let mut guest =
            ClientConnection::new(guest_cfg, ServerName::try_from("localhost").unwrap()).unwrap();

        let mut handshaken = false;
        for _ in 0..200 {
            let mut out = Vec::new();
            while guest.wants_write() {
                guest.write_tls(&mut out).unwrap();
            }

            for chunk in out.chunks(7) {
                gateway.push_from_guest(chunk);
            }

            let mut buf = [0u8; 16384];
            while let Some(n) = gateway.pull_to_guest(&mut buf) {
                let mut slice = &buf[..n];
                while !slice.is_empty() {
                    guest.read_tls(&mut slice).unwrap();
                }
                guest.process_new_packets().unwrap();
            }

            if !guest.is_handshaking() {
                handshaken = true;
                break;
            }
        }
        assert!(
            handshaken,
            "guest handshake did not complete through gateway"
        );
    }

    #[test]
    fn garbage_first_bytes_fail_like_a_bad_client_hello() {
        let ctx = TlsContext::new().unwrap();
        let mut gateway =
            HttpsGateway::new_test(&ctx, [127, 0, 0, 1], client_config_trusting(ca_cert_pem()));
        gateway.push_from_guest(b"GET / HTTP/1.0\r\n\r\n");
        assert!(gateway.failed(), "plaintext HTTP on :443 must be rejected");
    }

    #[test]
    fn malformed_preamble_fails_connection() {
        let ctx = TlsContext::new().unwrap();
        let mut gateway =
            HttpsGateway::new_test(&ctx, [127, 0, 0, 1], client_config_trusting(ca_cert_pem()));
        gateway.push_from_guest(b"VPOD-CONNECT missing-port\n");
        assert!(gateway.failed());
    }

    #[test]
    fn parse_preamble_accepts_host_and_port() {
        assert_eq!(
            parse_preamble(b"VPOD-CONNECT example.com 443\n"),
            Some(("example.com".to_string(), 443))
        );
        assert_eq!(
            parse_preamble(b"VPOD-CONNECT example.com 443\r\n"),
            Some(("example.com".to_string(), 443))
        );
    }

    #[test]
    fn parse_preamble_rejects_malformed_lines() {
        assert_eq!(parse_preamble(b"VPOD-CONNECT example.com\n"), None);
        assert_eq!(parse_preamble(b"VPOD-CONNECT  443\n"), None);
        assert_eq!(parse_preamble(b"VPOD-CONNECT a b c\n"), None);
        assert_eq!(parse_preamble(b"GET / HTTP/1.0\n"), None);
    }
}

#[cfg(test)]
mod native_probe {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::time::{Duration, Instant};

    #[test]
    #[ignore]
    fn plain_bridge_loop_against_real_host() {
        let ctx = TlsContext::new().unwrap();
        let stream = TcpStream::connect_timeout(
            &"172.66.147.243:443".parse().unwrap(),
            Duration::from_secs(5),
        )
        .unwrap();
        stream.set_nonblocking(true).unwrap();
        stream.set_nodelay(true).ok();
        let mut upstream = stream;
        let mut client = ClientConnection::new(
            ctx.upstream_config(),
            ServerName::try_from("example.com".to_string()).unwrap(),
        )
        .unwrap();
        client
            .writer()
            .write_all(b"GET / HTTP/1.0\r\nHost: example.com\r\n\r\n")
            .unwrap();

        let start = Instant::now();
        let mut got = Vec::new();
        while start.elapsed() < Duration::from_secs(8) {
            while client.wants_write() {
                match client.write_tls(&mut upstream) {
                    Ok(0) => break,
                    Ok(n) => println!("wrote {n}"),
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(e) => panic!("write err {e}"),
                }
            }
            match client.read_tls(&mut upstream) {
                Ok(0) => break,
                Ok(n) => {
                    println!("read {n}");
                    client.process_new_packets().unwrap();
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => panic!("read err {e}"),
            }
            let mut buf = [0u8; 4096];
            if let Ok(n) = client.reader().read(&mut buf) {
                got.extend_from_slice(&buf[..n]);
                if n > 0 {
                    println!("plaintext {n}");
                }
            }
            if got.len() > 100 {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        println!(
            "got {} plaintext bytes: {:?}",
            got.len(),
            String::from_utf8_lossy(&got[..got.len().min(60)])
        );
        assert!(!got.is_empty());
    }
}
