use anyhow::{Context, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};

pub const PUBLIC_REGISTRY: &str = "https://registry.vpod.sh/v1/snapshots.json";

pub const PRIVATE_REGISTRY: &str = "https://api.vpod.sh/v1/snapshots.json";

#[derive(Debug)]
pub struct AuthRefused {
    pub status: u16,
    pub url: String,
}

impl std::fmt::Display for AuthRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} refused the API key ({})", self.url, self.status)
    }
}

impl std::error::Error for AuthRefused {}

pub fn is_auth_refused(error: &anyhow::Error) -> bool {
    error.downcast_ref::<AuthRefused>().is_some()
}

#[derive(Debug, Deserialize, Clone)]
pub struct Snapshot {
    pub id: String,
    pub name: String,
    pub tag: String,
    pub memory_label: String,
    pub url: String,
    pub size: u64,
    pub sha256: String,

    #[serde(default)]
    pub description: String,
}

impl Snapshot {
    pub fn display_name(&self) -> String {
        self.name.to_string()
    }
}

#[derive(Debug, Deserialize)]
struct Registry {
    version: String,
    snapshots: Vec<Snapshot>,
}

pub fn resolve_registry_url(explicit: Option<&str>, api_key: Option<&str>) -> String {
    if let Some(url) = explicit.filter(|u| !u.is_empty()) {
        return url.to_string();
    }
    match api_key {
        Some(_) => PRIVATE_REGISTRY.to_string(),
        None => PUBLIC_REGISTRY.to_string(),
    }
}

pub fn check_api_key_kind(api_key: &str) -> Result<()> {
    if api_key.starts_with("vpod_pk_") {
        anyhow::bail!(
            "this is a publishable key (vpod_pk_), and the CLI is not a browser.\n\
             Publishable keys are protected by an allowlist of Origins, and nothing\n\
             outside a browser sends an Origin the server can trust, so the key buys\n\
             you nothing here. Use a secret key (vpod_sk_) instead."
        );
    }
    if !api_key.starts_with("vpod_sk_") {
        anyhow::bail!("an API key must start with vpod_sk_ (command line) or vpod_pk_ (browser)");
    }
    Ok(())
}

pub fn key_fingerprint(api_key: &str) -> String {
    let digest = Sha256::digest(api_key.as_bytes());
    hex::encode(digest)[..12].to_string()
}

pub fn origin_tag(registry_url: &str, api_key: Option<&str>) -> String {
    match api_key {
        Some(key) => format!("{registry_url}#{}", key_fingerprint(key)),
        None => registry_url.to_string(),
    }
}

fn same_origin(url: &str, other: &str) -> bool {
    match (reqwest::Url::parse(url), reqwest::Url::parse(other)) {
        (Ok(left), Ok(right)) => left.origin() == right.origin(),
        _ => false,
    }
}

pub fn authorize(
    request: reqwest::blocking::RequestBuilder,
    url: &str,
    registry_url: &str,
    api_key: Option<&str>,
) -> reqwest::blocking::RequestBuilder {
    match api_key {
        Some(key) if same_origin(url, registry_url) => request.bearer_auth(key),
        _ => request,
    }
}

pub fn fetch(registry_url: &str, api_key: Option<&str>) -> Result<(String, Vec<Snapshot>)> {
    if let Some(key) = api_key {
        check_api_key_kind(key)?;
    }

    let client = reqwest::blocking::Client::new();
    let request = authorize(
        client.get(registry_url),
        registry_url,
        registry_url,
        api_key,
    );
    let resp = request
        .send()
        .with_context(|| format!("failed to fetch registry from {registry_url}"))?;

    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(anyhow::Error::new(AuthRefused {
            status: status.as_u16(),
            url: registry_url.to_string(),
        })
        .context(
            "the key may be revoked, or it may belong to a different organisation \
             than the snapshot you asked for",
        ));
    }
    if !status.is_success() {
        anyhow::bail!("registry request failed: {status}");
    }

    let reg: Registry = resp.json().context("failed to parse registry JSON")?;
    Ok((reg.version, reg.snapshots))
}

pub fn resolve<'a>(snapshots: &'a [Snapshot], name: &str) -> Option<&'a Snapshot> {
    let (want_name, want_tag) = match name.split_once(':') {
        Some((n, t)) => (n, Some(t)),
        None => (name, None),
    };

    snapshots.iter().find(|s| {
        s.id == name
            || (s.name == want_name && want_tag.is_none_or(|t| t == "latest" || t == s.tag))
    })
}

pub fn not_found_message(name: &str, registry_url: &str, authenticated: bool) -> String {
    let credentials = if authenticated {
        "An API key WAS sent, so this catalogue is what that key can reach."
    } else {
        "No API key was sent, so only public snapshots were searched."
    };
    format!("unknown snapshot '{name}' in {registry_url}. {credentials} Run `vpod list`.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_fingerprint_matches_both_sdks() {
        assert_eq!(key_fingerprint("vpod_sk_example"), "b5e68514b5f1");
    }

    #[test]
    fn registry_precedence_is_one_chain() {
        assert_eq!(resolve_registry_url(None, None), PUBLIC_REGISTRY);
        assert_eq!(
            resolve_registry_url(None, Some("vpod_sk_k")),
            PRIVATE_REGISTRY
        );
        // Explicit beats a key, so a self-hosted registry stays reachable with one.
        assert_eq!(
            resolve_registry_url(Some("https://e.co/c.json"), Some("vpod_sk_k")),
            "https://e.co/c.json"
        );
        // An empty --registry is not a choice.
        assert_eq!(resolve_registry_url(Some(""), None), PUBLIC_REGISTRY);
    }

    #[test]
    fn publishable_key_is_refused_outside_a_browser() {
        let refused = check_api_key_kind("vpod_pk_abc").unwrap_err().to_string();
        assert!(refused.contains("not a browser"), "{refused}");
    }

    #[test]
    fn unrecognised_prefix_is_refused_rather_than_guessed() {
        assert!(check_api_key_kind("sk-openai-style").is_err());
    }

    #[test]
    fn secret_key_is_accepted() {
        assert!(check_api_key_kind("vpod_sk_abc").is_ok());
    }

    fn header_for(url: &str, registry: &str, key: Option<&str>) -> Option<String> {
        let request = authorize(
            reqwest::blocking::Client::new().get(url),
            url,
            registry,
            key,
        )
        .build()
        .expect("request should build");

        request
            .headers()
            .get(reqwest::header::AUTHORIZATION)
            .map(|value| value.to_str().unwrap().to_string())
    }

    #[test]
    fn the_key_never_leaves_the_registry_origin() {
        let registry = "https://api.vpod.sh/v1/snapshots.json";

        assert_eq!(
            header_for("https://api.vpod.sh/v1/blob/x", registry, Some("vpod_sk_s")),
            Some("Bearer vpod_sk_s".to_string())
        );

        for hostile in [
            "https://attacker.com/blob",
            "http://api.vpod.sh/v1/blob/x",
            "https://api.vpod.sh.attacker.com/x",
            "https://api.vpod.sh:8443/v1/blob/x",
        ] {
            assert_eq!(
                header_for(hostile, registry, Some("vpod_sk_s")),
                None,
                "leaked the key to {hostile}"
            );
        }
    }

    #[test]
    fn public_entries_carry_no_header() {
        assert_eq!(
            header_for("https://registry.vpod.sh/v1/x.snap", PUBLIC_REGISTRY, None),
            None
        );
    }

    #[test]
    fn origin_tag_is_scoped_by_key_but_bare_without_one() {
        assert_eq!(origin_tag(PRIVATE_REGISTRY, None), PRIVATE_REGISTRY);
        assert_ne!(
            origin_tag(PRIVATE_REGISTRY, Some("vpod_sk_a")),
            origin_tag(PRIVATE_REGISTRY, Some("vpod_sk_b"))
        );

        assert!(!origin_tag(PRIVATE_REGISTRY, Some("vpod_sk_a")).contains("vpod_sk_a"));
    }

    #[test]
    fn not_found_says_whether_a_key_was_sent() {
        assert!(not_found_message("x", "https://r", false).contains("No API key was sent"));
        assert!(not_found_message("x", "https://r", true).contains("An API key WAS sent"));
    }
}
