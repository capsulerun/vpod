// A user-mode network for the guest: it speaks ethernet to the guest and asks the host only for ordinary outbound sockets.

mod dhcp;
mod dns;
mod frames;
mod tcp;
mod udp;

use std::collections::{HashMap, VecDeque};

use dns::DnsPending;
use frames::{
    ETHERTYPE_ARP, ETHERTYPE_IP, GW_IP, HOST_MAC, IP_PROTO_ICMP, IP_PROTO_TCP, IP_PROTO_UDP,
    checksum, eth_src, eth_type, ip_dst, ip_payload, ip_proto, ip_src, make_ip_frame, u16be,
};
use tcp::{TcpConn, TcpKey, TcpState};
use udp::{UdpConn, UdpKey};

use super::net::NetworkBackend;
use super::tls_proxy::TlsContext;

pub struct SlirpBackend {
    guest_mac: [u8; 6],
    rx_pending: VecDeque<Vec<u8>>,
    tcp_conns: HashMap<TcpKey, TcpConn>,
    udp_conns: HashMap<UdpKey, UdpConn>,
    dns_pending: Vec<DnsPending>,
    dhcp_xid: u32,
    tls: Option<TlsContext>,
}

impl SlirpBackend {
    pub fn new(guest_mac: [u8; 6]) -> Self {
        let tls = match TlsContext::new() {
            Ok(ctx) => Some(ctx),
            Err(e) => {
                log::warn!("tls_proxy: disabled, terminator init failed: {e}");
                None
            }
        };

        Self {
            guest_mac,
            rx_pending: VecDeque::new(),
            tcp_conns: HashMap::new(),
            udp_conns: HashMap::new(),
            dns_pending: Vec::new(),
            dhcp_xid: 0,
            tls,
        }
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

        let body = &mut reply[14..];
        body[0..2].copy_from_slice(&1u16.to_be_bytes());
        body[2..4].copy_from_slice(&ETHERTYPE_IP.to_be_bytes());
        body[4] = 6;
        body[5] = 4;
        body[6..8].copy_from_slice(&2u16.to_be_bytes());
        body[8..14].copy_from_slice(&HOST_MAC);
        body[14..18].copy_from_slice(&GW_IP);
        body[18..24].copy_from_slice(&sender_mac);
        body[24..28].copy_from_slice(&sender_ip);

        self.rx_pending.push_back(reply);
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
        if !self.rx_pending.is_empty() || !self.dns_pending.is_empty() {
            return true;
        }

        self.tcp_conns.values().any(|conn| match conn.state {
            TcpState::Established | TcpState::FinWait => {
                conn.has_queued_output() || conn.transport.has_pending()
            }
            TcpState::Closed => true,
        })
    }

    fn has_active_connections(&self) -> bool {
        self.tcp_conns
            .values()
            .any(|conn| matches!(conn.state, TcpState::Established | TcpState::FinWait))
    }
}

#[cfg(test)]
mod tests {
    use super::frames::{ACK, Endpoints, FIN, GUEST_IP, SYN, make_tcp_frame};
    use super::*;
    use std::io::{Read, Write};
    use std::time::{Duration, Instant};

    const FLAGS_OFFSET: usize = 14 + 20 + 13;

    fn from_guest(guest_mac: [u8; 6], dst_ip: [u8; 4], src_port: u16, dst_port: u16) -> Endpoints {
        Endpoints {
            dst_mac: guest_mac,
            src_ip: GUEST_IP,
            src_port,
            dst_ip,
            dst_port,
        }
    }

    fn connect(
        slirp: &mut SlirpBackend,
        listener: &std::net::TcpListener,
        ends: &Endpoints,
        guest_isn: u32,
    ) -> (std::net::TcpStream, u32) {
        slirp.send(&make_tcp_frame(ends, guest_isn, 0, SYN, &[]));

        let (upstream, _) = listener.accept().unwrap();
        upstream.set_nonblocking(true).unwrap();

        let syn_ack = slirp.recv().expect("SYN-ACK");
        let host_isn = u32::from_be_bytes(syn_ack[38..42].try_into().unwrap());

        (upstream, host_isn.wrapping_add(1))
    }

    fn read_upstream(
        upstream: &mut std::net::TcpStream,
        slirp: &mut SlirpBackend,
        want: usize,
    ) -> Vec<u8> {
        let mut got = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(5);

        while got.len() < want && Instant::now() < deadline {
            while slirp.recv().is_some() {}

            let mut buf = [0u8; 64];
            match upstream.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => got.extend_from_slice(&buf[..n]),
                Err(_) => std::thread::sleep(Duration::from_millis(5)),
            }
        }

        got
    }

    fn saw_fin(slirp: &mut SlirpBackend, within: Duration) -> bool {
        let deadline = Instant::now() + within;

        while Instant::now() < deadline {
            while let Some(frame) = slirp.recv() {
                if frame.len() > FLAGS_OFFSET && frame[FLAGS_OFFSET] & FIN != 0 {
                    return true;
                }
            }
            std::thread::sleep(Duration::from_millis(2));
        }

        false
    }

    #[test]
    fn out_of_order_guest_segments_are_reassembled_before_upstream_write() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let guest_mac = [0x02, 0, 0, 0, 0, 0x15];
        let mut slirp = SlirpBackend::new(guest_mac);
        let ends = from_guest(guest_mac, [127, 0, 0, 1], 45001, port);
        let guest_isn = 1000u32;

        let (mut upstream, ack) = connect(&mut slirp, &listener, &ends, guest_isn);

        let part1 = b"hello ";
        let part2 = b"world";
        let seq1 = guest_isn.wrapping_add(1);
        let seq2 = seq1.wrapping_add(part1.len() as u32);

        slirp.send(&make_tcp_frame(&ends, seq2, ack, ACK, part2));
        slirp.send(&make_tcp_frame(&ends, seq1, ack, ACK, part1));

        let got = read_upstream(&mut upstream, &mut slirp, part1.len() + part2.len());
        assert_eq!(got, b"hello world");
    }

    #[test]
    fn an_upstream_eof_becomes_a_fin_to_the_guest() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let guest_mac = [0x02, 0, 0, 0, 0, 0x17];
        let mut slirp = SlirpBackend::new(guest_mac);
        let ends = from_guest(guest_mac, [127, 0, 0, 1], 45003, port);
        let guest_isn = 3000u32;

        let (mut upstream, ack) = connect(&mut slirp, &listener, &ends, guest_isn);

        let request = b"GET / HTTP/1.0\r\n\r\n";
        slirp.send(&make_tcp_frame(
            &ends,
            guest_isn.wrapping_add(1),
            ack,
            ACK,
            request,
        ));

        let seen = read_upstream(&mut upstream, &mut slirp, request.len());
        assert_eq!(seen, request, "the request never reached the upstream");

        drop(upstream);

        assert!(
            saw_fin(&mut slirp, Duration::from_secs(5)),
            "the upstream ended and the guest was never sent a FIN"
        );
    }

    #[test]
    fn a_guest_fin_becomes_an_upstream_eof() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let guest_mac = [0x02, 0, 0, 0, 0, 0x16];
        let mut slirp = SlirpBackend::new(guest_mac);
        let ends = from_guest(guest_mac, [127, 0, 0, 1], 45002, port);
        let guest_isn = 2000u32;

        let (mut upstream, ack) = connect(&mut slirp, &listener, &ends, guest_isn);

        let request = b"GET / HTTP/1.0\r\n\r\n";
        let seq = guest_isn.wrapping_add(1);
        slirp.send(&make_tcp_frame(&ends, seq, ack, ACK, request));
        slirp.send(&make_tcp_frame(
            &ends,
            seq.wrapping_add(request.len() as u32),
            ack,
            FIN | ACK,
            &[],
        ));

        let mut got = Vec::new();
        let mut buf = [0u8; 64];
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut saw_eof = false;

        while Instant::now() < deadline {
            while slirp.recv().is_some() {}

            match upstream.read(&mut buf) {
                Ok(0) => {
                    saw_eof = true;
                    break;
                }
                Ok(n) => got.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(e) => panic!("upstream read failed: {e}"),
            }
        }

        assert_eq!(got, request, "the request itself must arrive intact");
        assert!(saw_eof, "the guest's FIN was not forwarded");
    }

    #[test]
    fn a_deferred_fin_survives_the_guest_half_closing_first() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let guest_mac = [0x02, 0, 0, 0, 0, 0x18];
        let mut slirp = SlirpBackend::new(guest_mac);
        let ends = from_guest(guest_mac, [127, 0, 0, 1], 45004, port);
        let guest_isn = 4000u32;

        let (mut upstream, ack) = connect(&mut slirp, &listener, &ends, guest_isn);

        let request = b"GET / HTTP/1.1\r\nConnection: close\r\n\r\n";
        slirp.send(&make_tcp_frame(
            &ends,
            guest_isn.wrapping_add(1),
            ack,
            ACK,
            request,
        ));

        let seen = read_upstream(&mut upstream, &mut slirp, request.len());
        assert_eq!(seen, request, "the request never reached the upstream");

        let reply = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhi";
        upstream.write_all(reply).unwrap();
        drop(upstream);

        let mut delivered = 0u32;
        let mut fin_before_half_close = false;
        let deadline = Instant::now() + Duration::from_secs(2);

        while delivered < reply.len() as u32 && Instant::now() < deadline {
            while let Some(frame) = slirp.recv() {
                delivered += frame.len().saturating_sub(14 + 20 + 20) as u32;
                if frame.len() > FLAGS_OFFSET && frame[FLAGS_OFFSET] & FIN != 0 {
                    fin_before_half_close = true;
                }
            }
            std::thread::sleep(Duration::from_millis(2));
        }

        assert_eq!(
            delivered,
            reply.len() as u32,
            "the reply never reached the guest"
        );
        assert!(
            !fin_before_half_close,
            "the FIN was not deferred, so this no longer covers the deferred case"
        );

        slirp.send(&make_tcp_frame(
            &ends,
            guest_isn.wrapping_add(1 + request.len() as u32),
            ack.wrapping_add(delivered),
            FIN | ACK,
            &[],
        ));

        assert!(
            saw_fin(&mut slirp, Duration::from_secs(2)),
            "the guest half-closed before the deferred FIN went out, so it never \
             arrives: the guest waits in FIN_WAIT2 and its ssl_client never exits"
        );
    }
}
