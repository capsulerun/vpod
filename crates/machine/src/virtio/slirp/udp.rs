// Guest datagrams. DHCP and DNS are answered here; anything else gets a
// short-lived host socket.

use std::collections::hash_map::Entry;
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};

use super::SlirpBackend;
use super::frames::{
    GW_IP, IP_PROTO_UDP, eth_src, ip_dst, ip_payload, ip_src, make_ip_frame, make_udp_payload,
    u16be,
};

const DHCP_SERVER_PORT: u16 = 67;
const DHCP_CLIENT_PORT: u16 = 68;
const DNS_PORT: u16 = 53;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct UdpKey {
    src_port: u16,
    dst_ip: [u8; 4],
    dst_port: u16,
}

pub(super) struct UdpConn {
    sock: UdpSocket,
    guest_mac: [u8; 6],
    src_ip: [u8; 4],
}

impl SlirpBackend {
    pub(super) fn handle_udp(&mut self, frame: &[u8]) {
        let payload = ip_payload(frame);
        if payload.len() < 8 {
            return;
        }

        let src_port = u16be(payload, 0);
        let dst_port = u16be(payload, 2);
        let udp_len = u16be(payload, 4) as usize;
        if udp_len < 8 {
            return;
        }

        let dst_ip = ip_dst(frame);
        let src_ip = ip_src(frame);
        let guest_mac: [u8; 6] = eth_src(frame).try_into().unwrap();
        let data = &payload[8..udp_len.min(payload.len())];

        if dst_port == DHCP_SERVER_PORT && src_port == DHCP_CLIENT_PORT {
            self.handle_dhcp(frame, data);
            return;
        }

        if dst_ip == GW_IP && dst_port == DNS_PORT {
            self.handle_dns(data, guest_mac, src_ip, src_port);
            return;
        }

        self.relay_datagram(guest_mac, src_ip, src_port, dst_ip, dst_port, data);
    }

    fn relay_datagram(
        &mut self,
        guest_mac: [u8; 6],
        src_ip: [u8; 4],
        src_port: u16,
        dst_ip: [u8; 4],
        dst_port: u16,
        data: &[u8],
    ) {
        let key = UdpKey {
            src_port,
            dst_ip,
            dst_port,
        };

        let conn = match self.udp_conns.entry(key) {
            Entry::Occupied(existing) => existing.into_mut(),
            Entry::Vacant(slot) => {
                let Ok(sock) = UdpSocket::bind("0.0.0.0:0") else {
                    log::debug!("slirp: no udp available, dropping datagram to port {dst_port}");
                    return;
                };
                sock.set_nonblocking(true).ok();

                slot.insert(UdpConn {
                    sock,
                    guest_mac,
                    src_ip,
                })
            }
        };

        let destination = SocketAddrV4::new(Ipv4Addr::from(dst_ip), dst_port);
        let _ = conn.sock.send_to(data, destination);

        let mut buf = vec![0u8; 2048];
        while let Ok((n, _)) = conn.sock.recv_from(&mut buf) {
            let reply = make_udp_payload(dst_port, src_port, &buf[..n]);
            self.rx_pending.push_back(make_ip_frame(
                &conn.guest_mac,
                &GW_IP,
                &conn.src_ip,
                IP_PROTO_UDP,
                &reply,
            ));
        }
    }
}
