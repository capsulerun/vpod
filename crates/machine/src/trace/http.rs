use serde_json::Value;

use super::Tracer;

const MAX_HEAD_BYTES: usize = 16 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
}

pub struct HttpRequests {
    scheme: &'static str,
    default_host: Option<String>,
    head: Vec<u8>,
    body_bytes_left: u64,
    stopped: bool,
}

impl HttpRequests {
    pub fn new(scheme: &'static str, default_host: Option<String>) -> Self {
        Self {
            scheme,
            default_host,
            head: Vec::new(),
            body_bytes_left: 0,
            stopped: false,
        }
    }

    pub fn set_default_host(&mut self, host: &str) {
        if self.default_host.is_none() {
            self.default_host = Some(host.to_string());
        }
    }

    pub fn observe(&mut self, bytes: &[u8]) -> Vec<HttpRequest> {
        let mut requests = Vec::new();
        if self.stopped {
            return requests;
        }

        let skipped = self.body_bytes_left.min(bytes.len() as u64);
        self.body_bytes_left -= skipped;
        let bytes = &bytes[skipped as usize..];

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
                    requests.push(request);
                    let skipped = length.min(self.head.len() as u64);
                    self.head.drain(..skipped as usize);
                    self.body_bytes_left = length - skipped;
                }
                Some((request, Body::Undelimited)) => {
                    requests.push(request);
                    self.stop();
                }
                None => self.stop(),
            }
        }

        requests
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
        for line in lines {
            let Some((name, value)) = line.split_once(':') else {
                continue;
            };
            let value = value.trim();
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
}

impl HttpObserver {
    pub fn new(tracer: Tracer, scheme: &'static str, address: [u8; 4], port: u16) -> Self {
        Self {
            tracer,
            requests: HttpRequests::new(scheme, None),
            address,
            port,
        }
    }

    pub fn set_default_host(&mut self, host: &str) {
        self.requests.set_default_host(host);
    }

    pub fn observe(&mut self, bytes: &[u8]) {
        for request in self.requests.observe(bytes) {
            self.tracer.record(
                "net.http",
                &[
                    ("protocol", Value::from(self.requests.scheme)),
                    ("method", request.method.into()),
                    ("url", request.url.into()),
                    ("address", super::format_address(self.address).into()),
                    ("port", self.port.into()),
                ],
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn urls(requests: Vec<HttpRequest>) -> Vec<String> {
        requests
            .into_iter()
            .map(|request| format!("{} {}", request.method, request.url))
            .collect()
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
