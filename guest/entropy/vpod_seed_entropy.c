#include <fcntl.h>
#include <linux/random.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <unistd.h>

#ifndef RNDRESEEDCRNG
#define RNDRESEEDCRNG _IO('R', 0x07)
#endif

#define MAX_SEED_BYTES 512

static int unhex(char digit) {
    if (digit >= '0' && digit <= '9')
        return digit - '0';

    if (digit >= 'a' && digit <= 'f')
        return digit - 'a' + 10;

    if (digit >= 'A' && digit <= 'F')
        return digit - 'A' + 10;

    return -1;
}

int main(int argc, char **argv) {

    struct {
        struct rand_pool_info info;
        unsigned char buf[MAX_SEED_BYTES];
    } pool;

    if (argc != 2) {
        fprintf(stderr, "usage: vpod-seed-entropy <hex>\n");
        return 1;
    }

    size_t digits = strlen(argv[1]);
    if (digits == 0 || digits % 2 != 0 || digits / 2 > MAX_SEED_BYTES) {
        fprintf(stderr, "vpod-seed-entropy: need an even count of at most %d hex digits, got %zu\n",
                MAX_SEED_BYTES * 2, digits);

        return 1;
    }

    size_t seed_bytes = digits / 2;
    for (size_t i = 0; i < seed_bytes; i++) {
        int high = unhex(argv[1][i * 2]);
        int low = unhex(argv[1][i * 2 + 1]);

        if (high < 0 || low < 0) {
            fprintf(stderr, "vpod-seed-entropy: argument is not hex\n");
            return 1;
        }

        pool.buf[i] = (unsigned char)((high << 4) | low);
    }

    int fd = open("/dev/urandom", O_WRONLY);

    if (fd < 0) {
        perror("vpod-seed-entropy: open /dev/urandom");
        return 1;
    }

    pool.info.entropy_count = (int)(seed_bytes * 8);
    pool.info.buf_size = (int)seed_bytes;

    if (ioctl(fd, RNDADDENTROPY, &pool) != 0) {
        perror("vpod-seed-entropy: RNDADDENTROPY");
        close(fd);
        return 1;
    }

    if (ioctl(fd, RNDRESEEDCRNG, 0) != 0) {
        perror("vpod-seed-entropy: RNDRESEEDCRNG");
        close(fd);
        return 1;
    }

    close(fd);
    return 0;
}
