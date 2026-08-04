// Enough DHCP to hand the guest its one address.

use super::SlirpBackend;
use super::frames::{
    BCAST_IP, BCAST_MAC, GUEST_IP, GW_IP, IP_PROTO_UDP, SUBNET, eth_src, make_ip_frame,
    make_udp_payload,
};

const MAGIC_COOKIE: u32 = 0x6382_5363;
const OPTION_END: u8 = 255;
const OPTION_PAD: u8 = 0;
const OPTION_MESSAGE_TYPE: u8 = 53;

const DISCOVER: u8 = 1;
const OFFER: u8 = 2;
const REQUEST: u8 = 3;
const ACK: u8 = 5;

impl SlirpBackend {
    pub(super) fn handle_dhcp(&mut self, frame: &[u8], dhcp: &[u8]) {
        if dhcp.len() < 240 || dhcp[0] != 1 {
            return;
        }

        let xid = u32::from_be_bytes(dhcp[4..8].try_into().unwrap());
        self.dhcp_xid = xid;

        if u32::from_be_bytes(dhcp[236..240].try_into().unwrap()) != MAGIC_COOKIE {
            return;
        }

        match message_type(dhcp) {
            DISCOVER => self.send_dhcp_reply(frame, xid, OFFER),
            REQUEST => self.send_dhcp_reply(frame, xid, ACK),
            _ => {}
        }
    }

    fn send_dhcp_reply(&mut self, frame: &[u8], xid: u32, msg_type: u8) {
        let src_mac: [u8; 6] = eth_src(frame).try_into().unwrap();
        let reply = build_reply(xid, &self.guest_mac, msg_type);
        let udp = make_udp_payload(68, 67, &reply);

        let mut packet = make_ip_frame(&BCAST_MAC, &GW_IP, &BCAST_IP, IP_PROTO_UDP, &udp);
        packet[0..6].copy_from_slice(&src_mac);
        self.rx_pending.push_back(packet);
    }
}

fn message_type(dhcp: &[u8]) -> u8 {
    let mut i = 240usize;

    while i < dhcp.len() {
        let option = dhcp[i];
        if option == OPTION_END {
            break;
        }

        if option == OPTION_PAD {
            i += 1;
            continue;
        }

        if i + 1 >= dhcp.len() {
            break;
        }

        let len = dhcp[i + 1] as usize;
        if option == OPTION_MESSAGE_TYPE && len >= 1 {
            return dhcp[i + 2];
        }

        i += 2 + len;
    }

    0
}

fn build_reply(xid: u32, client_mac: &[u8; 6], msg_type: u8) -> Vec<u8> {
    let mut p = vec![0u8; 300];
    p[0] = 2;
    p[1] = 1;
    p[2] = 6;
    p[4..8].copy_from_slice(&xid.to_be_bytes());
    p[16..20].copy_from_slice(&GUEST_IP);
    p[20..24].copy_from_slice(&GW_IP);
    p[28..34].copy_from_slice(client_mac);
    p[236..240].copy_from_slice(&MAGIC_COOKIE.to_be_bytes());

    let mut i = 240usize;
    let mut option = |code: u8, value: &[u8], at: &mut usize| {
        p[*at] = code;
        p[*at + 1] = value.len() as u8;
        p[*at + 2..*at + 2 + value.len()].copy_from_slice(value);
        *at += 2 + value.len();
    };

    option(OPTION_MESSAGE_TYPE, &[msg_type], &mut i);
    option(54, &GW_IP, &mut i); // server identifier
    option(51, &86400u32.to_be_bytes(), &mut i); // lease time
    option(1, &SUBNET, &mut i); // subnet mask
    option(3, &GW_IP, &mut i); // router
    option(6, &GW_IP, &mut i); // dns server

    p[i] = OPTION_END;
    p
}
