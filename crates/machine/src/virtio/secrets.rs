#[derive(Clone, Debug)]
pub struct SecretBinding {
    pub placeholder: String,
    pub value: String,
    pub hosts: Vec<String>,
}

const MAX_HEAD_BYTES: usize = 64 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub struct Refused {
    pub placeholder: String,
    pub host: String,
}

#[derive(Debug, PartialEq, Eq)]
enum Phase {
    Head(Vec<u8>),
    Body(u64),
    Opaque,
}

pub struct Substitution {
    allowed: Vec<SecretBinding>,
    forbidden: Vec<String>,
    host: String,
    phase: Phase,
}

impl Substitution {
    pub fn for_host(bindings: &[SecretBinding], host: &str) -> Option<Self> {
        if bindings.is_empty() {
            return None;
        }

        let (allowed, rejected): (Vec<_>, Vec<_>) = bindings
            .iter()
            .cloned()
            .partition(|binding| binding.hosts.iter().any(|name| host_matches(name, host)));

        Some(Self {
            allowed,
            forbidden: rejected
                .into_iter()
                .map(|binding| binding.placeholder)
                .collect(),
            host: host.to_string(),
            phase: Phase::Head(Vec::new()),
        })
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<u8>, Refused> {
        let mut pending = bytes.to_vec();
        let mut out = Vec::with_capacity(bytes.len());

        loop {
            match &mut self.phase {
                Phase::Opaque => {
                    self.refuse_forbidden(&pending)?;
                    out.extend_from_slice(&pending);
                    return Ok(out);
                }

                Phase::Body(remaining) => {
                    let take = (*remaining).min(pending.len() as u64) as usize;
                    out.extend_from_slice(&pending[..take]);
                    *remaining -= take as u64;
                    pending.drain(..take);

                    if *remaining > 0 {
                        return Ok(out);
                    }
                    self.phase = Phase::Head(Vec::new());
                    if pending.is_empty() {
                        return Ok(out);
                    }
                }

                Phase::Head(head) => {
                    head.extend_from_slice(&pending);
                    pending.clear();

                    let Some(end) = find_head_end(head) else {
                        if head.len() > MAX_HEAD_BYTES {
                            return Err(Refused {
                                placeholder: String::new(),
                                host: self.host.clone(),
                            });
                        }

                        return Ok(out);
                    };

                    let rest = head.split_off(end);
                    let head = std::mem::take(head);

                    self.refuse_forbidden(&head)?;
                    let (rewritten, body) = self.rewrite_head(head);
                    out.extend_from_slice(&rewritten);

                    self.phase = match body {
                        Some(length) => Phase::Body(length),
                        None => Phase::Opaque,
                    };
                    pending = rest;

                    if pending.is_empty() && self.phase != Phase::Opaque {
                        return Ok(out);
                    }
                }
            }
        }
    }

    fn refuse_forbidden(&self, bytes: &[u8]) -> Result<(), Refused> {
        for placeholder in &self.forbidden {
            if contains(bytes, placeholder.as_bytes()) {
                return Err(Refused {
                    placeholder: placeholder.clone(),
                    host: self.host.clone(),
                });
            }
        }

        Ok(())
    }

    /// Returns the head to send and how long the body is, or `None` when the
    /// body has no length the sender declared and parsing has to stop.
    fn rewrite_head(&self, head: Vec<u8>) -> (Vec<u8>, Option<u64>) {
        let Ok(text) = std::str::from_utf8(&head) else {
            return (head, None);
        };

        let mut rewritten = text.to_string();
        for binding in &self.allowed {
            if rewritten.contains(&binding.placeholder) {
                rewritten = rewritten.replace(&binding.placeholder, &binding.value);
            }
        }

        (rewritten.into_bytes(), body_length(text))
    }
}

fn host_matches(pattern: &str, host: &str) -> bool {
    match pattern.strip_prefix("*.") {
        Some(suffix) => host
            .split_once('.')
            .is_some_and(|(_, rest)| rest.eq_ignore_ascii_case(suffix)),
        None => pattern.eq_ignore_ascii_case(host),
    }
}

fn find_head_end(bytes: &[u8]) -> Option<usize> {
    bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|start| start + 4)
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack.len() >= needle.len()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

fn body_length(head: &str) -> Option<u64> {
    let mut lines = head.split("\r\n");

    let request_line = lines.next()?;
    let mut parts = request_line.split(' ');
    let method = parts.next()?;
    let _target = parts.next()?;
    let version = parts.next()?;

    if parts.next().is_some()
        || !version.starts_with("HTTP/1.")
        || method.is_empty()
        || !method.bytes().all(|byte| byte.is_ascii_uppercase())
    {
        return None;
    }

    let mut length = 0;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };

        if name.eq_ignore_ascii_case("transfer-encoding") {
            return None;
        }
        if name.eq_ignore_ascii_case("content-length") {
            length = value.trim().parse().ok()?;
        }
    }

    Some(length)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLACEHOLDER: &str = "vpod-secret-key-a1b2c3d4";
    const VALUE: &str = "sk-ant-the-real-thing";

    fn binding(hosts: &[&str]) -> SecretBinding {
        SecretBinding {
            placeholder: PLACEHOLDER.to_string(),
            value: VALUE.to_string(),
            hosts: hosts.iter().map(|host| host.to_string()).collect(),
        }
    }

    fn request(header_value: &str, body: &str) -> String {
        format!(
            "POST /v1/messages HTTP/1.1\r\nHost: api.anthropic.com\r\n\
             x-api-key: {header_value}\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
    }

    fn feed_all(substitution: &mut Substitution, chunks: &[&str]) -> Result<String, Refused> {
        let mut out = Vec::new();
        for chunk in chunks {
            out.extend(substitution.feed(chunk.as_bytes())?);
        }

        Ok(String::from_utf8(out).expect("output was not utf8"))
    }

    #[test]
    fn a_placeholder_bound_for_an_allowed_host_is_swapped() {
        let mut sub =
            Substitution::for_host(&[binding(&["api.anthropic.com"])], "api.anthropic.com")
                .expect("bindings were given");

        let out = feed_all(&mut sub, &[&request(PLACEHOLDER, "{}")]).expect("allowed");

        assert!(out.contains(VALUE), "the real value never went out");
        assert!(!out.contains(PLACEHOLDER), "the stand-in went out too");
    }

    #[test]
    fn a_placeholder_bound_for_another_host_refuses_the_connection() {
        let mut sub =
            Substitution::for_host(&[binding(&["api.anthropic.com"])], "evil.example.com")
                .expect("bindings were given");

        let refused = feed_all(&mut sub, &[&request(PLACEHOLDER, "{}")]).expect_err("refused");

        assert_eq!(refused.placeholder, PLACEHOLDER);
        assert_eq!(refused.host, "evil.example.com");
    }

    #[test]
    fn a_request_to_an_unlisted_host_without_the_placeholder_is_untouched() {
        let mut sub =
            Substitution::for_host(&[binding(&["api.anthropic.com"])], "example.com").unwrap();

        let wire = request("nothing-secret", "{}");
        let out = feed_all(&mut sub, &[&wire]).expect("no placeholder, no refusal");

        assert_eq!(out, wire);
    }

    #[test]
    fn a_host_header_naming_an_allowed_host_does_not_earn_the_swap() {
        // The connection is to evil.example.com; only the header claims otherwise.
        let mut sub =
            Substitution::for_host(&[binding(&["api.anthropic.com"])], "evil.example.com").unwrap();

        let wire = format!(
            "GET / HTTP/1.1\r\nHost: api.anthropic.com\r\nx-api-key: {PLACEHOLDER}\r\n\
             Content-Length: 0\r\n\r\n"
        );

        assert!(
            feed_all(&mut sub, &[&wire]).is_err(),
            "a header bought authority"
        );
    }

    #[test]
    fn a_placeholder_split_across_two_reads_is_still_swapped() {
        let mut sub =
            Substitution::for_host(&[binding(&["api.anthropic.com"])], "api.anthropic.com")
                .unwrap();

        let wire = request(PLACEHOLDER, "{}");
        let (first, second) = wire.split_at(wire.find(PLACEHOLDER).unwrap() + 8);

        let out = feed_all(&mut sub, &[first, second]).expect("allowed");

        assert!(out.contains(VALUE));
        assert!(!out.contains(PLACEHOLDER));
    }

    #[test]
    fn a_split_placeholder_bound_elsewhere_is_still_caught() {
        let mut sub =
            Substitution::for_host(&[binding(&["api.anthropic.com"])], "evil.example.com").unwrap();

        let wire = request(PLACEHOLDER, "{}");
        let (first, second) = wire.split_at(wire.find(PLACEHOLDER).unwrap() + 8);

        assert!(
            feed_all(&mut sub, &[first, second]).is_err(),
            "the split hid it"
        );
    }

    #[test]
    fn a_second_request_on_the_same_connection_is_swapped_too() {
        let mut sub =
            Substitution::for_host(&[binding(&["api.anthropic.com"])], "api.anthropic.com")
                .unwrap();

        let wire = format!(
            "{}{}",
            request(PLACEHOLDER, "{}"),
            request(PLACEHOLDER, "{}")
        );
        let out = feed_all(&mut sub, &[&wire]).expect("allowed");

        assert_eq!(
            out.matches(VALUE).count(),
            2,
            "keep-alive lost the second one"
        );
        assert!(!out.contains(PLACEHOLDER));
    }

    #[test]
    fn a_body_that_looks_like_a_head_is_not_parsed_as_one() {
        let mut sub =
            Substitution::for_host(&[binding(&["api.anthropic.com"])], "api.anthropic.com")
                .unwrap();

        let body = "GET /nested HTTP/1.1\r\nx-api-key: nothing\r\n\r\n";
        let out = feed_all(&mut sub, &[&request(PLACEHOLDER, body)]).expect("allowed");

        assert!(out.ends_with(body), "the body was rewritten as a head");
    }

    #[test]
    fn two_secrets_do_not_cross() {
        let anthropic = SecretBinding {
            placeholder: "vpod-secret-a".to_string(),
            value: "value-a".to_string(),
            hosts: vec!["api.anthropic.com".to_string()],
        };
        let github = SecretBinding {
            placeholder: "vpod-secret-b".to_string(),
            value: "value-b".to_string(),
            hosts: vec!["api.github.com".to_string()],
        };

        let mut sub = Substitution::for_host(&[anthropic, github], "api.anthropic.com").unwrap();

        let out = feed_all(&mut sub, &[&request("vpod-secret-a", "{}")]).expect("allowed");
        assert!(out.contains("value-a"));

        let mut sub =
            Substitution::for_host(&[binding(&["api.anthropic.com"])], "api.anthropic.com")
                .unwrap();
        let github_key = feed_all(&mut sub, &[&request("vpod-secret-b", "{}")]);
        assert!(
            github_key.is_ok(),
            "an unrelated stand-in is not ours to refuse"
        );
    }

    #[test]
    fn a_wildcard_matches_one_label_and_not_the_apex() {
        assert!(host_matches("*.example.com", "api.example.com"));
        assert!(!host_matches("*.example.com", "example.com"));
        assert!(!host_matches("*.example.com", "a.b.example.com"));
        assert!(host_matches("API.Example.com", "api.example.com"));
    }

    #[test]
    fn a_head_that_never_ends_is_refused_rather_than_buffered_forever() {
        let mut sub =
            Substitution::for_host(&[binding(&["api.anthropic.com"])], "api.anthropic.com")
                .unwrap();

        let flood = "x".repeat(MAX_HEAD_BYTES + 1);

        assert!(
            feed_all(&mut sub, &[&flood]).is_err(),
            "buffered without bound"
        );
    }

    #[test]
    fn traffic_that_is_not_http_still_refuses_a_forbidden_placeholder() {
        let mut sub =
            Substitution::for_host(&[binding(&["api.anthropic.com"])], "evil.example.com").unwrap();

        let wire = format!("\x16\x03\x01 binary junk {PLACEHOLDER} more junk\r\n\r\n");

        assert!(
            feed_all(&mut sub, &[&wire]).is_err(),
            "opaque bytes smuggled it out"
        );
    }

    #[test]
    fn nothing_bound_means_nothing_to_do() {
        assert!(Substitution::for_host(&[], "api.anthropic.com").is_none());
    }
}
