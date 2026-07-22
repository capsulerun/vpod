/*
 * vpod ssl_client, replacement for busybox's ssl_client that does no TLS.
 */

#include <arpa/inet.h>
#include <errno.h>
#include <poll.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

#define REAL_SSL_CLIENT "/usr/bin/ssl_client.real"

static void run_real_ssl_client(char **argv) {
    execv(REAL_SSL_CLIENT, argv);
    fprintf(stderr, "vpod ssl_client: exec %s: %s\n", REAL_SSL_CLIENT, strerror(errno));

    exit(1);
}

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

static int splice_ready(int src, int dst, int *open_flag) {
    char buf[16384];
    ssize_t n = read(src, buf, sizeof(buf));

    if (n < 0)
        return (errno == EINTR || errno == EAGAIN) ? 0 : -1;

    if (n == 0) {
        *open_flag = 0;
        return 0;
    }

    return write_all(dst, buf, (size_t)n);
}

int main(int argc, char **argv) {
    int net_fd = -1;
    const char *sni = NULL;

    for (int i = 1; i < argc; i++) {
        if ((strcmp(argv[i], "-s") == 0 || strcmp(argv[i], "-h") == 0) && i + 1 < argc) {
            net_fd = atoi(argv[++i]);
        } else if ((strncmp(argv[i], "-s", 2) == 0 || strncmp(argv[i], "-h", 2) == 0) && argv[i][2] != '\0') {
            net_fd = atoi(argv[i] + 2);
        } else if (strcmp(argv[i], "-n") == 0 && i + 1 < argc) {
            sni = argv[++i];
        } else if (strncmp(argv[i], "-n", 2) == 0 && argv[i][2] != '\0') {
            sni = argv[i] + 2;
        }
        /* other busybox options */
    }

    if (net_fd < 0 || sni == NULL) {
        run_real_ssl_client(argv);
    }

    struct sockaddr_in peer;
    socklen_t peer_len = sizeof(peer);
    unsigned port = 443;

    if (getpeername(net_fd, (struct sockaddr *)&peer, &peer_len) == 0 &&
        peer.sin_family == AF_INET) {
        port = ntohs(peer.sin_port);
    }

    if (port != 443) {
        run_real_ssl_client(argv);
    }

    char preamble[300];
    int len = snprintf(preamble, sizeof(preamble), "VPOD-CONNECT %s %u\n", sni, port);
    if (len <= 0 || (size_t)len >= sizeof(preamble) ||
        write_all(net_fd, preamble, (size_t)len) != 0) {
        fprintf(stderr, "vpod ssl_client: preamble write failed\n");
        return 1;
    }

    int stdin_open = 1, net_open = 1;
    while (net_open) {
        struct pollfd fds[2];
        nfds_t nfds = 0;
        int stdin_slot = -1, net_slot = -1;

        if (stdin_open) {
            stdin_slot = (int)nfds;
            fds[nfds].fd = 0;
            fds[nfds].events = POLLIN;
            nfds++;
        }

        net_slot = (int)nfds;
        fds[nfds].fd = net_fd;
        fds[nfds].events = POLLIN;
        nfds++;

        if (poll(fds, nfds, -1) < 0) {
            if (errno == EINTR)
                continue;

            return 1;
        }

        if (stdin_slot >= 0 && (fds[stdin_slot].revents & (POLLIN | POLLHUP))) {
            if (splice_ready(0, net_fd, &stdin_open) != 0)
                return 1;

            if (!stdin_open)
                shutdown(net_fd, SHUT_WR);
        }
        if (fds[net_slot].revents & (POLLIN | POLLHUP)) {
            if (splice_ready(net_fd, 1, &net_open) != 0)
                return 1;
        }

        if (fds[net_slot].revents & (POLLERR | POLLNVAL))
            return 1;
    }

    return 0;
}
