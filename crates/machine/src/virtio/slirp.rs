use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpStream, UdpSocket};
use std::collections::VecDeque;
use std::time::Duration;

use super::net::NetworkBackend;

const GW_IP:    [u8; 4] = [10, 0, 2, 2];
const GUEST_IP: [u8; 4] = [10, 0, 2, 15];
const SUBNET:   [u8; 4] = [255, 255, 255, 0];
const BCAST_IP: [u8; 4] = [10, 0, 2, 255];

const HOST_MAC:  [u8; 6] = [0x52, 0x54, 0x00, 0x00, 0x00, 0x01];
const BCAST_MAC: [u8; 6] = [0xff, 0xff, 0xff, 0xff, 0xff, 0xff];

const MSS: usize = 1460;

static IP_ID_COUNTER: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(1);



fn eth_src(f: &[u8]) -> &[u8] { &f[6..12] }
fn eth_type(f: &[u8]) -> u16  { u16::from_be_bytes([f[12], f[13]]) }

const ETHERTYPE_IP:  u16 = 0x0800;
const ETHERTYPE_ARP: u16 = 0x0806;

fn ip_proto(f: &[u8]) -> u8 { f[14 + 9] }
fn ip_src(f: &[u8])   -> [u8; 4] { f[14+12..14+16].try_into().unwrap() }
fn ip_dst(f: &[u8])   -> [u8; 4] { f[14+16..14+20].try_into().unwrap() }
fn ip_hlen(f: &[u8])  -> usize   { ((f[14] & 0x0f) as usize) * 4 }

fn ip_payload(f: &[u8]) -> &[u8] { &f[14 + ip_hlen(f)..] }

const IP_PROTO_ICMP: u8 = 1;
const IP_PROTO_TCP:  u8 = 6;
const IP_PROTO_UDP:  u8 = 17;

fn u16be(buf: &[u8], off: usize) -> u16 {
    u16::from_be_bytes([buf[off], buf[off+1]])
}

const SYN: u8 = 0x02;
const ACK: u8 = 0x10;
const FIN: u8 = 0x01;
const RST: u8 = 0x04;
const PSH: u8 = 0x08;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TcpState {
    Established,
    FinWait,
    Closed,
}

struct TcpConn {
    state:      TcpState,
    stream:     TcpStream,
    guest_mac:  [u8; 6],
    src_ip:     [u8; 4],
    dst_ip:     [u8; 4],
    src_port:   u16,
    dst_port:   u16,

    snd_buf:    VecDeque<u8>,
    snd_una:    u32,
    snd_nxt:    u32,

    write_buf:  VecDeque<u8>,

    rcv_nxt:    u32,
    rcv_wnd:    u32,
    wnd_shift:  u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct TcpKey {
    src_port: u16,
    dst_ip:   [u8; 4],
    dst_port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct UdpKey {
    src_port: u16,
    dst_ip:   [u8; 4],
    dst_port: u16,
}

struct UdpConn {
    sock:      UdpSocket,
    guest_mac: [u8; 6],
    src_ip:    [u8; 4],
    #[allow(dead_code)]
    src_port:  u16,
}

pub struct SlirpBackend {
    guest_mac:  [u8; 6],
    rx_pending: VecDeque<Vec<u8>>,
    tcp_conns:  HashMap<TcpKey, TcpConn>,
    udp_conns:  HashMap<UdpKey, UdpConn>,
    dhcp_xid:   u32,
}

impl SlirpBackend {
    pub fn new(guest_mac: [u8; 6]) -> Self {
        Self {
            guest_mac,
            rx_pending: VecDeque::new(),
            tcp_conns:  HashMap::new(),
            udp_conns:  HashMap::new(),
            dhcp_xid:   0,
        }
    }

    fn poll_tcp(&mut self) {
        let keys: Vec<TcpKey> = self.tcp_conns.keys().cloned().collect();
        for key in keys {
            let mut frames: Vec<Vec<u8>> = Vec::new();
            let mut remove = false;

            if let Some(conn) = self.tcp_conns.get_mut(&key) {
                match conn.state {
                    TcpState::Established => {
                        while !conn.write_buf.is_empty() {
                            let (a, b) = conn.write_buf.as_slices();
                            let slice = if !a.is_empty() { a } else { b };
                            match conn.stream.write(slice) {
                                Ok(n) => { conn.write_buf.drain(..n); }
                                Err(e) if would_block(&e) => break,
                                Err(_) => { remove = true; break; }
                            }
                        }

                        if remove { conn.state = TcpState::Closed; }

                        let mut buf = [0u8; MSS];
                        loop {
                            match conn.stream.read(&mut buf) {
                                Ok(0) => {
                                    if conn.snd_buf.is_empty() {
                                        frames.push(make_tcp_frame(
                                            &conn.guest_mac, &conn.dst_ip, &conn.src_ip,
                                            conn.dst_port, conn.src_port,
                                            conn.snd_nxt, conn.rcv_nxt, FIN | ACK, &[],
                                        ));
                                        conn.snd_nxt = conn.snd_nxt.wrapping_add(1);
                                        conn.state = TcpState::FinWait;
                                    } else {
                                        conn.state = TcpState::FinWait;
                                    }
                                    break;
                                }
                                Ok(n) => {
                                    conn.snd_buf.extend(&buf[..n]);
                                    if conn.snd_buf.len() >= 64 * 1024 {
                                        break;
                                    }
                                }
                                Err(e) if would_block(&e) => break,
                                Err(_) => {
                                    frames.push(make_tcp_frame(
                                        &conn.guest_mac, &conn.dst_ip, &conn.src_ip,
                                        conn.dst_port, conn.src_port,
                                        conn.snd_nxt, conn.rcv_nxt, RST | ACK, &[],
                                    ));
                                    remove = true;
                                    break;
                                }
                            }
                        }

                        let in_flight = conn.snd_nxt.wrapping_sub(conn.snd_una);
                        let can_send = conn.rcv_wnd.saturating_sub(in_flight) as usize;
                        let send_len = can_send.min(conn.snd_buf.len()).min(MSS);
                        if send_len > 0 {
                            let chunk: Vec<u8> = conn.snd_buf.drain(..send_len).collect();
                            frames.push(make_tcp_frame(
                                &conn.guest_mac, &conn.dst_ip, &conn.src_ip,
                                conn.dst_port, conn.src_port,
                                conn.snd_nxt, conn.rcv_nxt, PSH | ACK, &chunk,
                            ));
                            conn.snd_nxt = conn.snd_nxt.wrapping_add(send_len as u32);
                        }
                    }

                    TcpState::FinWait => {
                        let in_flight = conn.snd_nxt.wrapping_sub(conn.snd_una);
                        let can_send = conn.rcv_wnd.saturating_sub(in_flight) as usize;
                        let send_len = can_send.min(conn.snd_buf.len()).min(MSS);

                        if send_len > 0 {
                            let chunk: Vec<u8> = conn.snd_buf.drain(..send_len).collect();
                            frames.push(make_tcp_frame(
                                &conn.guest_mac, &conn.dst_ip, &conn.src_ip,
                                conn.dst_port, conn.src_port,
                                conn.snd_nxt, conn.rcv_nxt, PSH | ACK, &chunk,
                            ));
                            conn.snd_nxt = conn.snd_nxt.wrapping_add(send_len as u32);

                        } else if conn.snd_buf.is_empty() && conn.snd_nxt == conn.snd_una {
                            frames.push(make_tcp_frame(
                                &conn.guest_mac, &conn.dst_ip, &conn.src_ip,
                                conn.dst_port, conn.src_port,
                                conn.snd_nxt, conn.rcv_nxt, FIN | ACK, &[],
                            ));
                            conn.snd_nxt = conn.snd_nxt.wrapping_add(1);

                        } else if conn.snd_una == conn.snd_nxt {
                            remove = true;
                        }
                    }

                    TcpState::Closed => { remove = true; }
                }
            }

            for f in frames { self.rx_pending.push_back(f); }
            if remove { self.tcp_conns.remove(&key); }
        }
    }

    fn handle_arp(&mut self, frame: &[u8]) {
        if frame.len() < 14 + 28 { return; }
        let arp = &frame[14..];
        if u16be(arp, 6) != 1 { return; }
        let target_ip: [u8; 4] = arp[24..28].try_into().unwrap();
        if target_ip != GW_IP { return; }

        let sender_mac: [u8; 6] = arp[8..14].try_into().unwrap();
        let sender_ip:  [u8; 4] = arp[14..18].try_into().unwrap();

        let mut reply = vec![0u8; 14 + 28];
        reply[0..6].copy_from_slice(&sender_mac);
        reply[6..12].copy_from_slice(&HOST_MAC);
        reply[12..14].copy_from_slice(&ETHERTYPE_ARP.to_be_bytes());

        let a = &mut reply[14..];
        a[0..2].copy_from_slice(&1u16.to_be_bytes());
        a[2..4].copy_from_slice(&0x0800u16.to_be_bytes());
        a[4] = 6; a[5] = 4;
        a[6..8].copy_from_slice(&2u16.to_be_bytes());
        a[8..14].copy_from_slice(&HOST_MAC);
        a[14..18].copy_from_slice(&GW_IP);
        a[18..24].copy_from_slice(&sender_mac);
        a[24..28].copy_from_slice(&sender_ip);

        self.rx_pending.push_back(reply);
    }

    fn handle_ip(&mut self, frame: &[u8]) {
        if frame.len() < 14 + 20 { return; }
        match ip_proto(frame) {
            IP_PROTO_ICMP => self.handle_icmp(frame),
            IP_PROTO_TCP  => self.handle_tcp(frame),
            IP_PROTO_UDP  => self.handle_udp(frame),
            _ => {}
        }
    }

    fn handle_icmp(&mut self, frame: &[u8]) {
        if ip_dst(frame) != GW_IP { return; }
        let payload = ip_payload(frame);
        if payload.len() < 8 || payload[0] != 8 { return; }

        let src_mac: [u8; 6] = eth_src(frame).try_into().unwrap();
        let src_ip = ip_src(frame);
        let data = &payload[8..];

        let mut icmp = vec![0u8; 8 + data.len()];
        icmp[4..8].copy_from_slice(&payload[4..8]);
        icmp[8..].copy_from_slice(data);

        let csum = checksum(&icmp);
        icmp[2..4].copy_from_slice(&csum.to_be_bytes());
        self.rx_pending.push_back(make_ip_frame(&src_mac, &GW_IP, &src_ip, IP_PROTO_ICMP, &icmp));
    }

    fn handle_udp(&mut self, frame: &[u8]) {
        let payload = ip_payload(frame);
        if payload.len() < 8 {
            return;
        }

        let src_port = u16be(payload, 0);
        let dst_port = u16be(payload, 2);
        let udp_len  = u16be(payload, 4) as usize;
        let dst_ip   = ip_dst(frame);
        let src_ip   = ip_src(frame);
        let data     = &payload[8..udp_len.min(payload.len())];

        if dst_port == 67 && src_port == 68 {
            self.handle_dhcp(frame, data);
            return;
        }

        if dst_ip == GW_IP && dst_port == 53 {
            let guest_mac: [u8; 6] = eth_src(frame).try_into().unwrap();
            if let Ok(sock) = UdpSocket::bind("0.0.0.0:0") {
                sock.set_read_timeout(Some(Duration::from_millis(3000))).ok();
                let dst = SocketAddrV4::new(Ipv4Addr::new(8, 8, 8, 8), 53);
                if sock.send_to(data, dst).is_ok() {
                    let mut buf = vec![0u8; 2048];
                    if let Ok((n, _)) = sock.recv_from(&mut buf) {
                        let udp_reply = make_udp_payload(53, src_port, &buf[..n]);
                        self.rx_pending.push_back(
                            make_ip_frame(&guest_mac, &GW_IP, &src_ip, IP_PROTO_UDP, &udp_reply)
                        );
                    }
                }
            }
            return;
        }

        let key = UdpKey { src_port, dst_ip, dst_port };
        let src_mac: [u8; 6] = eth_src(frame).try_into().unwrap();
        let conn = self.udp_conns.entry(key.clone()).or_insert_with(|| {
            let sock = UdpSocket::bind("0.0.0.0:0").expect("udp bind");
            sock.set_nonblocking(true).ok();
            UdpConn { sock, guest_mac: src_mac, src_ip, src_port }
        });

        let dst = SocketAddrV4::new(Ipv4Addr::from(dst_ip), dst_port);
        let _ = conn.sock.send_to(data, dst);
        let mut buf = vec![0u8; 2048];

        loop {
            match conn.sock.recv_from(&mut buf) {
                Ok((n, _)) => {
                    let udp_reply = make_udp_payload(dst_port, src_port, &buf[..n]);
                    let pkt = make_ip_frame(&conn.guest_mac, &GW_IP, &conn.src_ip, IP_PROTO_UDP, &udp_reply);
                    self.rx_pending.push_back(pkt);
                }
                Err(_) => break,
            }
        }
    }

    fn handle_dhcp(&mut self, frame: &[u8], dhcp: &[u8]) {
        if dhcp.len() < 240 || dhcp[0] != 1 {
            return;
        }
        let xid = u32::from_be_bytes(dhcp[4..8].try_into().unwrap());
        self.dhcp_xid = xid;
        if u32::from_be_bytes(dhcp[236..240].try_into().unwrap()) != 0x63825363 {
            return;
        }

        let mut msg_type = 0u8;
        let mut i = 240usize;
        while i < dhcp.len() {
            let opt = dhcp[i];

            if opt == 255 {
                break;
            }

            if opt == 0 {
                i += 1;
                continue;
            }

            if i + 1 >= dhcp.len() {
                break;
            }

            let len = dhcp[i+1] as usize;
            if opt == 53 && len >= 1 {
                msg_type = dhcp[i+2];
            }
            i += 2 + len;
        }
        match msg_type {
            1 => self.send_dhcp_reply(frame, xid, 2),
            3 => self.send_dhcp_reply(frame, xid, 5),
            _ => {}
        }
    }

    fn send_dhcp_reply(&mut self, frame: &[u8], xid: u32, msg_type: u8) {
        let src_mac: [u8; 6] = eth_src(frame).try_into().unwrap();
        let reply = build_dhcp_reply(xid, &self.guest_mac.clone(), msg_type);
        let udp = make_udp_payload(68, 67, &reply);
        let mut pkt = make_ip_frame(&BCAST_MAC, &GW_IP, &BCAST_IP, IP_PROTO_UDP, &udp);
        pkt[0..6].copy_from_slice(&src_mac);

        self.rx_pending.push_back(pkt);
    }

    fn handle_tcp(&mut self, frame: &[u8]) {
        let payload = ip_payload(frame);
        if payload.len() < 20 { return; }

        let src_port  = u16be(payload, 0);
        let dst_port  = u16be(payload, 2);
        let seq_guest = u32::from_be_bytes(payload[4..8].try_into().unwrap());
        let ack_guest = u32::from_be_bytes(payload[8..12].try_into().unwrap());
        let flags     = payload[13];
        let window    = u16be(payload, 14);
        let dst_ip    = ip_dst(frame);
        let src_ip    = ip_src(frame);
        let tcp_hlen  = ((payload[12] >> 4) as usize) * 4;
        let data      = if tcp_hlen <= payload.len() { &payload[tcp_hlen..] } else { &[] };
        let src_mac: [u8; 6] = eth_src(frame).try_into().unwrap();
        let key = TcpKey { src_port, dst_ip, dst_port };

        if flags & RST != 0 {
            self.tcp_conns.remove(&key);
            return;
        }

        if flags & SYN != 0 && flags & ACK == 0 {
            if self.tcp_conns.contains_key(&key) { return; }

            let wnd_shift = parse_wnd_scale(payload, tcp_hlen);

            let addr = SocketAddrV4::new(Ipv4Addr::from(dst_ip), dst_port);
            let stream = match TcpStream::connect_timeout(
                &addr.into(), std::time::Duration::from_millis(100)
            ) {
                Ok(s) => s,
                Err(_) => {
                    self.rx_pending.push_back(make_tcp_frame(
                        &src_mac, &dst_ip, &src_ip, dst_port, src_port,
                        0, seq_guest.wrapping_add(1), RST | ACK, &[],
                    ));
                    return;
                }
            };
            stream.set_nonblocking(true).ok();
            stream.set_nodelay(true).ok();

            let isn_host: u32 = generate_isn(&src_ip, src_port, &dst_ip, dst_port);

            self.rx_pending.push_back(make_tcp_frame(
                &src_mac, &dst_ip, &src_ip, dst_port, src_port,
                isn_host, seq_guest.wrapping_add(1), SYN | ACK, &[],
            ));

            self.tcp_conns.insert(key, TcpConn {
                state:     TcpState::Established,
                stream,
                guest_mac: src_mac,
                src_ip,
                dst_ip,
                src_port,
                dst_port,
                snd_buf:   VecDeque::new(),
                snd_una:   isn_host,
                snd_nxt:   isn_host.wrapping_add(1),
                write_buf: VecDeque::new(),
                rcv_nxt:   seq_guest.wrapping_add(1),
                rcv_wnd:   (window as u32) << wnd_shift,
                wnd_shift,
            });
            return;
        }

        if let Some(conn) = self.tcp_conns.get_mut(&key) {
            conn.rcv_wnd = (window as u32) << conn.wnd_shift;

            if flags & ACK != 0 {
                if (ack_guest.wrapping_sub(conn.snd_una) as i32) > 0 {
                    conn.snd_una = ack_guest;
                }
                if conn.state == TcpState::FinWait && conn.snd_una == conn.snd_nxt {
                    conn.state = TcpState::Closed;
                }
            }

            if !data.is_empty() {
                let end_seq = seq_guest.wrapping_add(data.len() as u32);
                let new_bytes = end_seq.wrapping_sub(conn.rcv_nxt) as i32;
                if new_bytes > 0 {
                    let skip = data.len() - new_bytes as usize;
                    conn.write_buf.extend(&data[skip..]);
                    conn.rcv_nxt = end_seq;
                }
                self.rx_pending.push_back(make_tcp_frame(
                    &conn.guest_mac, &conn.dst_ip, &conn.src_ip,
                    conn.dst_port, conn.src_port,
                    conn.snd_nxt, conn.rcv_nxt, ACK, &[],
                ));
            }

            if flags & FIN != 0 {
                conn.rcv_nxt = conn.rcv_nxt.wrapping_add(1);
                self.rx_pending.push_back(make_tcp_frame(
                    &conn.guest_mac, &conn.dst_ip, &conn.src_ip,
                    conn.dst_port, conn.src_port,
                    conn.snd_nxt, conn.rcv_nxt, ACK, &[],
                ));
                conn.state = TcpState::Closed;
            }
        } else {
            self.rx_pending.push_back(make_tcp_frame(
                &src_mac, &dst_ip, &src_ip, dst_port, src_port,
                ack_guest, 0, RST, &[],
            ));
        }
    }
}

impl NetworkBackend for SlirpBackend {
    fn send(&mut self, frame: &[u8]) {
        if frame.len() < 14 { return; }
        match eth_type(frame) {
            ETHERTYPE_ARP => self.handle_arp(frame),
            ETHERTYPE_IP  => self.handle_ip(frame),
            _ => {}
        }
    }

    fn recv(&mut self) -> Option<Vec<u8>> {
        self.poll_tcp();
        self.rx_pending.pop_front()
    }

    fn has_rx(&self) -> bool {
        if !self.rx_pending.is_empty() {
            return true;
        }

        for conn in self.tcp_conns.values() {
            match conn.state {
                TcpState::Established | TcpState::FinWait =>
                    if !conn.snd_buf.is_empty() { return true; },
                TcpState::Closed => return true,
            }
        }
        false
    }
}

// Helpers

fn would_block(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::WouldBlock
    || e.kind() == std::io::ErrorKind::TimedOut
}

fn parse_wnd_scale(tcp_hdr: &[u8], tcp_hlen: usize) -> u8 {
    let mut i = 20;
    while i + 1 < tcp_hlen && i < tcp_hdr.len() {
        match tcp_hdr[i] {
            0 => i += 1,
            1 => i += 1,
            3 if i + 2 < tcp_hdr.len() && tcp_hdr[i + 1] == 3 => return tcp_hdr[i + 2].min(14),
            _ => {
                if i + 1 >= tcp_hdr.len() { break; }
                let len = tcp_hdr[i + 1] as usize;
                if len < 2 { break; }
                i += len;
                continue;
            }
        }
    }
    0
}

fn generate_isn(src_ip: &[u8; 4], src_port: u16, dst_ip: &[u8; 4], dst_port: u16) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for &b in src_ip.iter().chain(dst_ip.iter()) {
        h ^= b as u32; h = h.wrapping_mul(0x0100_0193);
    }
    h ^= src_port as u32; h = h.wrapping_mul(0x0100_0193);
    h ^= dst_port as u32; h = h.wrapping_mul(0x0100_0193);
    h
}


fn make_ip_frame(dst_mac: &[u8; 6], src_ip: &[u8; 4], dst_ip: &[u8; 4], proto: u8, payload: &[u8]) -> Vec<u8> {
    let total_len = (20 + payload.len()) as u16;
    let ip_id = IP_ID_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    let mut ip = vec![0u8; 20];
    ip[0] = 0x45;
    ip[2..4].copy_from_slice(&total_len.to_be_bytes());
    ip[4..6].copy_from_slice(&ip_id.to_be_bytes());
    ip[6..8].copy_from_slice(&0x4000u16.to_be_bytes());
    ip[8] = 64; ip[9] = proto;
    ip[12..16].copy_from_slice(src_ip);
    ip[16..20].copy_from_slice(dst_ip);

    let csum = checksum(&ip);
    ip[10..12].copy_from_slice(&csum.to_be_bytes());

    let mut frame = vec![0u8; 14 + 20 + payload.len()];
    frame[0..6].copy_from_slice(dst_mac);
    frame[6..12].copy_from_slice(&HOST_MAC);
    frame[12..14].copy_from_slice(&ETHERTYPE_IP.to_be_bytes());
    frame[14..34].copy_from_slice(&ip);
    frame[34..].copy_from_slice(payload);
    frame
}

fn make_udp_payload(src_port: u16, dst_port: u16, data: &[u8]) -> Vec<u8> {
    let len = (8 + data.len()) as u16;

    let mut udp = vec![0u8; 8 + data.len()];
    udp[0..2].copy_from_slice(&src_port.to_be_bytes());
    udp[2..4].copy_from_slice(&dst_port.to_be_bytes());
    udp[4..6].copy_from_slice(&len.to_be_bytes());
    udp[8..].copy_from_slice(data);

    udp
}

fn make_tcp_frame(
    dst_mac:  &[u8; 6],
    src_ip:   &[u8; 4],
    dst_ip:   &[u8; 4],
    src_port: u16,
    dst_port: u16,
    seq:      u32,
    ack:      u32,
    flags:    u8,
    data:     &[u8],
) -> Vec<u8> {
    let opts: &[u8] = if flags & SYN != 0 { &[0x02, 0x04, 0x05, 0xb4] } else { &[] };
    let tcp_hlen = 20 + opts.len();
    let tcp_len  = tcp_hlen + data.len();

    let mut tcp  = vec![0u8; tcp_len];
    tcp[0..2].copy_from_slice(&src_port.to_be_bytes());
    tcp[2..4].copy_from_slice(&dst_port.to_be_bytes());
    tcp[4..8].copy_from_slice(&seq.to_be_bytes());
    tcp[8..12].copy_from_slice(&ack.to_be_bytes());
    tcp[12] = ((tcp_hlen / 4) as u8) << 4;
    tcp[13] = flags;
    tcp[14..16].copy_from_slice(&65535u16.to_be_bytes());
    tcp[20..20 + opts.len()].copy_from_slice(opts);
    tcp[tcp_hlen..].copy_from_slice(data);

    let mut pseudo = vec![0u8; 12 + tcp_len];
    pseudo[0..4].copy_from_slice(src_ip);
    pseudo[4..8].copy_from_slice(dst_ip);
    pseudo[9] = IP_PROTO_TCP;
    pseudo[10..12].copy_from_slice(&(tcp_len as u16).to_be_bytes());
    pseudo[12..].copy_from_slice(&tcp);
    let csum = checksum(&pseudo);
    tcp[16..18].copy_from_slice(&csum.to_be_bytes());

    make_ip_frame(dst_mac, src_ip, dst_ip, IP_PROTO_TCP, &tcp)
}

fn build_dhcp_reply(xid: u32, client_mac: &[u8; 6], msg_type: u8) -> Vec<u8> {
    let mut p = vec![0u8; 300];
    p[0] = 2; p[1] = 1; p[2] = 6;
    p[4..8].copy_from_slice(&xid.to_be_bytes());
    p[16..20].copy_from_slice(&GUEST_IP);
    p[20..24].copy_from_slice(&GW_IP);
    p[28..34].copy_from_slice(client_mac);
    p[236..240].copy_from_slice(&0x63825363u32.to_be_bytes());
    let mut i = 240usize;
    p[i] = 53; p[i+1] = 1; p[i+2] = msg_type; i += 3;
    p[i] = 54; p[i+1] = 4; p[i+2..i+6].copy_from_slice(&GW_IP); i += 6;
    p[i] = 51; p[i+1] = 4; p[i+2..i+6].copy_from_slice(&86400u32.to_be_bytes()); i += 6;
    p[i] = 1;  p[i+1] = 4; p[i+2..i+6].copy_from_slice(&SUBNET); i += 6;
    p[i] = 3;  p[i+1] = 4; p[i+2..i+6].copy_from_slice(&GW_IP); i += 6;
    p[i] = 6;  p[i+1] = 4; p[i+2..i+6].copy_from_slice(&GW_IP); i += 6;
    p[i] = 255;
    p
}

fn checksum(data: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut i = 0;
    while i + 1 < data.len() {
        sum += u16::from_be_bytes([data[i], data[i+1]]) as u32;
        i += 2;
    }

    if i < data.len() {
        sum += (data[i] as u32) << 8;
    }

    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }

    !(sum as u16)
}
