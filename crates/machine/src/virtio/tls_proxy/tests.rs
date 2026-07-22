use super::testutil::{
    client_config_trusting, generate_ca_pems, mint_leaf_with_ca, spawn_test_upstream,
    spawn_test_upstream_streaming,
};
use super::*;
use std::net::TcpListener;
use std::thread;

const UPSTREAM_REPLY: &[u8] = b"HTTP/1.0 200 OK\r\nContent-Length: 5\r\n\r\nhello";

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls_rustcrypto::provider())
}

#[test]
fn resolver_mints_loadable_leaf_for_sni() {
    let ctx = TlsContext::new().expect("terminator init");
    let _ = ServerConnection::new(ctx.server_config.clone()).unwrap();
}

#[test]
fn end_to_end_bridge_delivers_upstream_reply() {
    // its own CA + a localhost leaf the proxy
    let (up_ca_key, up_ca_cert) = generate_ca_pems();
    let (leaf_der, leaf_key) = mint_leaf_with_ca(&up_ca_key, &up_ca_cert, "localhost");

    let up_server_cfg = ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(leaf_der)],
            PrivateKeyDer::try_from(leaf_key).unwrap(),
        )
        .unwrap();
    let up_server_cfg = Arc::new(up_server_cfg);

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();

    let up = thread::spawn(move || {
        listener.set_nonblocking(false).ok();
        let (mut sock, _) = listener.accept().unwrap();
        sock.set_read_timeout(Some(Duration::from_secs(10))).ok();
        let mut conn = ServerConnection::new(up_server_cfg).unwrap();

        let mut req = Vec::new();
        loop {
            if conn.wants_write() {
                conn.write_tls(&mut sock).unwrap();
                continue;
            }

            if conn.is_handshaking() {
                conn.read_tls(&mut sock).unwrap();
                conn.process_new_packets().unwrap();
                continue;
            }

            let mut buf = [0u8; 1024];
            match conn.reader().read(&mut buf) {
                Ok(n) if n > 0 => {
                    req.extend_from_slice(&buf[..n]);
                    if req.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                    continue;
                }
                _ => {}
            }
            conn.read_tls(&mut sock).unwrap();
            conn.process_new_packets().unwrap();
        }

        conn.writer().write_all(UPSTREAM_REPLY).unwrap();
        while conn.wants_write() {
            conn.write_tls(&mut sock).unwrap();
        }

        conn.send_close_notify();
        while conn.wants_write() {
            conn.write_tls(&mut sock).unwrap();
        }
    });

    let ctx = TlsContext::new().unwrap();
    let mut up_roots = RootCertStore::empty();
    let up_ca_der = Certificate::from_pem(&up_ca_cert)
        .unwrap()
        .to_der()
        .unwrap();

    up_roots.add(CertificateDer::from(up_ca_der)).unwrap();
    let proxy_client_cfg = Arc::new(
        ClientConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(up_roots)
            .with_no_client_auth(),
    );

    let mut proxy = TlsProxy::new_test(
        ctx.server_config.clone(),
        proxy_client_cfg,
        [127, 0, 0, 1],
        port,
    );

    let mut guest_roots = RootCertStore::empty();
    let vpod_ca_der = Certificate::from_pem(CA_CERT_PEM)
        .unwrap()
        .to_der()
        .unwrap();
    guest_roots.add(CertificateDer::from(vpod_ca_der)).unwrap();
    let guest_cfg = Arc::new(
        ClientConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(guest_roots)
            .with_no_client_auth(),
    );

    let mut guest =
        ClientConnection::new(guest_cfg, ServerName::try_from("localhost").unwrap()).unwrap();

    let mut request_sent = false;
    let mut got = Vec::new();
    for _ in 0..2000 {
        let mut out = Vec::new();
        while guest.wants_write() {
            guest.write_tls(&mut out).unwrap();
        }
        if !out.is_empty() {
            proxy.push_from_guest(&out);
        }

        let mut buf = [0u8; 16384];
        while let Some(n) = proxy.pull_to_guest(&mut buf) {
            let mut slice = &buf[..n];
            while !slice.is_empty() {
                guest.read_tls(&mut slice).unwrap();
            }
            guest.process_new_packets().unwrap();
        }

        if !request_sent && !guest.is_handshaking() {
            guest.writer().write_all(b"GET / HTTP/1.0\r\n\r\n").unwrap();
            request_sent = true;
        }

        let mut buf = [0u8; 1024];
        if let Ok(n) = guest.reader().read(&mut buf)
            && n > 0
        {
            got.extend_from_slice(&buf[..n]);
        }

        if got.windows(5).any(|w| w == b"hello") {
            break;
        }

        if proxy.failed() {
            panic!("proxy failed before delivering reply");
        }
        thread::sleep(Duration::from_millis(1));
    }

    let _ = up.join();
    assert!(
        got.windows(5).any(|w| w == b"hello"),
        "did not receive upstream reply, got {got:?}"
    );
}

#[test]
fn ring_backed_client_interops_with_rustcrypto_server() {
    let (port, up_ca, up) = spawn_test_upstream(UPSTREAM_REPLY);

    let ctx = TlsContext::new().unwrap();
    let mut proxy = TlsProxy::new_test(
        ctx.server_config.clone(),
        client_config_trusting(&up_ca),
        [127, 0, 0, 1],
        port,
    );

    let mut guest_roots = RootCertStore::empty();
    let vpod_ca_der = Certificate::from_pem(CA_CERT_PEM)
        .unwrap()
        .to_der()
        .unwrap();
    guest_roots.add(CertificateDer::from(vpod_ca_der)).unwrap();
    let ring_cfg = Arc::new(
        ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(guest_roots)
            .with_no_client_auth(),
    );

    let mut guest =
        ClientConnection::new(ring_cfg, ServerName::try_from("localhost").unwrap()).unwrap();

    let mut request_sent = false;
    let mut got = Vec::new();
    for _ in 0..2000 {
        let mut out = Vec::new();
        while guest.wants_write() {
            guest.write_tls(&mut out).unwrap();
        }
        if !out.is_empty() {
            proxy.push_from_guest(&out);
        }

        let mut buf = [0u8; 16384];
        while let Some(n) = proxy.pull_to_guest(&mut buf) {
            let mut slice = &buf[..n];
            while !slice.is_empty() {
                guest.read_tls(&mut slice).unwrap();
                guest.process_new_packets().unwrap();
            }
        }

        if !request_sent && !guest.is_handshaking() {
            guest.writer().write_all(b"GET / HTTP/1.0\r\n\r\n").unwrap();
            request_sent = true;
        }

        let mut buf = [0u8; 1024];
        if let Ok(n) = guest.reader().read(&mut buf)
            && n > 0
        {
            got.extend_from_slice(&buf[..n]);
        }

        if got.windows(5).any(|w| w == b"hello") {
            break;
        }

        if proxy.failed() {
            panic!("proxy failed against a ring-backed client");
        }
        thread::sleep(Duration::from_millis(1));
    }

    let _ = up.join();
    assert!(
        got.windows(5).any(|w| w == b"hello"),
        "did not receive upstream reply via ring client, got {got:?}"
    );
}

#[test]
fn hello_retry_request_path_completes() {
    // Modern rustls clients (uv via aws-lc-rs) lead with an X25519MLKEM768
    // post-quantum keyshare. Our rustcrypto server doesn't support it, so
    // it must send a HelloRetryRequest and complete on a classic group.
    // OpenSSL clients lead with X25519 and never take this path — an HRR
    // bug in the alpha rustcrypto provider only bites rustls clients.
    let (port, up_ca, up) = spawn_test_upstream(UPSTREAM_REPLY);

    let ctx = TlsContext::new().unwrap();
    let mut proxy = TlsProxy::new_test(
        ctx.server_config.clone(),
        client_config_trusting(&up_ca),
        [127, 0, 0, 1],
        port,
    );

    let mut guest_roots = RootCertStore::empty();
    let vpod_ca_der = Certificate::from_pem(CA_CERT_PEM)
        .unwrap()
        .to_der()
        .unwrap();
    guest_roots.add(CertificateDer::from(vpod_ca_der)).unwrap();

    let mut awslc_provider = rustls::crypto::aws_lc_rs::default_provider();
    awslc_provider.kx_groups = vec![
        rustls::crypto::aws_lc_rs::kx_group::X25519MLKEM768,
        rustls::crypto::aws_lc_rs::kx_group::X25519,
        rustls::crypto::aws_lc_rs::kx_group::SECP256R1,
    ];
    let hrr_cfg = Arc::new(
        ClientConfig::builder_with_provider(Arc::new(awslc_provider))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(guest_roots)
            .with_no_client_auth(),
    );

    let mut guest =
        ClientConnection::new(hrr_cfg, ServerName::try_from("localhost").unwrap()).unwrap();

    let mut request_sent = false;
    let mut got = Vec::new();
    for _ in 0..2000 {
        let mut out = Vec::new();
        while guest.wants_write() {
            guest.write_tls(&mut out).unwrap();
        }
        if !out.is_empty() {
            proxy.push_from_guest(&out);
        }

        let mut buf = [0u8; 16384];
        while let Some(n) = proxy.pull_to_guest(&mut buf) {
            let mut slice = &buf[..n];
            while !slice.is_empty() {
                guest.read_tls(&mut slice).unwrap();
                guest.process_new_packets().unwrap();
            }
        }

        if !request_sent && !guest.is_handshaking() {
            guest.writer().write_all(b"GET / HTTP/1.0\r\n\r\n").unwrap();
            request_sent = true;
        }

        let mut buf = [0u8; 1024];
        if let Ok(n) = guest.reader().read(&mut buf)
            && n > 0
        {
            got.extend_from_slice(&buf[..n]);
        }

        if got.windows(5).any(|w| w == b"hello") {
            break;
        }

        if proxy.failed() {
            panic!("proxy failed on the HelloRetryRequest path");
        }
        thread::sleep(Duration::from_millis(1));
    }

    let _ = up.join();
    assert!(
        got.windows(5).any(|w| w == b"hello"),
        "HRR handshake did not complete, got {got:?}"
    );
}

#[test]
fn real_uv_binary_handshakes_with_proxy() {
    use std::io::ErrorKind;
    use std::process::Command;
    use std::sync::atomic::{AtomicBool, Ordering};

    if std::env::var("VPOD_UV_REPRO").is_err() {
        eprintln!("skipping real-uv repro (set VPOD_UV_REPRO=1 to run)");
        return;
    }

    let (up_ca_key, up_ca_cert) = generate_ca_pems();
    let (leaf_der, leaf_key) = mint_leaf_with_ca(&up_ca_key, &up_ca_cert, "localhost");
    let up_cfg = Arc::new(
        ServerConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(leaf_der)],
                PrivateKeyDer::try_from(leaf_key).unwrap(),
            )
            .unwrap(),
    );

    let up_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let up_port = up_listener.local_addr().unwrap().port();
    thread::spawn(move || {
        for sock in up_listener.incoming().flatten() {
            let cfg = up_cfg.clone();
            thread::spawn(move || {
                let mut sock = sock;
                sock.set_read_timeout(Some(Duration::from_secs(10))).ok();
                let mut conn = ServerConnection::new(cfg).unwrap();
                let mut req = Vec::new();
                loop {
                    if conn.wants_write() {
                        if conn.write_tls(&mut sock).is_err() {
                            return;
                        }
                        continue;
                    }
                    let mut buf = [0u8; 4096];
                    match conn.reader().read(&mut buf) {
                        Ok(n) if n > 0 => {
                            req.extend_from_slice(&buf[..n]);
                            if req.windows(4).any(|w| w == b"\r\n\r\n") {
                                break;
                            }
                            continue;
                        }
                        _ => {}
                    }
                    if conn.read_tls(&mut sock).is_err() || conn.process_new_packets().is_err() {
                        return;
                    }
                }
                conn.writer()
                    .write_all(
                        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .unwrap();
                conn.send_close_notify();
                while conn.wants_write() {
                    if conn.write_tls(&mut sock).is_err() {
                        return;
                    }
                }
            });
        }
    });

    let ctx = TlsContext::new().unwrap();
    let proxy_client_cfg = client_config_trusting(&up_ca_cert);
    let front = TcpListener::bind("127.0.0.1:0").unwrap();
    let front_port = front.local_addr().unwrap().port();
    let any_proxy_failed = Arc::new(AtomicBool::new(false));

    {
        let ctx = ctx.clone();
        let failed_flag = any_proxy_failed.clone();
        thread::spawn(move || {
            for sock in front.incoming().flatten() {
                let mut proxy = TlsProxy::new_test(
                    ctx.server_config.clone(),
                    proxy_client_cfg.clone(),
                    [127, 0, 0, 1],
                    up_port,
                );
                let failed_flag = failed_flag.clone();
                thread::spawn(move || {
                    let mut sock = sock;
                    sock.set_nonblocking(true).ok();
                    let mut buf = [0u8; 16384];
                    loop {
                        match sock.read(&mut buf) {
                            Ok(0) => return,
                            Ok(n) => proxy.push_from_guest(&buf[..n]),
                            Err(ref e) if e.kind() == ErrorKind::WouldBlock => {}
                            Err(_) => return,
                        }
                        while let Some(n) = proxy.pull_to_guest(&mut buf) {
                            if sock.write_all(&buf[..n]).is_err() {
                                return;
                            }
                        }
                        if proxy.failed() {
                            failed_flag.store(true, Ordering::SeqCst);
                            return;
                        }
                        thread::sleep(Duration::from_millis(1));
                    }
                });
            }
        });
    }

    // Point real uv at the proxy, trusting the vpod CA.
    let ca_path = std::env::temp_dir().join("vpod-uv-repro-ca.pem");
    std::fs::write(&ca_path, CA_CERT_PEM).unwrap();
    let out = Command::new("uv")
        .args([
            "pip",
            "compile",
            "-",
            "--no-cache",
            "--index-url",
            &format!("https://localhost:{front_port}/simple/"),
        ])
        .env("SSL_CERT_FILE", &ca_path)
        .env("UV_HTTP_TIMEOUT", "20")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            child.stdin.take().unwrap().write_all(b"six\n").unwrap();
            child.wait_with_output()
        })
        .expect("failed to run uv");

    let stderr = String::from_utf8_lossy(&out.stderr);
    eprintln!("uv exit: {:?}\nuv stderr:\n{stderr}", out.status);

    assert!(
        !any_proxy_failed.load(Ordering::SeqCst),
        "TlsProxy failed against real uv (see [tls-proxy] lines above)"
    );
    let lower = stderr.to_lowercase();
    assert!(
        !lower.contains("handshake") && !lower.contains("decrypt") && !lower.contains("tls"),
        "uv reported a TLS-level error:\n{stderr}"
    );
}

#[test]
fn server_prefers_chacha20_when_client_offers_it() {
    // The guest offers AES-256 first, then ChaCha20 (see captured
    // ClientHello). Our server must ignore that order and pick ChaCha20 to
    // dodge the guest's broken AES-GCM. Verify the negotiated suite.
    let ctx = TlsContext::new().unwrap();

    let mut guest_roots = RootCertStore::empty();
    let vpod_ca_der = Certificate::from_pem(CA_CERT_PEM)
        .unwrap()
        .to_der()
        .unwrap();
    guest_roots.add(CertificateDer::from(vpod_ca_der)).unwrap();
    let mut prov = rustls::crypto::ring::default_provider();
    prov.kx_groups = vec![rustls::crypto::ring::kx_group::X25519];
    // AES-256 first, exactly like the guest — server must still choose ChaCha.
    prov.cipher_suites = vec![
        rustls::crypto::ring::cipher_suite::TLS13_AES_256_GCM_SHA384,
        rustls::crypto::ring::cipher_suite::TLS13_AES_128_GCM_SHA256,
        rustls::crypto::ring::cipher_suite::TLS13_CHACHA20_POLY1305_SHA256,
    ];
    let cfg = Arc::new(
        ClientConfig::builder_with_provider(Arc::new(prov))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(guest_roots)
            .with_no_client_auth(),
    );
    let mut guest = ClientConnection::new(cfg, ServerName::try_from("localhost").unwrap()).unwrap();
    let mut server = ServerConnection::new(ctx.server_config.clone()).unwrap();

    // Drive the handshake to completion in-memory.
    for _ in 0..20 {
        let mut c2s = Vec::new();
        while guest.wants_write() {
            guest.write_tls(&mut c2s).unwrap();
        }
        let mut cur = c2s.as_slice();
        while !cur.is_empty() {
            server.read_tls(&mut cur).unwrap();
        }
        server.process_new_packets().unwrap();

        let mut s2c = Vec::new();
        while server.wants_write() {
            server.write_tls(&mut s2c).unwrap();
        }
        let mut cur = s2c.as_slice();
        while !cur.is_empty() {
            guest.read_tls(&mut cur).unwrap();
        }
        guest.process_new_packets().unwrap();

        if !guest.is_handshaking() && !server.is_handshaking() {
            break;
        }
    }

    assert_eq!(
        server.negotiated_cipher_suite().map(|s| s.suite()),
        Some(rustls::CipherSuite::TLS13_CHACHA20_POLY1305_SHA256),
        "server should prefer ChaCha20 even though the client offered AES-256 first"
    );
}

#[test]
fn mirrors_guest_handshake_x25519_aes256_no_hrr() {
    // Reproduce the guest's EXACT handshake shape from the captured bytes:
    // ring client (like uv on riscv), x25519-only keyshare so there's no
    // HelloRetryRequest (guest sent one ClientHello), AES-256 offered first
    // so the server honors client order and picks AES_256_GCM_SHA384, plus
    // a large app-data request. If this decrypts, our server handles the
    // guest's handshake correctly and the fault is in the guest's TLS
    // client, not ours.
    let (port, up_ca, up) = spawn_test_upstream(UPSTREAM_REPLY);

    let ctx = TlsContext::new().unwrap();
    let mut proxy = TlsProxy::new_test(
        ctx.server_config.clone(),
        client_config_trusting(&up_ca),
        [127, 0, 0, 1],
        port,
    );

    let mut guest_roots = RootCertStore::empty();
    let vpod_ca_der = Certificate::from_pem(CA_CERT_PEM)
        .unwrap()
        .to_der()
        .unwrap();
    guest_roots.add(CertificateDer::from(vpod_ca_der)).unwrap();

    let mut prov = rustls::crypto::ring::default_provider();
    prov.kx_groups = vec![rustls::crypto::ring::kx_group::X25519];
    prov.cipher_suites = vec![
        rustls::crypto::ring::cipher_suite::TLS13_AES_256_GCM_SHA384,
        rustls::crypto::ring::cipher_suite::TLS13_AES_128_GCM_SHA256,
        rustls::crypto::ring::cipher_suite::TLS13_CHACHA20_POLY1305_SHA256,
    ];
    let cfg = Arc::new(
        ClientConfig::builder_with_provider(Arc::new(prov))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(guest_roots)
            .with_no_client_auth(),
    );
    let mut guest = ClientConnection::new(cfg, ServerName::try_from("localhost").unwrap()).unwrap();

    let mut request = b"GET /simple/six/ HTTP/1.1\r\nHost: files.pythonhosted.org\r\n".to_vec();
    for i in 0..20 {
        request
            .extend_from_slice(format!("X-Padding-Header-{i}: {}\r\n", "a".repeat(30)).as_bytes());
    }
    request.extend_from_slice(b"\r\n");

    let mut request_sent = false;
    let mut got = Vec::new();
    for _ in 0..2000 {
        let mut out = Vec::new();
        while guest.wants_write() {
            guest.write_tls(&mut out).unwrap();
        }
        if !out.is_empty() {
            proxy.push_from_guest(&out);
        }

        let mut buf = [0u8; 16384];
        while let Some(n) = proxy.pull_to_guest(&mut buf) {
            let mut slice = &buf[..n];
            while !slice.is_empty() {
                guest.read_tls(&mut slice).unwrap();
                guest.process_new_packets().unwrap();
            }
        }

        if !request_sent && !guest.is_handshaking() {
            guest.writer().write_all(&request).unwrap();
            request_sent = true;
        }

        let mut buf = [0u8; 1024];
        if let Ok(n) = guest.reader().read(&mut buf)
            && n > 0
        {
            got.extend_from_slice(&buf[..n]);
        }

        if got.windows(5).any(|w| w == b"hello") {
            break;
        }

        assert!(
            !proxy.failed(),
            "mirrored guest handshake: app-data failed to decrypt"
        );
        thread::sleep(Duration::from_millis(1));
    }

    let _ = up.join();
    assert!(
        got.windows(5).any(|w| w == b"hello"),
        "mirrored guest request did not round-trip, got {got:?}"
    );
}

#[test]
fn second_connection_with_session_resumption_succeeds() {
    let ctx = TlsContext::new().unwrap();

    let mut guest_roots = RootCertStore::empty();
    let vpod_ca_der = Certificate::from_pem(CA_CERT_PEM)
        .unwrap()
        .to_der()
        .unwrap();
    guest_roots.add(CertificateDer::from(vpod_ca_der)).unwrap();

    let ring_cfg = Arc::new(
        ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(guest_roots)
            .with_no_client_auth(),
    );

    for attempt in 0..2 {
        let (port, up_ca, up) = spawn_test_upstream(UPSTREAM_REPLY);
        let mut proxy = TlsProxy::new_test(
            ctx.server_config.clone(),
            client_config_trusting(&up_ca),
            [127, 0, 0, 1],
            port,
        );
        let mut guest =
            ClientConnection::new(ring_cfg.clone(), ServerName::try_from("localhost").unwrap())
                .unwrap();

        let mut request_sent = false;
        let mut got = Vec::new();
        for _ in 0..2000 {
            let mut out = Vec::new();
            while guest.wants_write() {
                guest.write_tls(&mut out).unwrap();
            }
            if !out.is_empty() {
                proxy.push_from_guest(&out);
            }

            let mut buf = [0u8; 16384];
            while let Some(n) = proxy.pull_to_guest(&mut buf) {
                let mut slice = &buf[..n];
                while !slice.is_empty() {
                    guest.read_tls(&mut slice).unwrap();
                    guest.process_new_packets().unwrap();
                }
            }

            if !request_sent && !guest.is_handshaking() {
                guest.writer().write_all(b"GET / HTTP/1.0\r\n\r\n").unwrap();
                request_sent = true;
            }

            let mut buf = [0u8; 1024];
            if let Ok(n) = guest.reader().read(&mut buf)
                && n > 0
            {
                got.extend_from_slice(&buf[..n]);
            }

            if got.windows(5).any(|w| w == b"hello") {
                break;
            }

            if proxy.failed() {
                panic!("proxy failed on connection #{attempt} (resumption)");
            }
            thread::sleep(Duration::from_millis(1));
        }

        let _ = up.join();
        assert!(
            got.windows(5).any(|w| w == b"hello"),
            "connection #{attempt} did not get reply, got {got:?}"
        );
    }
}

#[test]
fn large_response_delivered_in_full_without_truncation() {
    const BODY: usize = 1_000_000;
    let (port, up_ca, up) = spawn_test_upstream_streaming(BODY);

    let ctx = TlsContext::new().unwrap();
    let mut proxy = TlsProxy::new_test(
        ctx.server_config.clone(),
        client_config_trusting(&up_ca),
        [127, 0, 0, 1],
        port,
    );

    let mut guest = ClientConnection::new(
        client_config_trusting(CA_CERT_PEM),
        ServerName::try_from("localhost").unwrap(),
    )
    .unwrap();

    guest.set_buffer_limit(None);

    let mut request_sent = false;
    let mut got = Vec::new();
    for _ in 0..40000 {
        let mut out = Vec::new();
        while guest.wants_write() {
            guest.write_tls(&mut out).unwrap();
        }
        if !out.is_empty() {
            proxy.push_from_guest(&out);
        }

        let mut buf = [0u8; 16384];
        while let Some(n) = proxy.pull_to_guest(&mut buf) {
            let mut slice = &buf[..n];
            while !slice.is_empty() {
                guest.read_tls(&mut slice).unwrap();
                // Process + drain after every read_tls so neither the record
                // deframer nor the plaintext buffer overflows on a big body.
                guest.process_new_packets().unwrap();
                let mut pt = [0u8; 16384];
                loop {
                    match guest.reader().read(&mut pt) {
                        Ok(m) if m > 0 => got.extend_from_slice(&pt[..m]),
                        _ => break,
                    }
                }
            }
        }

        if !request_sent && !guest.is_handshaking() {
            guest.writer().write_all(b"GET / HTTP/1.0\r\n\r\n").unwrap();
            request_sent = true;
        }

        let body = got
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .map(|i| got.len() - (i + 4));
        if body == Some(BODY) {
            break;
        }

        assert!(!proxy.failed(), "proxy failed on large response");
        thread::sleep(Duration::from_millis(1));
    }

    let _ = up.join();
    let body = got
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| got.len() - (i + 4));
    assert_eq!(body, Some(BODY), "large body truncated: {body:?} of {BODY}");
}
