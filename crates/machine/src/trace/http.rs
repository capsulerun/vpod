use serde_json::Value;

use super::Tracer;

const MAX_HEAD_BYTES: usize = 16 * 1024;

pub const MAX_BODY_BYTES: usize = 64 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body_bytes: Option<u64>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct HttpBody {
    pub content: Vec<u8>,
    pub truncated: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum HttpEvent {
    Head(HttpRequest),
    Body(HttpBody),
}

pub struct HttpRequests {
    scheme: &'static str,
    default_host: Option<String>,
    head: Vec<u8>,
    body_bytes_left: u64,
    stopped: bool,
    body: Vec<u8>,
    body_truncated: bool,
}

impl HttpRequests {
    pub fn new(scheme: &'static str, default_host: Option<String>) -> Self {
        Self {
            scheme,
            default_host,
            head: Vec::new(),
            body_bytes_left: 0,
            stopped: false,
            body: Vec::new(),
            body_truncated: false,
        }
    }

    pub fn set_default_host(&mut self, host: &str) {
        if self.default_host.is_none() {
            self.default_host = Some(host.to_string());
        }
    }

    pub fn observe(&mut self, bytes: &[u8]) -> Vec<HttpEvent> {
        let mut events = Vec::new();
        if self.stopped {
            return events;
        }

        let taken = self.body_bytes_left.min(bytes.len() as u64);
        self.keep_body(&bytes[..taken as usize]);
        self.body_bytes_left -= taken;
        if taken > 0 && self.body_bytes_left == 0 {
            events.extend(self.finish_body());
        }
        let bytes = &bytes[taken as usize..];

        let mut searched_to = self.head.len().saturating_sub(3);
        self.head.extend_from_slice(bytes);

        while !self.stopped {
            let Some(end) = find_head_end(&self.head, searched_to) else {
                if self.head.len() > MAX_HEAD_BYTES {
                    self.stop();
                }
                break;
            };

            let parsed = self.parse_head(&self.head[..end]);
            self.head.drain(..end);
            searched_to = 0;

            match parsed {
                Some((request, Body::Length(length))) => {
                    events.push(HttpEvent::Head(request));

                    let taken = length.min(self.head.len() as u64);
                    let body: Vec<u8> = self.head.drain(..taken as usize).collect();
                    self.keep_body(&body);
                    self.body_bytes_left = length - taken;

                    if self.body_bytes_left == 0 {
                        events.extend(self.finish_body());
                    }
                }
                Some((request, Body::Undelimited)) => {
                    // No declared length, so there is no way to know where this
                    // body ends or the next head begins. The head says so with a
                    // null length and parsing stops here.
                    events.push(HttpEvent::Head(request));
                    self.stop();
                }
                None => self.stop(),
            }
        }

        events
    }

    fn keep_body(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }

        let room = MAX_BODY_BYTES.saturating_sub(self.body.len());
        if bytes.len() > room {
            self.body_truncated = true;
        }
        self.body.extend_from_slice(&bytes[..room.min(bytes.len())]);
    }

    fn finish_body(&mut self) -> Option<HttpEvent> {
        if self.body.is_empty() && !self.body_truncated {
            return None;
        }

        let body = HttpBody {
            content: std::mem::take(&mut self.body),
            truncated: std::mem::take(&mut self.body_truncated),
        };

        Some(HttpEvent::Body(body))
    }

    fn stop(&mut self) {
        self.stopped = true;
        self.head = Vec::new();
    }

    fn parse_head(&self, head: &[u8]) -> Option<(HttpRequest, Body)> {
        let text = std::str::from_utf8(head).ok()?;
        let mut lines = text.split("\r\n");

        let mut request_line = lines.next()?.split(' ');
        let method = request_line.next()?;
        let target = request_line.next()?;
        let version = request_line.next()?;
        if request_line.next().is_some()
            || !version.starts_with("HTTP/1.")
            || method.is_empty()
            || !method.bytes().all(|byte| byte.is_ascii_uppercase())
        {
            return None;
        }

        let mut host = None;
        let mut body = Body::Length(0);
        let mut headers: Vec<(String, String)> = Vec::new();

        for line in lines {
            let Some((name, value)) = line.split_once(':') else {
                continue;
            };
            let value = value.trim();

            match headers
                .iter_mut()
                .find(|(seen, _)| seen.eq_ignore_ascii_case(name))
            {
                Some((_, existing)) => {
                    existing.push_str(", ");
                    existing.push_str(value);
                }
                None => headers.push((name.to_string(), value.to_string())),
            }

            if name.eq_ignore_ascii_case("host") {
                host = Some(value.to_string());
            } else if name.eq_ignore_ascii_case("content-length") {
                body = Body::Length(value.parse().ok()?);
            } else if name.eq_ignore_ascii_case("transfer-encoding")
                || (name.eq_ignore_ascii_case("upgrade") && !value.is_empty())
            {
                body = Body::Undelimited;
            }
        }
        if method == "CONNECT" {
            body = Body::Undelimited;
        }

        let url = if target.starts_with("http://") || target.starts_with("https://") {
            target.to_string()
        } else {
            let host = host.or_else(|| self.default_host.clone())?;
            format!("{}://{host}{target}", self.scheme)
        };

        Some((
            HttpRequest {
                method: method.to_string(),
                url,
                headers,
                body_bytes: match body {
                    Body::Length(length) => Some(length),
                    Body::Undelimited => None,
                },
            },
            body,
        ))
    }
}

enum Body {
    Length(u64),
    Undelimited,
}

fn find_head_end(buffer: &[u8], from: usize) -> Option<usize> {
    buffer[from..]
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|position| from + position + 4)
}
pub struct HttpObserver {
    tracer: Tracer,
    requests: HttpRequests,
    address: [u8; 4],
    port: u16,
    head_seq: Option<u64>,
    head_url: Option<String>,
}

impl HttpObserver {
    pub fn new(tracer: Tracer, scheme: &'static str, address: [u8; 4], port: u16) -> Self {
        Self {
            tracer,
            requests: HttpRequests::new(scheme, None),
            address,
            port,
            head_seq: None,
            head_url: None,
        }
    }

    pub fn set_default_host(&mut self, host: &str) {
        self.requests.set_default_host(host);
    }

    pub fn observe(&mut self, bytes: &[u8]) {
        for event in self.requests.observe(bytes) {
            match event {
                HttpEvent::Head(request) => {
                    let url = request.url.clone();
                    let mut fields = vec![
                        ("protocol", Value::from(self.requests.scheme)),
                        ("method", request.method.into()),
                        ("url", request.url.into()),
                        ("address", super::format_address(self.address).into()),
                        ("port", self.port.into()),
                    ];

                    let headers: serde_json::Map<String, Value> = request
                        .headers
                        .into_iter()
                        .map(|(name, value)| (name, Value::from(value)))
                        .collect();

                    fields.push(("headers", Value::Object(headers)));
                    fields.push((
                        "body_bytes",
                        match request.body_bytes {
                            Some(length) => Value::from(length),
                            None => Value::Null,
                        },
                    ));

                    self.head_seq = self.tracer.record_seq("net.http", &fields);
                    self.head_url = Some(url);
                }

                HttpEvent::Body(body) => {
                    let (Some(head_seq), Some(url)) = (self.head_seq, self.head_url.clone()) else {
                        continue;
                    };

                    let (encoding, content) = match String::from_utf8(body.content) {
                        Ok(text) => ("utf8", text),
                        Err(raw) => (
                            "base64",
                            base64::Engine::encode(
                                &base64::engine::general_purpose::STANDARD,
                                raw.as_bytes(),
                            ),
                        ),
                    };

                    self.tracer.record(
                        "net.http.body",
                        &[
                            ("url", url.into()),
                            ("request_seq", head_seq.into()),
                            ("encoding", encoding.into()),
                            ("truncated", body.truncated.into()),
                            ("content", content.into()),
                        ],
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn urls(events: Vec<HttpEvent>) -> Vec<String> {
        heads(events)
            .into_iter()
            .map(|request| format!("{} {}", request.method, request.url))
            .collect()
    }

    fn heads(events: Vec<HttpEvent>) -> Vec<HttpRequest> {
        events
            .into_iter()
            .filter_map(|event| match event {
                HttpEvent::Head(request) => Some(request),
                HttpEvent::Body(_) => None,
            })
            .collect()
    }

    fn bodies(events: Vec<HttpEvent>) -> Vec<HttpBody> {
        events
            .into_iter()
            .filter_map(|event| match event {
                HttpEvent::Body(body) => Some(body),
                HttpEvent::Head(_) => None,
            })
            .collect()
    }

    fn capturing() -> HttpRequests {
        HttpRequests::new("https", Some("api.example.com".to_string()))
    }

    fn post(body: &str) -> Vec<u8> {
        format!(
            "POST /v1/messages HTTP/1.1\r\nHost: api.example.com\r\n\
             x-api-key: vpod-secret-key-a1b2c3d4\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .into_bytes()
    }

    #[test]
    fn the_headers_a_request_carried_are_kept_in_order() {
        let mut requests = capturing();
        let head = heads(requests.observe(&post("{}"))).remove(0);

        assert_eq!(
            head.headers,
            vec![
                ("Host".to_string(), "api.example.com".to_string()),
                (
                    "x-api-key".to_string(),
                    "vpod-secret-key-a1b2c3d4".to_string()
                ),
                ("Content-Type".to_string(), "application/json".to_string()),
                ("Content-Length".to_string(), "2".to_string()),
            ]
        );
    }

    #[test]
    fn a_header_sent_twice_is_joined_the_way_http_says_it_means() {
        let mut requests = capturing();
        let wire =
            b"GET /x HTTP/1.1\r\nHost: h\r\nAccept: a\r\nAccept: b\r\nContent-Length: 0\r\n\r\n";

        let head = heads(requests.observe(wire)).remove(0);
        let accept = head
            .headers
            .iter()
            .find(|(name, _)| name == "Accept")
            .expect("the header was dropped");

        assert_eq!(accept.1, "a, b");
    }

    #[test]
    fn a_body_under_the_cap_arrives_whole() {
        let mut requests = capturing();
        let events = requests.observe(&post(r#"{"model":"claude"}"#));
        let body = bodies(events).remove(0);

        assert_eq!(body.content, br#"{"model":"claude"}"#);
        assert!(!body.truncated);
    }

    #[test]
    fn a_body_over_the_cap_is_cut_and_says_so() {
        let mut requests = capturing();
        let long = "x".repeat(MAX_BODY_BYTES + 100);
        let body = bodies(requests.observe(&post(&long))).remove(0);

        assert_eq!(body.content.len(), MAX_BODY_BYTES);
        assert!(body.truncated, "an oversized body did not say it was cut");
    }

    #[test]
    fn a_body_split_across_writes_is_reassembled() {
        let mut requests = capturing();
        let wire = post(r#"{"a":1,"b":2}"#);

        let mut events = Vec::new();
        for byte in &wire {
            events.extend(requests.observe(std::slice::from_ref(byte)));
        }

        let body = bodies(events).remove(0);
        assert_eq!(body.content, br#"{"a":1,"b":2}"#);
    }

    #[test]
    fn the_head_is_emitted_before_its_body() {
        let mut requests = capturing();
        let events = requests.observe(&post("{}"));

        assert!(matches!(events[0], HttpEvent::Head(_)));
        assert!(matches!(events[1], HttpEvent::Body(_)));
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn a_request_with_no_body_produces_no_body_event() {
        let mut requests = capturing();
        let events = requests.observe(b"GET /x HTTP/1.1\r\nHost: h\r\nContent-Length: 0\r\n\r\n");

        assert_eq!(bodies(events).len(), 0);
    }

    #[test]
    fn a_second_request_on_one_connection_gets_its_own_pair() {
        let mut requests = capturing();
        let mut wire = post(r#"{"n":1}"#);
        wire.extend(post(r#"{"n":2}"#));

        let events = requests.observe(&wire);
        let captured = bodies(events);

        assert_eq!(captured.len(), 2, "keep-alive lost the second body");
        assert_eq!(captured[0].content, br#"{"n":1}"#);
        assert_eq!(captured[1].content, br#"{"n":2}"#);
    }

    #[test]
    fn a_body_with_no_declared_length_is_not_captured() {
        let mut requests = capturing();
        let wire = b"POST /x HTTP/1.1\r\nHost: h\r\nTransfer-Encoding: chunked\r\n\r\n7\r\nsecret\n\r\n0\r\n\r\n";

        let events = requests.observe(wire);
        let heads_seen = heads(events);

        assert_eq!(heads_seen.len(), 1);
        assert_eq!(
            heads_seen[0].body_bytes, None,
            "an undelimited body claimed a length"
        );
    }

    #[test]
    fn a_request_split_across_writes_is_seen_once() {
        let mut requests = HttpRequests::new("https", None);
        let wire = b"GET /simple/requests/ HTTP/1.1\r\nHost: pypi.org\r\nAccept: */*\r\n\r\n";

        let mut seen = Vec::new();
        for byte in wire {
            seen.extend(requests.observe(std::slice::from_ref(byte)));
        }

        assert_eq!(urls(seen), ["GET https://pypi.org/simple/requests/"]);
    }

    #[test]
    fn keep_alive_requests_are_followed_past_their_bodies() {
        let mut requests = HttpRequests::new("http", None);
        let wire = b"POST /upload HTTP/1.1\r\nHost: example.com\r\nContent-Length: 11\r\n\r\n\
                     hello worldGET /second HTTP/1.1\r\nHost: example.com\r\n\r\n";

        assert_eq!(
            urls(requests.observe(wire)),
            [
                "POST http://example.com/upload",
                "GET http://example.com/second"
            ]
        );
    }

    #[test]
    fn a_chunked_body_ends_the_following() {
        let mut requests = HttpRequests::new("http", None);
        let wire = b"POST /a HTTP/1.1\r\nHost: h\r\nTransfer-Encoding: chunked\r\n\r\n\
                     b\r\nGET /b HTTP/1.1\r\n\r\n0\r\n\r\n";

        assert_eq!(urls(requests.observe(wire)), ["POST http://h/a"]);
        assert!(
            requests
                .observe(b"GET /c HTTP/1.1\r\nHost: h\r\n\r\n")
                .is_empty()
        );
    }

    #[test]
    fn the_default_host_fills_in_a_missing_host_header() {
        let mut requests = HttpRequests::new("https", None);
        requests.set_default_host("files.pythonhosted.org");

        assert_eq!(
            urls(requests.observe(b"GET /x.whl HTTP/1.0\r\n\r\n")),
            ["GET https://files.pythonhosted.org/x.whl"]
        );
    }

    #[test]
    fn a_proxy_form_target_is_kept_as_given() {
        let mut requests = HttpRequests::new("http", None);
        assert_eq!(
            urls(requests.observe(b"GET http://example.com/a HTTP/1.1\r\n\r\n")),
            ["GET http://example.com/a"]
        );
    }

    #[test]
    fn non_http_bytes_stop_the_following() {
        let mut requests = HttpRequests::new("http", None);
        assert!(requests.observe(b"SSH-2.0-OpenSSH_9.6\r\n\r\n").is_empty());
        assert!(
            requests
                .observe(b"GET / HTTP/1.1\r\nHost: h\r\n\r\n")
                .is_empty()
        );
    }
}
