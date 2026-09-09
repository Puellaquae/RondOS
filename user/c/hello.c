/* A C program on RondOS: no libc, no CRT beyond crt0.S — just the ABI header. */

#include "rondos.h"

static void puts_raw(const char *s, size_t n)
{
    rondos_log(s, n);
}

/* Length of a string literal without its NUL: hand-written numbers drifted. */
#define LIT(s) puts_raw((s), sizeof(s) - 1)

static size_t strlen_(const char *s)
{
    size_t n = 0;
    while (s[n]) n++;
    return n;
}

/* malloc/free live in rondos.c, which is linked next to crt0.S. */
extern void *malloc(size_t);
extern void free(void *);

int main(const struct rondos_startup_block *block)
{
    LIT("chello: hello from C on RondOS\n");

    struct rondos_handle root = rondos_root_dir(block);
    if (root.raw == ~0ull) {
        LIT("chello: no root capability\n");
        return 1;
    }

    const char *path = "/bin/hello.c";
    struct rondos_handle f = rondos_open(root, path, strlen_(path));
    if (f.raw == ~0ull) {
        LIT("chello: cannot open /bin/hello.c\n");
        return 2;
    }
    char buf[64];
    int n = rondos_read(f, buf, sizeof buf);
    rondos_close(f);
    if (n <= 0) {
        LIT("chello: read failed\n");
        return 3;
    }
    LIT("chello: read back its own source: ");
    puts_raw(buf, (size_t)(n < 32 ? n : 32));
    LIT("\n");

    /* The heap: allocate, use, free, and allocate again from the freed space. */
    char *heap = malloc(1024);
    if (!heap) {
        LIT("chello: malloc failed\n");
        return 4;
    }
    for (int i = 0; i < 1024; i++)
        heap[i] = (char)(i & 0x7f);
    free(heap);
    char *again = malloc(1024);
    if (again != heap) {
        LIT("chello: malloc did not reuse the freed block\n");
        return 5;
    }
    free(again);
    LIT("chello: malloc/free ok\n");
    return 0;
}
