/*
 * vpod libssl shim
 */

#define _GNU_SOURCE
#include <arpa/inet.h>
#include <dlfcn.h>
#include <errno.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

typedef struct ssl_st SSL;
typedef struct x509_st X509;
typedef struct bio_st BIO;

#define SSL_CTRL_SET_TLSEXT_HOSTNAME 55


struct entry {
    SSL *ssl;
    int fd;
    char host[256];
    int bridged;
    int last_errno;
};



#define VPOD_SSL_ERROR_WANT_READ 2
#define VPOD_SSL_ERROR_NONE 0
#define VPOD_SSL_ERROR_ZERO_RETURN 6

#define VPOD_SSL_ERROR_WANT_WRITE 3
#define VPOD_SSL_ERROR_SYSCALL 5


#define MAX_SSL 256
static struct entry g_table[MAX_SSL];

static int g_enabled = -1;

static void log_line(const char *msg) {

    if (getenv("VPOD_SHIM_TRACE"))
        fprintf(stderr, "vpod-shim: %s\n", msg);
}


static int shim_enabled(void) {
    if (g_enabled < 0)
        g_enabled = getenv("VPOD_REAL_TLS") ? 0 : 1;

    return g_enabled;
}

static struct entry *find(SSL *ssl) {
    for (int i = 0; i < MAX_SSL; i++)
        if (g_table[i].ssl == ssl)
            return &g_table[i];

    return NULL;
}

static struct entry *intern(SSL *ssl) {
    struct entry *e = find(ssl);

    if (e)
        return e;

    for (int i = 0; i < MAX_SSL; i++) {
        if (g_table[i].ssl == NULL) {
            g_table[i].ssl = ssl;
            g_table[i].fd = -1;
            g_table[i].host[0] = '\0';
            g_table[i].bridged = 0;
            g_table[i].last_errno = 0;
            return &g_table[i];
        }
    }

    return NULL;
}

static void forget(SSL *ssl) {
    struct entry *e = find(ssl);
    if (e)
        e->ssl = NULL;
}

static void *libssl_handle(void) {
    static void *handler;

    if (!handler)
        handler = dlopen("libssl.so.3", RTLD_NOW | RTLD_GLOBAL | RTLD_NOLOAD);

    if (!handler)
        handler = dlopen("libssl.so.3", RTLD_NOW | RTLD_GLOBAL);

    return handler;
}

static void *libcrypto_handle(void) {
    static void *handler;

    if (!handler)
        handler = dlopen("libcrypto.so.3", RTLD_NOW | RTLD_GLOBAL | RTLD_NOLOAD);

    if (!handler)
        handler = dlopen("libcrypto.so.3", RTLD_NOW | RTLD_GLOBAL);

    return handler;
}

#define REAL(name) ((real_##name) ? real_##name : (real_##name = dlsym(libssl_handle(), #name)))
#define REALC(name) ((real_##name) ? real_##name : (real_##name = dlsym(libcrypto_handle(), #name)))

typedef int (*real_SSL_set_fd_t)(SSL *, int);
typedef long (*real_SSL_ctrl_t)(SSL *, int, long, void *);
typedef int (*real_SSL_connect_t)(SSL *);
typedef int (*real_SSL_do_handshake_t)(SSL *);
typedef int (*real_SSL_read_t)(SSL *, void *, int);
typedef int (*real_SSL_write_t)(SSL *, const void *, int);
typedef int (*real_SSL_read_ex_t)(SSL *, void *, size_t, size_t *);
typedef int (*real_SSL_write_ex_t)(SSL *, const void *, size_t, size_t *);
typedef int (*real_SSL_shutdown_t)(SSL *);
typedef void (*real_SSL_free_t)(SSL *);
typedef int (*real_SSL_get_error_t)(const SSL *, int);
typedef long (*real_SSL_get_verify_result_t)(const SSL *);
typedef int (*real_SSL_get_fd_t)(const SSL *);

static real_SSL_set_fd_t real_SSL_set_fd;
static real_SSL_ctrl_t real_SSL_ctrl;
static real_SSL_connect_t real_SSL_connect;
static real_SSL_do_handshake_t real_SSL_do_handshake;
static real_SSL_read_t real_SSL_read;
static real_SSL_write_t real_SSL_write;
static real_SSL_read_ex_t real_SSL_read_ex;
static real_SSL_write_ex_t real_SSL_write_ex;
static real_SSL_shutdown_t real_SSL_shutdown;
static real_SSL_free_t real_SSL_free;
static real_SSL_get_error_t real_SSL_get_error;
static real_SSL_get_verify_result_t real_SSL_get_verify_result;
static real_SSL_get_fd_t real_SSL_get_fd;

typedef X509 *(*real_SSL_get1_peer_certificate_t)(const SSL *);
typedef X509 *(*real_SSL_get_peer_certificate_t)(const SSL *);
typedef int (*real_X509_check_host_t)(X509 *, const char *, size_t, unsigned int, char **);
typedef int (*real_X509_up_ref_t)(X509 *);
typedef BIO *(*real_BIO_new_file_t)(const char *, const char *);
typedef int (*real_BIO_free_t)(BIO *);
typedef X509 *(*real_PEM_read_bio_X509_t)(BIO *, X509 **, void *, void *);

static real_SSL_get1_peer_certificate_t real_SSL_get1_peer_certificate;
static real_SSL_get_peer_certificate_t real_SSL_get_peer_certificate;
static real_X509_check_host_t real_X509_check_host;
static real_X509_up_ref_t real_X509_up_ref;
static real_BIO_new_file_t real_BIO_new_file;
static real_BIO_free_t real_BIO_free;
static real_PEM_read_bio_X509_t real_PEM_read_bio_X509;

static int write_all(int fd, const char *buf, size_t len) {

    while (len > 0) {
        ssize_t n = write(fd, buf, len);

        if (n < 0) {
            if (errno == EINTR)
                continue;
            return -1;
        }

        buf += n;
        len -= (size_t)n;
    }
    return 0;
}

static int begin_bridge(struct entry *e) {
    if (e->bridged)
        return 1;

    if (e->fd < 0 || e->host[0] == '\0')
        return 0;

    struct sockaddr_in peer;
    socklen_t plen = sizeof(peer);
    unsigned port = 443;

    if (getpeername(e->fd, (struct sockaddr *)&peer, &plen) == 0 &&
        peer.sin_family == AF_INET)
        port = ntohs(peer.sin_port);

    if (port != 443)
        return 0;

    char pre[300];
    int n = snprintf(pre, sizeof(pre), "VPOD-CONNECT %s %u\n", e->host, port);
    if (n <= 0 || (size_t)n >= sizeof(pre) || write_all(e->fd, pre, (size_t)n) != 0)
        return 0;

    e->bridged = 1;
    log_line("bridge established");
    return 1;
}

int SSL_set_fd(SSL *ssl, int fd) {
    if (shim_enabled()) {
        struct entry *e = intern(ssl);
        if (e)
            e->fd = fd;
    }

    return REAL(SSL_set_fd)(ssl, fd);
}

long SSL_ctrl(SSL *ssl, int cmd, long larg, void *parg) {
    if (shim_enabled() && cmd == SSL_CTRL_SET_TLSEXT_HOSTNAME && parg) {
        struct entry *e = intern(ssl);

        if (e) {
            strncpy(e->host, (const char *)parg, sizeof(e->host) - 1);
            e->host[sizeof(e->host) - 1] = '\0';
        }
    }

    return REAL(SSL_ctrl)(ssl, cmd, larg, parg);
}

static int do_connect(SSL *ssl) {
    struct entry *e = shim_enabled() ? find(ssl) : NULL;
    if (e) {
        if (e->fd < 0)
            e->fd = REAL(SSL_get_fd)(ssl);

        if (begin_bridge(e))
            return 1;

        log_line("fell through at connect (no fd/host or non-443)");
    }

    return -2;
}

int SSL_connect(SSL *ssl) {
    int r = do_connect(ssl);
    return r == -2 ? REAL(SSL_connect)(ssl) : r;
}

int SSL_do_handshake(SSL *ssl) {
    int r = do_connect(ssl);
    return r == -2 ? REAL(SSL_do_handshake)(ssl) : r;
}

int SSL_read(SSL *ssl, void *buf, int num) {
    struct entry *e = shim_enabled() ? find(ssl) : NULL;
    if (e && e->bridged) {
        ssize_t n;

        do {
            n = read(e->fd, buf, (size_t)num);
        } while (n < 0 && errno == EINTR);

        e->last_errno = n < 0 ? errno : 0;
        return (int)n;
    }

    return REAL(SSL_read)(ssl, buf, num);
}

int SSL_read_ex(SSL *ssl, void *buf, size_t num, size_t *readbytes) {
    struct entry *e = shim_enabled() ? find(ssl) : NULL;

    if (e && e->bridged) {
        ssize_t n;
        do {
            n = read(e->fd, buf, num);
        } while (n < 0 && errno == EINTR);

        e->last_errno = n < 0 ? errno : 0;
        if (n > 0) {
            *readbytes = (size_t)n;
            return 1;
        }

        return 0;
    }

    return REAL(SSL_read_ex)(ssl, buf, num, readbytes);
}

int SSL_write_ex(SSL *ssl, const void *buf, size_t num, size_t *written) {
    struct entry *e = shim_enabled() ? find(ssl) : NULL;
    if (e && e->bridged) {
        ssize_t n;
        do {
            n = write(e->fd, buf, num);
        } while (n < 0 && errno == EINTR);

        e->last_errno = n < 0 ? errno : 0;
        if (n > 0) {
            *written = (size_t)n;
            return 1;
        }

        return 0;
    }
    return REAL(SSL_write_ex)(ssl, buf, num, written);
}

int SSL_write(SSL *ssl, const void *buf, int num) {
    struct entry *e = shim_enabled() ? find(ssl) : NULL;
    if (e && e->bridged) {
        ssize_t n;
        do {
            n = write(e->fd, buf, (size_t)num);
        } while (n < 0 && errno == EINTR);

        e->last_errno = n < 0 ? errno : 0;
        return (int)n;
    }

    return REAL(SSL_write)(ssl, buf, num);
}

int SSL_shutdown(SSL *ssl) {
    struct entry *e = shim_enabled() ? find(ssl) : NULL;
    if (e && e->bridged)
        return 1;

    return REAL(SSL_shutdown)(ssl);
}

void SSL_free(SSL *ssl) {
    if (shim_enabled())
        forget(ssl);

    REAL(SSL_free)(ssl);
}

int SSL_get_error(const SSL *ssl, int ret) {
    struct entry *e = shim_enabled() ? find((SSL *)ssl) : NULL;
    if (e && e->bridged) {
        if (ret > 0)
            return VPOD_SSL_ERROR_NONE;

        if (ret == 0)
            return VPOD_SSL_ERROR_ZERO_RETURN;

        if (e->last_errno == EAGAIN || e->last_errno == EWOULDBLOCK)
            return VPOD_SSL_ERROR_WANT_READ;

        return VPOD_SSL_ERROR_SYSCALL;
    }
    return REAL(SSL_get_error)(ssl, ret);
}

long SSL_get_verify_result(const SSL *ssl) {
    struct entry *e = shim_enabled() ? find((SSL *)ssl) : NULL;

    if (e && e->bridged)
        return 0;

    return REAL(SSL_get_verify_result)(ssl);
}

/*
 * For apk's libfetch, notably etc that inspect it
 */
static X509 *fake_peer_cert(void) {
    static X509 *cert;
    static int attempted;

    if (!cert && !attempted) {
        attempted = 1;
        const char *path = getenv("VPOD_SHIM_CERT");

        if (!path || !*path)
            path = "/etc/ssl/vpod/ca-only.pem";

        BIO *bio = REALC(BIO_new_file)(path, "r");
        if (bio) {
            cert = REALC(PEM_read_bio_X509)(bio, NULL, NULL, NULL);
            REALC(BIO_free)(bio);
        }
        log_line(cert ? "fake peer cert loaded" : "fake peer cert load FAILED");

    }
    return cert;
}

X509 *SSL_get1_peer_certificate(const SSL *ssl) {
    struct entry *e = shim_enabled() ? find((SSL *)ssl) : NULL;

    if (e && e->bridged) {
        X509 *cert = fake_peer_cert();
        if (cert) {
            REALC(X509_up_ref)(cert);
            return cert;
        }
    }

    return REAL(SSL_get1_peer_certificate)(ssl);
}

X509 *SSL_get_peer_certificate(const SSL *ssl) {
    struct entry *e = shim_enabled() ? find((SSL *)ssl) : NULL;

    if (e && e->bridged) {
        X509 *cert = fake_peer_cert();
        if (cert) {
            REALC(X509_up_ref)(cert);
            return cert;
        }
    }

    return REAL(SSL_get_peer_certificate)(ssl);
}

int X509_check_host(X509 *x, const char *chk, size_t chklen, unsigned int flags,
                    char **peername) {
    if (shim_enabled() && x && x == fake_peer_cert()) {
        if (peername)
            *peername = strdup(chk ? chk : "");
        return 1;
    }

    return REALC(X509_check_host)(x, chk, chklen, flags, peername);
}
