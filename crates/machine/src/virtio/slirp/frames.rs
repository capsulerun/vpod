// Ethernet, IP, TCP and UDP on the wire.

pub(super) const GW_IP: [u8; 4] = [10, 0, 2, 2];
pub(super) const GUEST_IP: [u8; 4] = [10, 0, 2, 15];
pub(super) const SUBNET: [u8; 4] = [255, 255, 255, 0];
pub(super) const BCAST_IP: [u8; 4] = [10, 0, 2, 255];

pub(super) const HOST_MAC: [u8; 6] = [0x52, 0x54, 0x00, 0x00, 0x00, 0x01];
pub(super) const BCAST_MAC: [u8; 6] = [0xff, 0xff, 0xff, 0xff, 0xff, 0xff];

pub(super) const ETHERTYPE_IP: u16 = 0x0800;
pub(super) const ETHERTYPE_ARP: u16 = 0x0806;

pub(super) const IP_PROTO_ICMP: u8 = 1;
pub(super) const IP_PROTO_TCP: u8 = 6;
pub(super) const IP_PROTO_UDP: u8 = 17;

pub(super) const SYN: u8 = 0x02;
pub(super) const ACK: u8 = 0x10;
pub(super) const FIN: u8 = 0x01;
pub(super) const RST: u8 = 0x04;
pub(super) const PSH: u8 = 0x08;

pub(super) fn eth_src(frame: &[u8]) -> &[u8] {
    &frame[6..12]
}

pub(super) fn eth_type(frame: &[u8]) -> u16 {
    u16::from_be_bytes([frame[12], frame[13]])
}

pub(super) fn ip_proto(frame: &[u8]) -> u8 {
    frame[14 + 9]
}

pub(super) fn ip_src(frame: &[u8]) -> [u8; 4] {
    frame[14 + 12..14 + 16].try_into().unwrap()
}

pub(super) fn ip_dst(frame: &[u8]) -> [u8; 4] {
    frame[14 + 16..14 + 20].try_into().unwrap()
}

pub(super) fn ip_hlen(frame: &[u8]) -> usize {
    ((frame[14] & 0x0f) as usize) * 4
}

pub(super) fn ip_payload(frame: &[u8]) -> &[u8] {
    &frame[14 + ip_hlen(frame)..]
}

pub(super) fn u16be(buf: &[u8], offset: usize) -> u16 {
    u16::from_be_bytes([buf[offset], buf[offset + 1]])
}

pub(super) fn would_block(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut
}

pub(super) struct Endpoints {
    pub dst_mac: [u8; 6],
    pub src_ip: [u8; 4],
    pub src_port: u16,
    pub dst_ip: [u8; 4],
    pub dst_port: u16,
}

static IP_ID_COUNTER: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(1);

pub(super) fn make_ip_frame(
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

pub(super) fn make_udp_payload(src_port: u16, dst_port: u16, data: &[u8]) -> Vec<u8> {
    let len = (8 + data.len()) as u16;

    let mut udp = vec![0u8; 8 + data.len()];
    udp[0..2].copy_from_slice(&src_port.to_be_bytes());
    udp[2..4].copy_from_slice(&dst_port.to_be_bytes());
    udp[4..6].copy_from_slice(&len.to_be_bytes());
    udp[8..].copy_from_slice(data);
    udp
}

pub(super) fn make_tcp_frame(
    ends: &Endpoints,
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
    tcp[0..2].copy_from_slice(&ends.src_port.to_be_bytes());
    tcp[2..4].copy_from_slice(&ends.dst_port.to_be_bytes());
    tcp[4..8].copy_from_slice(&seq.to_be_bytes());
    tcp[8..12].copy_from_slice(&ack.to_be_bytes());
    tcp[12] = ((tcp_hlen / 4) as u8) << 4;
    tcp[13] = flags;
    tcp[14..16].copy_from_slice(&65535u16.to_be_bytes());
    tcp[20..20 + opts.len()].copy_from_slice(opts);
    tcp[tcp_hlen..].copy_from_slice(data);

    let mut pseudo = vec![0u8; 12 + tcp_len];
    pseudo[0..4].copy_from_slice(&ends.src_ip);
    pseudo[4..8].copy_from_slice(&ends.dst_ip);
    pseudo[9] = IP_PROTO_TCP;
    pseudo[10..12].copy_from_slice(&(tcp_len as u16).to_be_bytes());
    pseudo[12..].copy_from_slice(&tcp);

    let csum = checksum(&pseudo);
    tcp[16..18].copy_from_slice(&csum.to_be_bytes());

    make_ip_frame(
        &ends.dst_mac,
        &ends.src_ip,
        &ends.dst_ip,
        IP_PROTO_TCP,
        &tcp,
    )
}

pub(super) fn checksum(data: &[u8]) -> u16 {
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
