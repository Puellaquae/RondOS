/* A C program on RondOS: no libc, no CRT beyond crt0.S — just the ABI header. */

#include "rondos.h"

static void puts_raw(const char *s, size_t n)
{
    rondos_log(s, n);
}

static size_t strlen_(const char *s)
{
    size_t n = 0;
    while (s[n]) n++;
    return n;
}

int main(const struct rondos_startup_block *block)
{
    puts_raw("chello: hello from C on RondOS\n", 31);

    struct rondos_handle root = rondos_root_dir(block);
    if (root.raw == ~0ull) {
        puts_raw("chello: no root capability\n", 27);
        return 1;
    }

    const char *path = "/bin/hello.c";
    struct rondos_handle f = rondos_open(root, path, strlen_(path));
    if (f.raw == ~0ull) {
        puts_raw("chello: cannot open /bin/hello.c\n", 32);
        return 2;
    }
    char buf[64];
    int n = rondos_read(f, buf, sizeof buf);
    rondos_close(f);
    if (n <= 0) {
        puts_raw("chello: read failed\n", 20);
        return 3;
    }
    puts_raw("chello: read back its own source: ", 33);
    puts_raw(buf, (size_t)(n < 32 ? n : 32));
    puts_raw("\n", 1);
    return 0;
}
