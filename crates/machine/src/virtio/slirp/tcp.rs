// The guest's TCP connections, and the two directions they are pumped in.

use std::collections::{BTreeMap, VecDeque};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpStream};

#[cfg(not(target_family = "wasm"))]
use std::time::Duration;

use super::SlirpBackend;
use super::frames::{
    ACK, Endpoints, FIN, PSH, RST, SYN, eth_src, ip_dst, ip_payload, ip_src, make_tcp_frame, u16be,
    would_block,
};
use crate::virtio::https_gateway::HttpsGateway;

const HTTPS_PORT: u16 = 443;
const MSS: usize = 1460;
const MAX_SND_BUF: usize = 128 * 1024;
const MAX_OOO_BYTES: usize = 256 * 1024;

pub(super) enum Transport {
    Raw(TcpStream),
    Https(Box<HttpsGateway>),
}

impl Transport {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Transport::Raw(s) => s.write(buf),
            Transport::Https(g) => {
                g.push_from_guest(buf);
                if g.failed() {
                    Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
                } else {
                    Ok(buf.len())
                }
            }
        }
    }

    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Transport::Raw(s) => s.read(buf),
            Transport::Https(g) => {
                if g.failed() {
                    return Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe));
                }
                match g.pull_to_guest(buf) {
                    Some(n) => Ok(n),
                    None if g.eof() => Ok(0),
                    None => Err(std::io::Error::from(std::io::ErrorKind::WouldBlock)),
                }
            }
        }
    }

    pub(super) fn has_pending(&self) -> bool {
        match self {
            Transport::Raw(_) => false,
            Transport::Https(g) => g.has_pending(),
        }
    }

    fn shutdown_write(&mut self) {
        match self {
            Transport::Raw(s) => {
                s.shutdown(std::net::Shutdown::Write).ok();
            }
            Transport::Https(g) => g.shutdown_write(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TcpState {
    Established,
    FinWait,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct TcpKey {
    pub src_port: u16,
    pub dst_ip: [u8; 4],
    pub dst_port: u16,
}

#[derive(PartialEq, Eq)]
pub(super) enum Serviced {
    Alive,
    Finished,
}

pub(super) struct TcpConn {
    pub state: TcpState,
    pub transport: Transport,
    to_guest: Endpoints,

    snd_buf: VecDeque<u8>,
    snd_una: u32,
    snd_nxt: u32,
    fin_sent: bool,

    write_buf: VecDeque<u8>,
    guest_finished_writing: bool,

    rcv_nxt: u32,
    rcv_wnd: u32,
    wnd_shift: u8,

    ooo_buf: BTreeMap<u32, Vec<u8>>,
}

impl TcpConn {
    pub(super) fn has_queued_output(&self) -> bool {
        !self.snd_buf.is_empty()
    }

    fn frame(&self, flags: u8, data: &[u8]) -> Vec<u8> {
        make_tcp_frame(&self.to_guest, self.snd_nxt, self.rcv_nxt, flags, data)
    }

    fn in_flight(&self) -> u32 {
        self.snd_nxt.wrapping_sub(self.snd_una)
    }

    /// Both directions, plus whatever the guest is owed. `Finished` once the
    /// connection can be dropped.
    pub(super) fn service(&mut self, frames: &mut Vec<Vec<u8>>) -> Serviced {
        match self.state {
            TcpState::Established => {
                let mut serviced = Serviced::Alive;

                if self.forward_guest_writes() {
                    self.forward_half_close();
                    if !self.read_upstream(frames) {
                        serviced = Serviced::Finished;
                    }
                } else {
                    self.state = TcpState::Closed;
                    serviced = Serviced::Finished;
                }

                self.drain_snd_buf(frames);
                serviced
            }

            TcpState::FinWait => {
                self.drain_snd_buf(frames);
                self.send_fin_if_due(frames);
                Serviced::Alive
            }

            TcpState::Closed => Serviced::Finished,
        }
    }

    /// False once the upstream can no longer be written to.
    fn forward_guest_writes(&mut self) -> bool {
        while !self.write_buf.is_empty() {
            let (front, back) = self.write_buf.as_slices();
            let pending = if front.is_empty() { back } else { front };

            match self.transport.write(pending) {
                Ok(written) => {
                    self.write_buf.drain(..written);
                }
                Err(e) if would_block(&e) => return true,
                Err(_) => return false,
            }
        }

        true
    }

    /// Held until the guest's own writes have drained, because shutting the
    /// write side down discards whatever is still queued.
    fn forward_half_close(&mut self) {
        if self.guest_finished_writing && self.write_buf.is_empty() {
            self.guest_finished_writing = false;
            self.transport.shutdown_write();
        }
    }

    /// False once the upstream failed, after telling the guest.
    fn read_upstream(&mut self, frames: &mut Vec<Vec<u8>>) -> bool {
        let mut buf = [0u8; 16384];

        loop {
            match self.transport.read(&mut buf) {
                Ok(0) => {
                    if self.snd_buf.is_empty() {
                        self.send_fin(frames);
                    }
                    self.state = TcpState::FinWait;
                    return true;
                }
                Ok(n) => {
                    self.snd_buf.extend(&buf[..n]);
                    if self.snd_buf.len() >= MAX_SND_BUF {
                        return true;
                    }
                }
                Err(e) if would_block(&e) => return true,
                Err(_) => {
                    frames.push(self.frame(RST | ACK, &[]));
                    return false;
                }
            }
        }
    }

    fn drain_snd_buf(&mut self, frames: &mut Vec<Vec<u8>>) {
        loop {
            let can_send = self.rcv_wnd.saturating_sub(self.in_flight()) as usize;
            if can_send == 0 || self.snd_buf.is_empty() {
                break;
            }

            let send_len = can_send.min(self.snd_buf.len()).min(MSS);
            let chunk: Vec<u8> = self.snd_buf.drain(..send_len).collect();

            frames.push(self.frame(PSH | ACK, &chunk));
            self.snd_nxt = self.snd_nxt.wrapping_add(send_len as u32);
        }
    }

    fn send_fin(&mut self, frames: &mut Vec<Vec<u8>>) {
        frames.push(self.frame(FIN | ACK, &[]));
        self.snd_nxt = self.snd_nxt.wrapping_add(1);
        self.fin_sent = true;
    }

    /// The FIN owed to the guest, once the reply ahead of it has been sent and
    /// acknowledged.
    fn send_fin_if_due(&mut self, frames: &mut Vec<Vec<u8>>) {
        if self.fin_sent || !self.snd_buf.is_empty() || self.in_flight() != 0 {
            return;
        }

        self.send_fin(frames);
        self.state = TcpState::Closed;
    }

    fn on_guest_window(&mut self, window: u16) {
        self.rcv_wnd = (window as u32) << self.wnd_shift;
    }

    /// Only our own FIN being acknowledged closes the connection. A guest
    /// acknowledging the last of the reply must not, or the FIN it is still
    /// owed never goes out.
    fn on_guest_ack(&mut self, ack: u32) {
        if (ack.wrapping_sub(self.snd_una) as i32) > 0 {
            self.snd_una = ack;
        }

        if self.state == TcpState::FinWait && self.fin_sent && self.snd_una == self.snd_nxt {
            self.state = TcpState::Closed;
        }
    }

    fn on_guest_data(&mut self, seq: u32, data: &[u8]) {
        let end_seq = seq.wrapping_add(data.len() as u32);
        let new_bytes = end_seq.wrapping_sub(self.rcv_nxt) as i32;
        let gap = seq.wrapping_sub(self.rcv_nxt) as i32;

        if new_bytes > 0 && gap <= 0 {
            let skip = data.len() - new_bytes as usize;
            self.write_buf.extend(&data[skip..]);
            self.rcv_nxt = end_seq;
            self.drain_ooo_buf();
            return;
        }

        if gap > 0 {
            let buffered: usize = self.ooo_buf.values().map(Vec::len).sum();
            if buffered + data.len() <= MAX_OOO_BYTES {
                self.ooo_buf.entry(seq).or_insert_with(|| data.to_vec());
            }
        }
    }

    /// The guest will not write again. This says nothing about the FIN we owe
    /// it, which may still be queued behind a reply.
    fn on_guest_fin(&mut self) {
        self.rcv_nxt = self.rcv_nxt.wrapping_add(1);

        match self.state {
            TcpState::Established => self.guest_finished_writing = true,
            TcpState::FinWait if !self.fin_sent => {}
            TcpState::FinWait | TcpState::Closed => self.state = TcpState::Closed,
        }
    }

    fn drain_ooo_buf(&mut self) {
        loop {
            let mut progressed = false;

            for seq in self.ooo_buf.keys().cloned().collect::<Vec<u32>>() {
                let data_len = self.ooo_buf[&seq].len() as u32;
                let end_seq = seq.wrapping_add(data_len);
                let gap = seq.wrapping_sub(self.rcv_nxt) as i32;
                let new_bytes = end_seq.wrapping_sub(self.rcv_nxt) as i32;

                if gap > 0 {
                    continue;
                }

                let data = self.ooo_buf.remove(&seq).unwrap();
                if new_bytes > 0 {
                    let skip = data.len() - new_bytes as usize;
                    self.write_buf.extend(&data[skip..]);
                    self.rcv_nxt = end_seq;
                    progressed = true;
                }
            }

            if !progressed {
                break;
            }
        }
    }
}

impl SlirpBackend {
    pub(super) fn poll_tcp(&mut self) {
        for key in self.tcp_conns.keys().cloned().collect::<Vec<TcpKey>>() {
            let mut frames: Vec<Vec<u8>> = Vec::new();

            let Some(conn) = self.tcp_conns.get_mut(&key) else {
                continue;
            };

            if conn.service(&mut frames) == Serviced::Finished {
                self.tcp_conns.remove(&key);
            }

            self.rx_pending.extend(frames);
        }
    }

    pub(super) fn handle_tcp(&mut self, frame: &[u8]) {
        let payload = ip_payload(frame);
        if payload.len() < 20 {
            return;
        }

        let src_port = u16be(payload, 0);
        let dst_port = u16be(payload, 2);
        let seq_guest = u32::from_be_bytes(payload[4..8].try_into().unwrap());
        let ack_guest = u32::from_be_bytes(payload[8..12].try_into().unwrap());
        let flags = payload[13];
        let window = u16be(payload, 14);
        let dst_ip = ip_dst(frame);
        let src_ip = ip_src(frame);
        let tcp_hlen = ((payload[12] >> 4) as usize) * 4;
        let data = if tcp_hlen <= payload.len() {
            &payload[tcp_hlen..]
        } else {
            &[]
        };
        let src_mac: [u8; 6] = eth_src(frame).try_into().unwrap();

        let key = TcpKey {
            src_port,
            dst_ip,
            dst_port,
        };

        // The guest dialled src -> dst, so everything we send back reverses it.
        let to_guest = Endpoints {
            dst_mac: src_mac,
            src_ip: dst_ip,
            src_port: dst_port,
            dst_ip: src_ip,
            dst_port: src_port,
        };

        if flags & RST != 0 {
            self.tcp_conns.remove(&key);
            return;
        }

        if flags & SYN != 0 && flags & ACK == 0 {
            self.open_connection(key, to_guest, payload, tcp_hlen, seq_guest, window);
            return;
        }

        let Some(conn) = self.tcp_conns.get_mut(&key) else {
            self.rx_pending
                .push_back(make_tcp_frame(&to_guest, ack_guest, 0, RST, &[]));
            return;
        };

        conn.on_guest_window(window);

        if flags & ACK != 0 {
            conn.on_guest_ack(ack_guest);
        }

        if !data.is_empty() {
            conn.on_guest_data(seq_guest, data);
            self.rx_pending.push_back(conn.frame(ACK, &[]));
        }

        if flags & FIN != 0 {
            conn.on_guest_fin();
            self.rx_pending.push_back(conn.frame(ACK, &[]));
        }
    }

    fn open_connection(
        &mut self,
        key: TcpKey,
        to_guest: Endpoints,
        payload: &[u8],
        tcp_hlen: usize,
        seq_guest: u32,
        window: u16,
    ) {
        if self.tcp_conns.contains_key(&key) {
            return;
        }

        let Some(transport) = self.connect(&to_guest) else {
            self.rx_pending.push_back(make_tcp_frame(
                &to_guest,
                0,
                seq_guest.wrapping_add(1),
                RST | ACK,
                &[],
            ));
            return;
        };

        let wnd_shift = parse_wnd_scale(payload, tcp_hlen);
        let isn_host = generate_isn(&to_guest);

        self.rx_pending.push_back(make_tcp_frame(
            &to_guest,
            isn_host,
            seq_guest.wrapping_add(1),
            SYN | ACK,
            &[],
        ));

        self.tcp_conns.insert(
            key,
            TcpConn {
                state: TcpState::Established,
                transport,
                to_guest,
                snd_buf: VecDeque::new(),
                snd_una: isn_host,
                snd_nxt: isn_host.wrapping_add(1),
                fin_sent: false,
                write_buf: VecDeque::new(),
                guest_finished_writing: false,
                rcv_nxt: seq_guest.wrapping_add(1),
                rcv_wnd: (window as u32) << wnd_shift,
                wnd_shift,
                ooo_buf: BTreeMap::new(),
            },
        );
    }

    /// `:443` goes through the gateway, which terminates the guest's TLS.
    /// Everything else is an ordinary outbound socket.
    fn connect(&self, to_guest: &Endpoints) -> Option<Transport> {
        let host_ip = to_guest.src_ip;
        let host_port = to_guest.src_port;

        if let Some(ctx) = self.tls.as_ref().filter(|_| host_port == HTTPS_PORT) {
            return Some(Transport::Https(Box::new(HttpsGateway::new(ctx, host_ip))));
        }

        let address = SocketAddrV4::new(Ipv4Addr::from(host_ip), host_port);

        #[cfg(target_family = "wasm")]
        let stream = TcpStream::connect(address);

        #[cfg(not(target_family = "wasm"))]
        let stream = TcpStream::connect_timeout(&address.into(), Duration::from_secs(5));

        let stream = stream.ok()?;
        stream.set_nonblocking(true).ok();
        stream.set_nodelay(true).ok();

        Some(Transport::Raw(stream))
    }
}

fn parse_wnd_scale(tcp_hdr: &[u8], tcp_hlen: usize) -> u8 {
    let mut i = 20;

    while i < tcp_hlen && i < tcp_hdr.len() {
        match tcp_hdr[i] {
            0 => i += 1,
            1 => i += 1,
            3 => {
                if i + 2 < tcp_hdr.len() && tcp_hdr[i + 1] == 3 {
                    return tcp_hdr[i + 2].min(14);
                }

                break;
            }
            _ => {
                if i + 1 >= tcp_hdr.len() {
                    break;
                }

                let len = tcp_hdr[i + 1] as usize;
                if len < 2 {
                    break;
                }

                i += len;
            }
        }
    }

    0
}

fn generate_isn(to_guest: &Endpoints) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;

    for &byte in to_guest.dst_ip.iter().chain(to_guest.src_ip.iter()) {
        hash ^= byte as u32;
        hash = hash.wrapping_mul(0x0100_0193);
    }

    hash ^= to_guest.dst_port as u32;
    hash = hash.wrapping_mul(0x0100_0193);
    hash ^= to_guest.src_port as u32;
    hash.wrapping_mul(0x0100_0193)
}
