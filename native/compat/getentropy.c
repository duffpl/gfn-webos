/* getentropy for the glibc 2.12 baseline (added in glibc 2.25); aws-lc seeds its DRBG with it.
 * Uses the getrandom syscall where the kernel has it (3.17+), else /dev/urandom. */
#include <errno.h>
#include <fcntl.h>
#include <stddef.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef SYS_getrandom
#define SYS_getrandom 384 /* ARM EABI */
#endif

static int read_urandom(unsigned char *out, size_t length)
{
    int fd = open("/dev/urandom", O_RDONLY | O_CLOEXEC);
    if (fd < 0)
        return -1;
    while (length > 0) {
        ssize_t n = read(fd, out, length);
        if (n < 0 && errno == EINTR)
            continue;
        if (n <= 0) {
            close(fd);
            errno = EIO;
            return -1;
        }
        out += n;
        length -= (size_t)n;
    }
    close(fd);
    return 0;
}

int getentropy(void *buffer, size_t length)
{
    unsigned char *out = buffer;
    if (length > 256) {
        errno = EIO;
        return -1;
    }
    while (length > 0) {
        long n = syscall(SYS_getrandom, out, length, 0);
        if (n < 0 && errno == EINTR)
            continue;
        if (n < 0 && errno == ENOSYS)
            return read_urandom(out, length);
        if (n <= 0) {
            errno = EIO;
            return -1;
        }
        out += n;
        length -= (size_t)n;
    }
    return 0;
}
