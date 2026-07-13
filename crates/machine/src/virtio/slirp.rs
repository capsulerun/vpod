use std::collections::HashMap;
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpStream, ToSocketAddrs, UdpSocket};
use std::time::{Duration, Instant};

use super::net::NetworkBackend;

const GW_IP: [u8; 4] = [10, 0, 2, 2];
const GUEST_IP: [u8; 4] = [10, 0, 2, 15];
const SUBNET: [u8; 4] = [255, 255, 255, 0];
const BCAST_IP: [u8; 4] = [10, 0, 2, 255];

const HOST_MAC: [u8; 6] = [0x52, 0x54, 0x00, 0x00, 0x00, 0x01];
const BCAST_MAC: [u8; 6] = [0xff, 0xff, 0xff, 0xff, 0xff, 0xff];

const MSS: usize = 1460;

fn eth_src(f: &[u8]) -> &[u8] {
    &f[6..12]
}
fn eth_type(f: &[u8]) -> u16 {
    u16::from_be_bytes([f[12], f[13]])
}

const ETHERTYPE_IP: u16 = 0x0800;
const ETHERTYPE_ARP: u16 = 0x0806;

fn ip_proto(f: &[u8]) -> u8 {
    f[14 + 9]
}

fn ip_src(f: &[u8]) -> [u8; 4] {
    f[14 + 12..14 + 16].try_into().unwrap()
}

fn ip_dst(f: &[u8]) -> [u8; 4] {
    f[14 + 16..14 + 20].try_into().unwrap()
}

fn ip_hlen(f: &[u8]) -> usize {
    ((f[14] & 0x0f) as usize) * 4
}

fn ip_payload(f: &[u8]) -> &[u8] {
    &f[14 + ip_hlen(f)..]
}

const IP_PROTO_ICMP: u8 = 1;
const IP_PROTO_TCP: u8 = 6;
const IP_PROTO_UDP: u8 = 17;

fn u16be(buf: &[u8], off: usize) -> u16 {
    u16::from_be_bytes([buf[off], buf[off + 1]])
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
    state: TcpState,
    stream: TcpStream,
    guest_mac: [u8; 6],
    src_ip: [u8; 4],
    dst_ip: [u8; 4],
    src_port: u16,
    dst_port: u16,

    snd_buf: VecDeque<u8>,
    snd_una: u32,
    snd_nxt: u32,

    write_buf: VecDeque<u8>,

    rcv_nxt: u32,
    rcv_wnd: u32,
    wnd_shift: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct TcpKey {
    src_port: u16,
    dst_ip: [u8; 4],
    dst_port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct UdpKey {
    src_port: u16,
    dst_ip: [u8; 4],
    dst_port: u16,
}

struct UdpConn {
    sock: UdpSocket,
    guest_mac: [u8; 6],
    src_ip: [u8; 4],
    #[allow(dead_code)]
    src_port: u16,
}

struct DnsPending {
    sock: UdpSocket,
    guest_mac: [u8; 6],
    src_ip: [u8; 4],
    src_port: u16,
    query: Vec<u8>,
    created: Instant,
}

pub struct SlirpBackend {
    guest_mac: [u8; 6],
    rx_pending: VecDeque<Vec<u8>>,
    tcp_conns: HashMap<TcpKey, TcpConn>,
    udp_conns: HashMap<UdpKey, UdpConn>,
    dns_pending: Vec<DnsPending>,
    dhcp_xid: u32,
}

impl SlirpBackend {
    pub fn new(guest_mac: [u8; 6]) -> Self {
        Self {
            guest_mac,
            rx_pending: VecDeque::new(),
            tcp_conns: HashMap::new(),
            udp_conns: HashMap::new(),
            dns_pending: Vec::new(),
            dhcp_xid: 0,
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
                                Ok(n) => {
                                    conn.write_buf.drain(..n);
                                }
                                Err(e) if would_block(&e) => break,
                                Err(_) => {
                                    remove = true;
                                    break;
                                }
                            }
                        }

                        if remove {
                            conn.state = TcpState::Closed;
                        }

                        if !remove {
                            let mut buf = [0u8; 16384];
                            loop {
                                match conn.stream.read(&mut buf) {
                                    Ok(0) => {
                                        if conn.snd_buf.is_empty() {
                                            frames.push(make_tcp_frame(
                                                &conn.guest_mac,
                                                &conn.dst_ip,
                                                &conn.src_ip,
                                                conn.dst_port,
                                                conn.src_port,
                                                conn.snd_nxt,
                                                conn.rcv_nxt,
                                                FIN | ACK,
                                                &[],
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
                                        if conn.snd_buf.len() >= 128 * 1024 {
                                            break;
                                        }
                                    }
                                    Err(e) if would_block(&e) => break,
                                    Err(_) => {
                                        frames.push(make_tcp_frame(
                                            &conn.guest_mac,
                                            &conn.dst_ip,
                                            &conn.src_ip,
                                            conn.dst_port,
                                            conn.src_port,
                                            conn.snd_nxt,
                                            conn.rcv_nxt,
                                            RST | ACK,
                                            &[],
                                        ));
                                        remove = true;
                                        break;
                                    }
                                }
                            }
                        }

                        Self::drain_snd_buf(conn, &mut frames);
                    }

                    TcpState::FinWait => {
                        Self::drain_snd_buf(conn, &mut frames);

                        if conn.snd_buf.is_empty() {
                            let in_flight = conn.snd_nxt.wrapping_sub(conn.snd_una);
                            if in_flight == 0 {
                                frames.push(make_tcp_frame(
                                    &conn.guest_mac,
                                    &conn.dst_ip,
                                    &conn.src_ip,
                                    conn.dst_port,
                                    conn.src_port,
                                    conn.snd_nxt,
                                    conn.rcv_nxt,
                                    FIN | ACK,
                                    &[],
                                ));

                                conn.snd_nxt = conn.snd_nxt.wrapping_add(1);
                                conn.state = TcpState::Closed;
                            }
                        }
                    }

                    TcpState::Closed => {
                        remove = true;
                    }
                }
            }

            for f in frames {
                self.rx_pending.push_back(f);
            }

            if remove {
                self.tcp_conns.remove(&key);
            }
        }
    }

    fn drain_snd_buf(conn: &mut TcpConn, frames: &mut Vec<Vec<u8>>) {
        loop {
            let in_flight = conn.snd_nxt.wrapping_sub(conn.snd_una);
            let can_send = conn.rcv_wnd.saturating_sub(in_flight) as usize;
            if can_send == 0 || conn.snd_buf.is_empty() {
                break;
            }

            let send_len = can_send.min(conn.snd_buf.len()).min(MSS);
            let chunk: Vec<u8> = conn.snd_buf.drain(..send_len).collect();

            frames.push(make_tcp_frame(
                &conn.guest_mac,
                &conn.dst_ip,
                &conn.src_ip,
                conn.dst_port,
                conn.src_port,
                conn.snd_nxt,
                conn.rcv_nxt,
                PSH | ACK,
                &chunk,
            ));
            conn.snd_nxt = conn.snd_nxt.wrapping_add(send_len as u32);
        }
    }

    fn poll_dns(&mut self) {
        let mut finished = Vec::new();

        for (i, req) in self.dns_pending.iter().enumerate() {
            let mut buf = [0u8; 2048];

            if let Ok((n, _)) = req.sock.recv_from(&mut buf) {
                let udp_reply = make_udp_payload(53, req.src_port, &buf[..n]);
                self.rx_pending.push_back(make_ip_frame(
                    &req.guest_mac,
                    &GW_IP,
                    &req.src_ip,
                    IP_PROTO_UDP,
                    &udp_reply,
                ));
                finished.push(i);
                continue;
            }

            if req.created.elapsed() > DNS_RELAY_TIMEOUT {
                if let Some(question) = parse_dns_question(&req.query) {
                    let servfail =
                        build_dns_reply(&req.query, &question, &[], DNS_RCODE_SERVFAIL);
                    let udp_reply = make_udp_payload(53, req.src_port, &servfail);
                    self.rx_pending.push_back(make_ip_frame(
                        &req.guest_mac,
                        &GW_IP,
                        &req.src_ip,
                        IP_PROTO_UDP,
                        &udp_reply,
                    ));
                }
                finished.push(i);
            }
        }

        for i in finished.into_iter().rev() {
            self.dns_pending.swap_remove(i);
        }
    }

    fn handle_arp(&mut self, frame: &[u8]) {
        if frame.len() < 14 + 28 {
            return;
        }

        let arp = &frame[14..];
        if u16be(arp, 6) != 1 {
            return;
        }

        let target_ip: [u8; 4] = arp[24..28].try_into().unwrap();
        if target_ip != GW_IP {
            return;
        }

        let sender_mac: [u8; 6] = arp[8..14].try_into().unwrap();
        let sender_ip: [u8; 4] = arp[14..18].try_into().unwrap();

        let mut reply = vec![0u8; 14 + 28];
        reply[0..6].copy_from_slice(&sender_mac);
        reply[6..12].copy_from_slice(&HOST_MAC);
        reply[12..14].copy_from_slice(&ETHERTYPE_ARP.to_be_bytes());

        let a = &mut reply[14..];
        a[0..2].copy_from_slice(&1u16.to_be_bytes());
        a[2..4].copy_from_slice(&0x0800u16.to_be_bytes());
        a[4] = 6;
        a[5] = 4;
        a[6..8].copy_from_slice(&2u16.to_be_bytes());
        a[8..14].copy_from_slice(&HOST_MAC);
        a[14..18].copy_from_slice(&GW_IP);
        a[18..24].copy_from_slice(&sender_mac);
        a[24..28].copy_from_slice(&sender_ip);
        self.rx_pending.push_back(reply);
    }

    fn handle_ip(&mut self, frame: &[u8]) {
        if frame.len() < 14 + 20 {
            return;
        }
        match ip_proto(frame) {
            IP_PROTO_ICMP => self.handle_icmp(frame),
            IP_PROTO_TCP => self.handle_tcp(frame),
            IP_PROTO_UDP => self.handle_udp(frame),
            _ => {}
        }
    }

    fn handle_icmp(&mut self, frame: &[u8]) {
        if ip_dst(frame) != GW_IP {
            return;
        }
        let payload = ip_payload(frame);
        if payload.len() < 8 || payload[0] != 8 {
            return;
        }

        let src_mac: [u8; 6] = eth_src(frame).try_into().unwrap();
        let src_ip = ip_src(frame);
        let data = &payload[8..];

        let mut icmp = vec![0u8; 8 + data.len()];
        icmp[4..8].copy_from_slice(&payload[4..8]);
        icmp[8..].copy_from_slice(data);

        let csum = checksum(&icmp);
        icmp[2..4].copy_from_slice(&csum.to_be_bytes());
        self.rx_pending.push_back(make_ip_frame(
            &src_mac,
            &GW_IP,
            &src_ip,
            IP_PROTO_ICMP,
            &icmp,
        ));
    }

    fn handle_udp(&mut self, frame: &[u8]) {
        let payload = ip_payload(frame);
        if payload.len() < 8 {
            return;
        }
        let src_port = u16be(payload, 0);
        let dst_port = u16be(payload, 2);
        let udp_len = u16be(payload, 4) as usize;
        let dst_ip = ip_dst(frame);
        let src_ip = ip_src(frame);
        let data = &payload[8..udp_len.min(payload.len())];

        if dst_port == 67 && src_port == 68 {
            self.handle_dhcp(frame, data);
            return;
        }

        if dst_ip == GW_IP && dst_port == 53 {
            let guest_mac: [u8; 6] = eth_src(frame).try_into().unwrap();

            if let Some(reply) = answer_dns_with_host_resolver(data) {
                let udp_reply = make_udp_payload(53, src_port, &reply);
                self.rx_pending.push_back(make_ip_frame(
                    &guest_mac,
                    &GW_IP,
                    &src_ip,
                    IP_PROTO_UDP,
                    &udp_reply,
                ));
                return;
            }

            if let Ok(sock) = UdpSocket::bind("0.0.0.0:0") {
                sock.set_nonblocking(true).ok();

                let dns_servers = [
                    (1, 1, 1, 1),        // Cloudflare DNS
                    (8, 8, 8, 8),        // Google DNS
                    (208, 67, 222, 222), // OpenDNS
                ];

                let mut sent = false;
                for server in dns_servers.iter() {
                    let socket_addr = SocketAddrV4::new(
                        Ipv4Addr::new(server.0, server.1, server.2, server.3),
                        53,
                    );

                    if sock.send_to(data, socket_addr).is_ok() {
                        sent = true;
                        break;
                    }
                }

                if sent {
                    self.dns_pending.push(DnsPending {
                        sock,
                        guest_mac,
                        src_ip,
                        src_port,
                        query: data.to_vec(),
                        created: Instant::now(),
                    });
                }
            }

            return;
        }

        let key = UdpKey {
            src_port,
            dst_ip,
            dst_port,
        };
        let src_mac: [u8; 6] = eth_src(frame).try_into().unwrap();
        let conn = self.udp_conns.entry(key.clone()).or_insert_with(|| {
            let sock = UdpSocket::bind("0.0.0.0:0").expect("udp bind");
            sock.set_nonblocking(true).ok();

            UdpConn {
                sock,
                guest_mac: src_mac,
                src_ip,
                src_port,
            }
        });

        let dst = SocketAddrV4::new(Ipv4Addr::from(dst_ip), dst_port);
        let _ = conn.sock.send_to(data, dst);
        let mut buf = vec![0u8; 2048];

        while let Ok((n, _)) = conn.sock.recv_from(&mut buf) {
            let udp_reply = make_udp_payload(dst_port, src_port, &buf[..n]);
            let pkt = make_ip_frame(
                &conn.guest_mac,
                &GW_IP,
                &conn.src_ip,
                IP_PROTO_UDP,
                &udp_reply,
            );
            self.rx_pending.push_back(pkt);
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

            let len = dhcp[i + 1] as usize;
            if opt == 53 && len >= 1 {
                msg_type = dhcp[i + 2];
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

        if flags & RST != 0 {
            self.tcp_conns.remove(&key);
            return;
        }

        if flags & SYN != 0 && flags & ACK == 0 {
            if self.tcp_conns.contains_key(&key) {
                return;
            }

            let wnd_shift = parse_wnd_scale(payload, tcp_hlen);

            let addr = SocketAddrV4::new(Ipv4Addr::from(dst_ip), dst_port);

            #[cfg(target_family = "wasm")]
            let stream_result = TcpStream::connect(addr);

            #[cfg(not(target_family = "wasm"))]
            let stream_result = TcpStream::connect_timeout(&addr.into(), Duration::from_secs(5));

            let stream = match stream_result {
                Ok(s) => s,
                Err(_) => {
                    self.rx_pending.push_back(make_tcp_frame(
                        &src_mac,
                        &dst_ip,
                        &src_ip,
                        dst_port,
                        src_port,
                        0,
                        seq_guest.wrapping_add(1),
                        RST | ACK,
                        &[],
                    ));
                    return;
                }
            };
            stream.set_nonblocking(true).ok();
            stream.set_nodelay(true).ok();

            let isn_host: u32 = generate_isn(&src_ip, src_port, &dst_ip, dst_port);

            self.rx_pending.push_back(make_tcp_frame(
                &src_mac,
                &dst_ip,
                &src_ip,
                dst_port,
                src_port,
                isn_host,
                seq_guest.wrapping_add(1),
                SYN | ACK,
                &[],
            ));

            self.tcp_conns.insert(
                key,
                TcpConn {
                    state: TcpState::Established,
                    stream,
                    guest_mac: src_mac,
                    src_ip,
                    dst_ip,
                    src_port,
                    dst_port,
                    snd_buf: VecDeque::new(),
                    snd_una: isn_host,
                    snd_nxt: isn_host.wrapping_add(1),
                    write_buf: VecDeque::new(),
                    rcv_nxt: seq_guest.wrapping_add(1),
                    rcv_wnd: (window as u32) << wnd_shift,
                    wnd_shift,
                },
            );

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
                    &conn.guest_mac,
                    &conn.dst_ip,
                    &conn.src_ip,
                    conn.dst_port,
                    conn.src_port,
                    conn.snd_nxt,
                    conn.rcv_nxt,
                    ACK,
                    &[],
                ));
            }

            if flags & FIN != 0 {
                conn.rcv_nxt = conn.rcv_nxt.wrapping_add(1);
                self.rx_pending.push_back(make_tcp_frame(
                    &conn.guest_mac,
                    &conn.dst_ip,
                    &conn.src_ip,
                    conn.dst_port,
                    conn.src_port,
                    conn.snd_nxt,
                    conn.rcv_nxt,
                    ACK,
                    &[],
                ));
                conn.state = TcpState::Closed;
            }
        } else {
            self.rx_pending.push_back(make_tcp_frame(
                &src_mac,
                &dst_ip,
                &src_ip,
                dst_port,
                src_port,
                ack_guest,
                0,
                RST,
                &[],
            ));
        }
    }
}

impl NetworkBackend for SlirpBackend {
    fn send(&mut self, frame: &[u8]) {
        if frame.len() < 14 {
            return;
        }
        match eth_type(frame) {
            ETHERTYPE_ARP => self.handle_arp(frame),
            ETHERTYPE_IP => self.handle_ip(frame),
            _ => {}
        }
    }

    fn recv(&mut self) -> Option<Vec<u8>> {
        self.poll_tcp();
        self.poll_dns();
        self.rx_pending.pop_front()
    }

    fn has_rx(&self) -> bool {
        if !self.rx_pending.is_empty() {
            return true;
        }
        if !self.dns_pending.is_empty() {
            return true;
        }
        for conn in self.tcp_conns.values() {
            match conn.state {
                TcpState::Established | TcpState::FinWait => {
                    if !conn.snd_buf.is_empty() {
                        return true;
                    }
                }
                TcpState::Closed => return true,
            }
        }
        false
    }

    fn has_active_connections(&self) -> bool {
        self.tcp_conns
            .values()
            .any(|c| matches!(c.state, TcpState::Established | TcpState::FinWait))
    }
}

fn would_block(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut
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
                continue;
            }
        }
    }
    0
}

fn generate_isn(src_ip: &[u8; 4], src_port: u16, dst_ip: &[u8; 4], dst_port: u16) -> u32 {
    let mut h: u32 = 0x811c_9dc5;

    for &b in src_ip.iter().chain(dst_ip.iter()) {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }

    h ^= src_port as u32;
    h = h.wrapping_mul(0x0100_0193);
    h ^= dst_port as u32;
    h = h.wrapping_mul(0x0100_0193);
    h
}

static IP_ID_COUNTER: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(1);

fn make_ip_frame(
    dst_mac: &[u8; 6],
    src_ip: &[u8; 4],
    dst_ip: &[u8; 4],
    proto: u8,
    payload: &[u8],
) -> Vec<u8> {
    let total_len = (20 + payload.len()) as u16;
    let ip_id = IP_ID_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    let mut ip = vec![0u8; 20];
    ip[0] = 0x45;
    ip[2..4].copy_from_slice(&total_len.to_be_bytes());
    ip[4..6].copy_from_slice(&ip_id.to_be_bytes());
    ip[6..8].copy_from_slice(&0x4000u16.to_be_bytes());
    ip[8] = 64;
    ip[9] = proto;
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

const DNS_RELAY_TIMEOUT: Duration = Duration::from_secs(5);
const DNS_QTYPE_A: u16 = 1;
const DNS_QTYPE_AAAA: u16 = 28;
const DNS_RCODE_NOERROR: u16 = 0;
const DNS_RCODE_SERVFAIL: u16 = 2;
const DNS_MAX_ANSWERS: usize = 4;

struct DnsQuestion {
    hostname: String,
    qtype: u16,
    question_end: usize,
}

fn parse_dns_question(query: &[u8]) -> Option<DnsQuestion> {
    if query.len() < 12 {
        return None;
    }

    let question_count = u16::from_be_bytes([query[4], query[5]]);
    if question_count != 1 {
        return None;
    }

    let mut position = 12;
    let mut hostname = String::new();

    loop {
        let label_len = *query.get(position)? as usize;
        if label_len == 0 {
            position += 1;
            break;
        }

        if label_len & 0xC0 != 0 {
            return None;
        }

        let label = query.get(position + 1..position + 1 + label_len)?;
        if !hostname.is_empty() {
            hostname.push('.');
        }
        hostname.push_str(std::str::from_utf8(label).ok()?);
        if hostname.len() > 253 {
            return None;
        }

        position += 1 + label_len;
    }

    let qtype = u16::from_be_bytes([*query.get(position)?, *query.get(position + 1)?]);
    let question_end = position + 4; // qtype + qclass

    if query.len() < question_end {
        return None;
    }

    Some(DnsQuestion {
        hostname,
        qtype,
        question_end,
    })
}

fn answer_dns_with_host_resolver(query: &[u8]) -> Option<Vec<u8>> {
    let question = parse_dns_question(query)?;

    match question.qtype {
        DNS_QTYPE_A => {
            let resolved = (question.hostname.as_str(), 0u16).to_socket_addrs().ok()?;
            let ipv4_addresses: Vec<[u8; 4]> = resolved
                .filter_map(|address| match address {
                    SocketAddr::V4(v4) => Some(v4.ip().octets()),
                    SocketAddr::V6(_) => None,
                })
                .take(DNS_MAX_ANSWERS)
                .collect();

            if ipv4_addresses.is_empty() {
                return None;
            }

            Some(build_dns_reply(
                query,
                &question,
                &ipv4_addresses,
                DNS_RCODE_NOERROR,
            ))
        }

        DNS_QTYPE_AAAA => Some(build_dns_reply(query, &question, &[], DNS_RCODE_NOERROR)),
        _ => None,
    }
}

fn build_dns_reply(
    query: &[u8],
    question: &DnsQuestion,
    ipv4_answers: &[[u8; 4]],
    rcode: u16,
) -> Vec<u8> {
    let mut reply = Vec::with_capacity(question.question_end + ipv4_answers.len() * 16);

    reply.extend_from_slice(&query[0..2]); // transaction id
    let flags: u16 = 0x8180 | rcode; // response + recursion desired + available
    reply.extend_from_slice(&flags.to_be_bytes());
    reply.extend_from_slice(&1u16.to_be_bytes()); // questions
    reply.extend_from_slice(&(ipv4_answers.len() as u16).to_be_bytes());
    reply.extend_from_slice(&0u16.to_be_bytes()); // authority records
    reply.extend_from_slice(&0u16.to_be_bytes()); // additional records
    reply.extend_from_slice(&query[12..question.question_end]);

    for address in ipv4_answers {
        reply.extend_from_slice(&[0xC0, 0x0C]); // pointer to question name
        reply.extend_from_slice(&DNS_QTYPE_A.to_be_bytes());
        reply.extend_from_slice(&1u16.to_be_bytes()); // class IN
        reply.extend_from_slice(&60u32.to_be_bytes()); // ttl
        reply.extend_from_slice(&4u16.to_be_bytes()); // rdata length
        reply.extend_from_slice(address);
    }

    reply
}

// to refacto the function in order to avoid to many argument
#[allow(clippy::too_many_arguments)]
fn make_tcp_frame(
    dst_mac: &[u8; 6],
    src_ip: &[u8; 4],
    dst_ip: &[u8; 4],
    src_port: u16,
    dst_port: u16,
    seq: u32,
    ack: u32,
    flags: u8,
    data: &[u8],
) -> Vec<u8> {
    let opts: &[u8] = if flags & SYN != 0 {
        &[0x02, 0x04, 0x05, 0xb4, 0x01, 0x03, 0x03, 0x07]
    } else {
        &[]
    };
    let tcp_hlen = 20 + opts.len();
    let tcp_len = tcp_hlen + data.len();

    let mut tcp = vec![0u8; tcp_len];
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
    p[0] = 2;
    p[1] = 1;
    p[2] = 6;
    p[4..8].copy_from_slice(&xid.to_be_bytes());
    p[16..20].copy_from_slice(&GUEST_IP);
    p[20..24].copy_from_slice(&GW_IP);
    p[28..34].copy_from_slice(client_mac);
    p[236..240].copy_from_slice(&0x63825363u32.to_be_bytes());

    let mut i = 240usize;
    p[i] = 53;
    p[i + 1] = 1;
    p[i + 2] = msg_type;

    i += 3;
    p[i] = 54;
    p[i + 1] = 4;
    p[i + 2..i + 6].copy_from_slice(&GW_IP);

    i += 6;
    p[i] = 51;
    p[i + 1] = 4;
    p[i + 2..i + 6].copy_from_slice(&86400u32.to_be_bytes());

    i += 6;
    p[i] = 1;
    p[i + 1] = 4;
    p[i + 2..i + 6].copy_from_slice(&SUBNET);

    i += 6;
    p[i] = 3;
    p[i + 1] = 4;
    p[i + 2..i + 6].copy_from_slice(&GW_IP);

    i += 6;
    p[i] = 6;
    p[i + 1] = 4;
    p[i + 2..i + 6].copy_from_slice(&GW_IP);

    i += 6;
    p[i] = 255;
    p
}

fn checksum(data: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut i = 0;

    while i + 1 < data.len() {
        sum += u16::from_be_bytes([data[i], data[i + 1]]) as u32;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Standard query for example.com, type A: 12-byte header, labels,
    /// qtype, qclass.
    fn example_com_query(qtype: u16) -> Vec<u8> {
        let mut query = vec![
            0xAB, 0xCD, // transaction id
            0x01, 0x00, // recursion desired
            0x00, 0x01, // 1 question
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        query.extend_from_slice(b"\x07example\x03com\x00");
        query.extend_from_slice(&qtype.to_be_bytes());
        query.extend_from_slice(&1u16.to_be_bytes()); // class IN
        query
    }

    #[test]
    fn parses_hostname_and_qtype() {
        let query = example_com_query(DNS_QTYPE_A);
        let question = parse_dns_question(&query).unwrap();
        assert_eq!(question.hostname, "example.com");
        assert_eq!(question.qtype, DNS_QTYPE_A);
        assert_eq!(question.question_end, query.len());
    }

    #[test]
    fn rejects_truncated_and_multi_question_queries() {
        assert!(parse_dns_question(&[0u8; 5]).is_none());

        let mut two_questions = example_com_query(DNS_QTYPE_A);
        two_questions[5] = 2;
        assert!(parse_dns_question(&two_questions).is_none());
    }

    #[test]
    fn reply_echoes_id_and_question_with_answers() {
        let query = example_com_query(DNS_QTYPE_A);
        let question = parse_dns_question(&query).unwrap();
        let reply = build_dns_reply(&query, &question, &[[93, 184, 215, 14]], DNS_RCODE_NOERROR);

        assert_eq!(&reply[0..2], &[0xAB, 0xCD]); // same transaction id
        assert_eq!(u16::from_be_bytes([reply[2], reply[3]]) & 0x8000, 0x8000); // response bit
        assert_eq!(u16::from_be_bytes([reply[6], reply[7]]), 1); // one answer
        assert_eq!(&reply[12..question.question_end], &query[12..]); // question echoed
        assert_eq!(&reply[reply.len() - 4..], &[93, 184, 215, 14]); // rdata
    }

    #[test]
    fn servfail_reply_has_rcode_and_no_answers() {
        let query = example_com_query(DNS_QTYPE_A);
        let question = parse_dns_question(&query).unwrap();
        let reply = build_dns_reply(&query, &question, &[], DNS_RCODE_SERVFAIL);

        assert_eq!(u16::from_be_bytes([reply[2], reply[3]]) & 0x000F, DNS_RCODE_SERVFAIL);
        assert_eq!(u16::from_be_bytes([reply[6], reply[7]]), 0);
    }

    #[test]
    fn aaaa_query_gets_empty_noerror_without_touching_resolver() {
        let query = example_com_query(DNS_QTYPE_AAAA);
        let reply = answer_dns_with_host_resolver(&query).unwrap();

        assert_eq!(u16::from_be_bytes([reply[2], reply[3]]) & 0x000F, DNS_RCODE_NOERROR);
        assert_eq!(u16::from_be_bytes([reply[6], reply[7]]), 0); // no answers
    }
}
