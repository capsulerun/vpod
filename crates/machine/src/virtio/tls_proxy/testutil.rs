use super::*;

pub(crate) fn mint_leaf_with_ca(
    ca_key_pem: &str,
    ca_cert_pem: &str,
    sni: &str,
) -> (Vec<u8>, Vec<u8>) {
    let provider = Arc::new(rustls_rustcrypto::provider());
    let resolver = SniResolver::from_pems(provider, ca_key_pem, ca_cert_pem).unwrap();
    resolver.mint_leaf(sni).unwrap()
}

pub(crate) fn generate_ca_pems() -> (String, String) {
    use der::pem::LineEnding;
    use x509_cert::der::EncodePem;

    let key = SigningKey::random(&mut OsRng);
    let spki = SubjectPublicKeyInfoOwned::from_key(*key.verifying_key()).unwrap();
    let name = Name::from_str("CN=vpod local CA").unwrap();
    let validity = Validity::from_now(Duration::from_secs(10 * 365 * 24 * 3600)).unwrap();
    let builder = CertificateBuilder::new(
        Profile::Root,
        SerialNumber::from(1u32),
        validity,
        name,
        spki,
        &key,
    )
    .unwrap();
    let cert: Certificate = builder.build::<DerSignature>().unwrap();
    (
        key.to_pkcs8_pem(Default::default()).unwrap().to_string(),
        cert.to_pem(LineEnding::LF).unwrap(),
    )
}

pub(crate) fn spawn_test_upstream(
    reply: &'static [u8],
) -> (u16, String, std::thread::JoinHandle<()>) {
    use std::net::TcpListener;

    let (up_ca_key, up_ca_cert) = generate_ca_pems();
    let (leaf_der, leaf_key) = mint_leaf_with_ca(&up_ca_key, &up_ca_cert, "localhost");

    let up_server_cfg =
        ServerConfig::builder_with_provider(Arc::new(rustls_rustcrypto::provider()))
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

    let handle = std::thread::spawn(move || {
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
        conn.writer().write_all(reply).unwrap();
        while conn.wants_write() {
            conn.write_tls(&mut sock).unwrap();
        }
        conn.send_close_notify();
        while conn.wants_write() {
            conn.write_tls(&mut sock).unwrap();
        }
    });

    (port, up_ca_cert, handle)
}

pub(crate) fn spawn_test_upstream_streaming(
    body_len: usize,
) -> (u16, String, std::thread::JoinHandle<()>) {
    use std::net::TcpListener;

    let (up_ca_key, up_ca_cert) = generate_ca_pems();
    let (leaf_der, leaf_key) = mint_leaf_with_ca(&up_ca_key, &up_ca_cert, "localhost");
    let up_server_cfg =
        ServerConfig::builder_with_provider(Arc::new(rustls_rustcrypto::provider()))
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

    let handle = std::thread::spawn(move || {
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

        let header = format!("HTTP/1.0 200 OK\r\nContent-Length: {body_len}\r\n\r\n");
        conn.writer().write_all(header.as_bytes()).unwrap();
        let mut sent = 0usize;
        let chunk = vec![b'x'; 16384];
        while sent < body_len {
            let n = chunk.len().min(body_len - sent);
            conn.writer().write_all(&chunk[..n]).unwrap();
            sent += n;

            while conn.wants_write() {
                conn.write_tls(&mut sock).unwrap();
            }
        }

        conn.send_close_notify();
        while conn.wants_write() {
            conn.write_tls(&mut sock).unwrap();
        }
    });

    (port, up_ca_cert, handle)
}

pub(crate) fn spawn_test_upstream_rst(
    reply: &'static [u8],
) -> (u16, String, std::thread::JoinHandle<()>) {
    use std::net::TcpListener;

    let (up_ca_key, up_ca_cert) = generate_ca_pems();
    let (leaf_der, leaf_key) = mint_leaf_with_ca(&up_ca_key, &up_ca_cert, "localhost");
    let up_server_cfg =
        ServerConfig::builder_with_provider(Arc::new(rustls_rustcrypto::provider()))
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

    let handle = std::thread::spawn(move || {
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

        conn.writer().write_all(reply).unwrap();
        while conn.wants_write() {
            conn.write_tls(&mut sock).unwrap();
        }

        std::thread::sleep(Duration::from_millis(500));

        {
            use std::os::fd::AsRawFd;
            let linger = libc::linger {
                l_onoff: 1,
                l_linger: 0,
            };
            unsafe {
                libc::setsockopt(
                    sock.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_LINGER,
                    &linger as *const libc::linger as *const libc::c_void,
                    std::mem::size_of::<libc::linger>() as libc::socklen_t,
                );
            }
        }
        drop(sock);
    });

    (port, up_ca_cert, handle)
}

pub(crate) fn client_config_trusting(ca_pem: &str) -> Arc<ClientConfig> {
    let mut roots = RootCertStore::empty();
    let ca_der = Certificate::from_pem(ca_pem).unwrap().to_der().unwrap();

    roots.add(CertificateDer::from(ca_der)).unwrap();
    Arc::new(
        ClientConfig::builder_with_provider(Arc::new(rustls_rustcrypto::provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    )
}
