// The guest's resolver, answered from the host where possible and relayed
// upstream where not.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, ToSocketAddrs, UdpSocket};
use std::time::{Duration, Instant};

use super::SlirpBackend;
use super::frames::{GW_IP, IP_PROTO_UDP, make_ip_frame, make_udp_payload};

const RELAY_TIMEOUT: Duration = Duration::from_secs(5);
const QTYPE_A: u16 = 1;
const QTYPE_AAAA: u16 = 28;
const RCODE_NOERROR: u16 = 0;
const RCODE_SERVFAIL: u16 = 2;
const MAX_ANSWERS: usize = 4;

const UPSTREAM_SERVERS: [[u8; 4]; 3] = [
    [1, 1, 1, 1],        // Cloudflare
    [8, 8, 8, 8],        // Google
    [208, 67, 222, 222], // OpenDNS
];

pub(super) struct DnsPending {
    sock: UdpSocket,
    guest_mac: [u8; 6],
    src_ip: [u8; 4],
    src_port: u16,
    query: Vec<u8>,
    created: Instant,
}

struct Question {
    hostname: String,
    qtype: u16,
    question_end: usize,
}

impl SlirpBackend {
    pub(super) fn handle_dns(
        &mut self,
        query: &[u8],
        guest_mac: [u8; 6],
        src_ip: [u8; 4],
        src_port: u16,
    ) {
        if let Some(reply) = answer_from_host(query) {
            self.reply_to_guest(&guest_mac, &src_ip, src_port, &reply);
            return;
        }

        if self.relay_upstream(query, guest_mac, src_ip, src_port) {
            return;
        }

        if let Some(question) = parse_question(query) {
            let servfail = build_reply(query, &question, &[], RCODE_SERVFAIL);
            self.reply_to_guest(&guest_mac, &src_ip, src_port, &servfail);
        }
    }

    pub(super) fn poll_dns(&mut self) {
        let mut finished = Vec::new();

        for (index, request) in self.dns_pending.iter().enumerate() {
            let mut buf = [0u8; 2048];

            if let Ok((n, _)) = request.sock.recv_from(&mut buf) {
                let reply = make_udp_payload(53, request.src_port, &buf[..n]);
                self.rx_pending.push_back(make_ip_frame(
                    &request.guest_mac,
                    &GW_IP,
                    &request.src_ip,
                    IP_PROTO_UDP,
                    &reply,
                ));
                finished.push(index);
                continue;
            }

            if request.created.elapsed() > RELAY_TIMEOUT {
                if let Some(question) = parse_question(&request.query) {
                    let servfail = build_reply(&request.query, &question, &[], RCODE_SERVFAIL);
                    let reply = make_udp_payload(53, request.src_port, &servfail);
                    self.rx_pending.push_back(make_ip_frame(
                        &request.guest_mac,
                        &GW_IP,
                        &request.src_ip,
                        IP_PROTO_UDP,
                        &reply,
                    ));
                }
                finished.push(index);
            }
        }

        for index in finished.into_iter().rev() {
            self.dns_pending.swap_remove(index);
        }
    }

    fn reply_to_guest(
        &mut self,
        guest_mac: &[u8; 6],
        src_ip: &[u8; 4],
        src_port: u16,
        reply: &[u8],
    ) {
        let udp = make_udp_payload(53, src_port, reply);
        self.rx_pending
            .push_back(make_ip_frame(guest_mac, &GW_IP, src_ip, IP_PROTO_UDP, &udp));
    }

    fn relay_upstream(
        &mut self,
        query: &[u8],
        guest_mac: [u8; 6],
        src_ip: [u8; 4],
        src_port: u16,
    ) -> bool {
        let Ok(sock) = UdpSocket::bind("0.0.0.0:0") else {
            return false;
        };
        sock.set_nonblocking(true).ok();

        let sent = UPSTREAM_SERVERS.iter().any(|server| {
            let address = SocketAddrV4::new(Ipv4Addr::from(*server), 53);
            sock.send_to(query, address).is_ok()
        });

        if !sent {
            return false;
        }

        self.dns_pending.push(DnsPending {
            sock,
            guest_mac,
            src_ip,
            src_port,
            query: query.to_vec(),
            created: Instant::now(),
        });

        true
    }
}

fn parse_question(query: &[u8]) -> Option<Question> {
    if query.len() < 12 {
        return None;
    }

    if u16::from_be_bytes([query[4], query[5]]) != 1 {
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

    Some(Question {
        hostname,
        qtype,
        question_end,
    })
}

fn answer_from_host(query: &[u8]) -> Option<Vec<u8>> {
    let question = parse_question(query)?;

    match question.qtype {
        QTYPE_A => {
            let resolved = (question.hostname.as_str(), 0u16).to_socket_addrs().ok()?;
            let addresses: Vec<[u8; 4]> = resolved
                .filter_map(|address| match address {
                    SocketAddr::V4(v4) => Some(v4.ip().octets()),
                    SocketAddr::V6(_) => None,
                })
                .take(MAX_ANSWERS)
                .collect();

            if addresses.is_empty() {
                return None;
            }

            Some(build_reply(query, &question, &addresses, RCODE_NOERROR))
        }

        QTYPE_AAAA => Some(build_reply(query, &question, &[], RCODE_NOERROR)),
        _ => None,
    }
}

fn build_reply(query: &[u8], question: &Question, ipv4_answers: &[[u8; 4]], rcode: u16) -> Vec<u8> {
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
        reply.extend_from_slice(&QTYPE_A.to_be_bytes());
        reply.extend_from_slice(&1u16.to_be_bytes()); // class IN
        reply.extend_from_slice(&60u32.to_be_bytes()); // ttl
        reply.extend_from_slice(&4u16.to_be_bytes()); // rdata length
        reply.extend_from_slice(address);
    }

    reply
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let query = example_com_query(QTYPE_A);
        let question = parse_question(&query).unwrap();
        assert_eq!(question.hostname, "example.com");
        assert_eq!(question.qtype, QTYPE_A);
        assert_eq!(question.question_end, query.len());
    }

    #[test]
    fn rejects_truncated_and_multi_question_queries() {
        assert!(parse_question(&[0u8; 5]).is_none());

        let mut two_questions = example_com_query(QTYPE_A);
        two_questions[5] = 2;
        assert!(parse_question(&two_questions).is_none());
    }

    #[test]
    fn reply_echoes_id_and_question_with_answers() {
        let query = example_com_query(QTYPE_A);
        let question = parse_question(&query).unwrap();
        let reply = build_reply(&query, &question, &[[93, 184, 215, 14]], RCODE_NOERROR);

        assert_eq!(&reply[0..2], &[0xAB, 0xCD]); // same transaction id
        assert_eq!(u16::from_be_bytes([reply[2], reply[3]]) & 0x8000, 0x8000); // response bit
        assert_eq!(u16::from_be_bytes([reply[6], reply[7]]), 1); // one answer
        assert_eq!(&reply[12..question.question_end], &query[12..]); // question echoed
        assert_eq!(&reply[reply.len() - 4..], &[93, 184, 215, 14]); // rdata
    }

    #[test]
    fn servfail_reply_has_rcode_and_no_answers() {
        let query = example_com_query(QTYPE_A);
        let question = parse_question(&query).unwrap();
        let reply = build_reply(&query, &question, &[], RCODE_SERVFAIL);

        assert_eq!(
            u16::from_be_bytes([reply[2], reply[3]]) & 0x000F,
            RCODE_SERVFAIL
        );
        assert_eq!(u16::from_be_bytes([reply[6], reply[7]]), 0);
    }

    #[test]
    fn aaaa_query_gets_empty_noerror_without_touching_resolver() {
        let query = example_com_query(QTYPE_AAAA);
        let reply = answer_from_host(&query).unwrap();

        assert_eq!(
            u16::from_be_bytes([reply[2], reply[3]]) & 0x000F,
            RCODE_NOERROR
        );
        assert_eq!(u16::from_be_bytes([reply[6], reply[7]]), 0); // no answers
    }
}
