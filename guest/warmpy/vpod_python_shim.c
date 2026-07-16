/*
 * vpod python shim: client installed over /usr/bin/python3.
 */

#include <errno.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <sys/wait.h>
#include <unistd.h>

extern char **environ;

static char **g_argv;

static const char *real_python(void) {
    const char *p = getenv("VPOD_PYTHON_REAL");
    return (p && *p) ? p : "/usr/bin/python3.real";
}

static const char *sock_path(void) {
    const char *p = getenv("VPOD_PYD_SOCK");
    return (p && *p) ? p : "/run/vpod-pyd.sock";
}

static void fallback(void) {
    signal(SIGPIPE, SIG_DFL);
    execv(real_python(), g_argv);
    perror("vpod-python-shim: exec python3.real");
    _exit(127);
}

static volatile pid_t child_pid = 0;

static void forward_signal(int sig) {
    pid_t pid = child_pid;
    if (pid > 0) {
        kill(pid, sig);
    }
}

static void install_forwarders(void) {
    static const int sigs[] = {SIGINT, SIGTERM, SIGHUP, SIGQUIT, SIGUSR1, SIGUSR2};
    struct sigaction sa;
    memset(&sa, 0, sizeof(sa));

    sa.sa_handler = forward_signal;
    sa.sa_flags = SA_RESTART;
    sigemptyset(&sa.sa_mask);

    for (size_t i = 0; i < sizeof(sigs) / sizeof(sigs[0]); i++) {
        sigaction(sigs[i], &sa, NULL);
    }
}

static int full_read(int fd, void *buf, size_t n) {
    char *p = buf;
    while (n > 0) {
        ssize_t r = read(fd, p, n);
        if (r < 0) {
            if (errno == EINTR) continue;
            return -1;
        }
        if (r == 0) return -1;
        p += r;
        n -= (size_t)r;
    }
    return 0;
}

static int full_write(int fd, const void *buf, size_t n) {
    const char *p = buf;
    while (n > 0) {
        ssize_t r = write(fd, p, n);
        if (r < 0) {
            if (errno == EINTR) continue;
            return -1;
        }
        p += r;
        n -= (size_t)r;
    }
    return 0;
}

static char *build_payload(int argc, char **argv, uint32_t *out_len) {
    char argc_str[16];
    int argc_len = snprintf(argc_str, sizeof(argc_str), "%d", argc);

    char cwd[4096];
    if (!getcwd(cwd, sizeof(cwd))) {
        strcpy(cwd, "/");
    }

    size_t len = (size_t)argc_len + 1;
    for (int i = 0; i < argc; i++) len += strlen(argv[i]) + 1;
    len += strlen(cwd) + 1;
    for (char **e = environ; *e; e++) len += strlen(*e) + 1;

    char *buf = malloc(len);
    if (!buf) return NULL;

    char *p = buf;
    memcpy(p, argc_str, (size_t)argc_len + 1);
    p += argc_len + 1;
    for (int i = 0; i < argc; i++) {
        size_t l = strlen(argv[i]) + 1;
        memcpy(p, argv[i], l);
        p += l;
    }

    size_t l = strlen(cwd) + 1;
    memcpy(p, cwd, l);
    p += l;
    for (char **e = environ; *e; e++) {
        l = strlen(*e) + 1;
        memcpy(p, *e, l);
        p += l;
    }

    *out_len = (uint32_t)len;
    return buf;
}

static int send_request(int fd, int argc, char **argv) {
    uint32_t payload_len;
    char *payload = build_payload(argc, argv, &payload_len);
    if (!payload) return -1;

    unsigned char header[8] = {'V', 'P', 'Y', '1'};
    header[4] = (unsigned char)(payload_len & 0xff);
    header[5] = (unsigned char)((payload_len >> 8) & 0xff);
    header[6] = (unsigned char)((payload_len >> 16) & 0xff);
    header[7] = (unsigned char)((payload_len >> 24) & 0xff);

    int fds[3] = {0, 1, 2};
    union {
        char buf[CMSG_SPACE(sizeof(fds))];
        struct cmsghdr align;
    } cmsg_storage;
    memset(&cmsg_storage, 0, sizeof(cmsg_storage));

    struct iovec iov = {.iov_base = header, .iov_len = sizeof(header)};
    struct msghdr msg;
    memset(&msg, 0, sizeof(msg));
    msg.msg_iov = &iov;
    msg.msg_iovlen = 1;
    msg.msg_control = cmsg_storage.buf;
    msg.msg_controllen = sizeof(cmsg_storage.buf);

    struct cmsghdr *cmsg = CMSG_FIRSTHDR(&msg);
    cmsg->cmsg_level = SOL_SOCKET;
    cmsg->cmsg_type = SCM_RIGHTS;
    cmsg->cmsg_len = CMSG_LEN(sizeof(fds));
    memcpy(CMSG_DATA(cmsg), fds, sizeof(fds));

    ssize_t sent;
    do {
        sent = sendmsg(fd, &msg, 0);
    } while (sent < 0 && errno == EINTR);
    if (sent != (ssize_t)sizeof(header)) {
        free(payload);
        return -1;
    }

    int rc = full_write(fd, payload, payload_len);
    free(payload);
    return rc;
}

int main(int argc, char **argv) {
    g_argv = argv;

    int fd = socket(AF_UNIX, SOCK_STREAM, 0);
    if (fd < 0) fallback();

    struct sockaddr_un addr;
    memset(&addr, 0, sizeof(addr));
    addr.sun_family = AF_UNIX;
    const char *path = sock_path();
    if (strlen(path) >= sizeof(addr.sun_path)) fallback();
    strcpy(addr.sun_path, path);

    if (connect(fd, (struct sockaddr *)&addr, sizeof(addr)) < 0) fallback();

    signal(SIGPIPE, SIG_IGN);
    if (send_request(fd, argc, argv) < 0) fallback();

    int started = 0;
    for (;;) {
        unsigned char tag;
        if (full_read(fd, &tag, 1) < 0) {
            if (!started) fallback();

            fprintf(stderr, "vpod-python-shim: server connection lost\n");
            return 1;
        }
        if (tag == 'F') {
            fallback();
        } else if (tag == 'P') {
            unsigned char b[4];
            if (full_read(fd, b, 4) < 0) return 1;
            child_pid = (pid_t)(b[0] | (b[1] << 8) | ((uint32_t)b[2] << 16) |
                                ((uint32_t)b[3] << 24));
            started = 1;
            install_forwarders();
        } else if (tag == 'X') {
            unsigned char b[4];
            if (full_read(fd, b, 4) < 0) return 1;
            uint32_t status = (uint32_t)b[0] | ((uint32_t)b[1] << 8) |
                              ((uint32_t)b[2] << 16) | ((uint32_t)b[3] << 24);

            if (WIFEXITED(status)) return WEXITSTATUS(status);
            if (WIFSIGNALED(status)) {
                int sig = WTERMSIG(status);
                signal(sig, SIG_DFL);
                raise(sig);
                return 128 + sig;
            }
            return 1;
        } else {
            if (!started) fallback();
            return 1;
        }
    }
}
