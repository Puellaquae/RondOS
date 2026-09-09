/* Minimal C interface to the frozen RondOS v1 syscall ABI (P2b).
 *
 * Numbers and layouts mirror user/lib/rondos-abi/src/lib.rs; the long-term plan
 * is to generate this header from the ABI crate (design §6.8), until then the
 * `static_assert`s below are the drift check.
 */
#ifndef RONDOS_H
#define RONDOS_H

#include <stdint.h>
#include <stddef.h>

#define RONDOS_ABI_VERSION 1

/* rax = syscall id, rdi/rsi/rdx/r10/r8/r9 = arg0..arg5,
 * rax = status, rdx = value.  Only rax/rdx are clobbered.
 *
 * rdx is both arg2 (input) and the returned value, so it is a read-write
 * operand — exactly like the Rust wrapper in rondos-abi. */

struct rondos_result { uint32_t status; uint32_t _pad; uint64_t value; };

static inline struct rondos_result
rondos_syscall6(uint64_t id, uint64_t a0, uint64_t a1, uint64_t a2,
                uint64_t a3, uint64_t a4, uint64_t a5)
{
    struct rondos_result r;
    register uint64_t r10 __asm__("r10") = a3;
    register uint64_t r8  __asm__("r8")  = a4;
    register uint64_t r9  __asm__("r9")  = a5;
    __asm__ volatile("int $0x80"
                     : "+a"(id), "+d"(a2), "+D"(a0), "+S"(a1)
                     : "r"(r10), "r"(r8), "r"(r9)
                     : "memory");
    r.status = (uint32_t)id;
    r.value = a2;
    return r;
}

enum rondos_status {
    RONDOS_OK = 0,
    RONDOS_UNSUPPORTED = 1,
    RONDOS_INVALID_ARGUMENT = 2,
    RONDOS_BAD_ADDRESS = 3,
    RONDOS_BAD_HANDLE = 4,
    RONDOS_PERMISSION = 5,
    RONDOS_NOT_FOUND = 6,
    RONDOS_OUT_OF_MEMORY = 7,
    RONDOS_NOT_READY = 8,
    RONDOS_CANCELLED = 9,
    RONDOS_BROKEN = 10,
    RONDOS_FAULT = 11,
};

/* Frozen syscall ids (user/lib/rondos-abi). */
#define RONDOS_SYS_INFO            0x00
#define RONDOS_SYS_EXIT            0x10
#define RONDOS_SYS_THREAD_EXIT     0x11
#define RONDOS_SYS_THREAD_SPAWN    0x12
#define RONDOS_SYS_YIELD           0x13
#define RONDOS_SYS_SLEEP_NS        0x14
#define RONDOS_SYS_CLOCK_GETTIME   0x15
#define RONDOS_SYS_LOG             0x16
#define RONDOS_SYS_SPAWN           0x20
#define RONDOS_SYS_WAIT            0x21
#define RONDOS_SYS_PROC_STATUS     0x22
#define RONDOS_SYS_KILL            0x23
#define RONDOS_SYS_MEM_MAP         0x30
#define RONDOS_SYS_MEM_UNMAP       0x31
#define RONDOS_SYS_MEM_SHARE       0x32
#define RONDOS_SYS_MEM_MAP_PHYS    0x33
#define RONDOS_SYS_CHAN_CREATE     0x40
#define RONDOS_SYS_CHAN_SEND       0x41
#define RONDOS_SYS_CHAN_RECV       0x42
#define RONDOS_SYS_CHAN_CLOSE      0x43
#define RONDOS_SYS_OPEN            0x50
#define RONDOS_SYS_READ            0x51
#define RONDOS_SYS_WRITE           0x52
#define RONDOS_SYS_SEEK            0x53
#define RONDOS_SYS_STAT            0x54
#define RONDOS_SYS_READDIR         0x55
#define RONDOS_SYS_CLOSE           0x56
#define RONDOS_SYS_UNLINK          0x57

struct rondos_handle { uint64_t raw; };
struct rondos_strref { const char *ptr; uint64_t len; };
struct rondos_slice  { const void *ptr; uint64_t count; };
struct rondos_capdesc { uint32_t kind; uint32_t _pad; uint64_t rights; uint64_t handle; };

/* `sys_stat` output — mirrors rondos_abi::Stat field for field. */
struct rondos_stat {
    uint32_t hdr_size, hdr_version;
    uint32_t kind, _pad0;
    uint64_t len_bytes;
    uint64_t va;
    uint64_t rights;
    uint64_t _reserved[2];
};

_Static_assert(sizeof(struct rondos_result) == 16, "rondos_result layout");
_Static_assert(sizeof(struct rondos_stat) == 56, "rondos_stat layout");
_Static_assert(offsetof(struct rondos_stat, va) == 24, "rondos_stat.va offset");

struct rondos_startup_block {
    uint32_t hdr_size;
    uint32_t hdr_version;
    uint32_t abi_version;
    uint32_t _pad0;
    uint64_t feature_bits;
    uint64_t entry;
    uint64_t image_base;
    struct rondos_slice argv;
    struct rondos_slice envp;
    struct rondos_slice caps;
    uint64_t window;
    uint64_t random_seed;
    uint64_t _reserved[4];
};

_Static_assert(sizeof(struct rondos_startup_block) == 136, "StartupBlock layout");
_Static_assert(offsetof(struct rondos_startup_block, caps) == 72, "caps offset");
_Static_assert(sizeof(struct rondos_capdesc) == 24, "CapDesc layout");

/* ---- wrappers -------------------------------------------------------- */

__attribute__((noreturn))
static inline void rondos_exit(uint32_t status)
{
    rondos_syscall6(RONDOS_SYS_EXIT, status, 0, 0, 0, 0, 0);
    for (;;) { __asm__ volatile("pause"); }
}

static inline int rondos_log(const char *buf, size_t len)
{
    struct rondos_result r = rondos_syscall6(RONDOS_SYS_LOG, 2, (uint64_t)buf, len, 0, 0, 0);
    return r.status == RONDOS_OK ? (int)r.value : -(int)r.status;
}

static inline struct rondos_handle rondos_open(struct rondos_handle dir,
                                               const char *path, size_t len)
{
    struct rondos_result r = rondos_syscall6(RONDOS_SYS_OPEN, dir.raw,
                                             (uint64_t)path, len, 1, 0, 0);
    return (struct rondos_handle){ r.status == RONDOS_OK ? r.value : ~0ull };
}

static inline int rondos_read(struct rondos_handle h, void *buf, size_t len)
{
    struct rondos_result r = rondos_syscall6(RONDOS_SYS_READ, h.raw,
                                             (uint64_t)buf, len, 0, 0, 0);
    return r.status == RONDOS_OK ? (int)r.value : -(int)r.status;
}

static inline int rondos_write(struct rondos_handle h, const void *buf, size_t len)
{
    struct rondos_result r = rondos_syscall6(RONDOS_SYS_WRITE, h.raw,
                                             (uint64_t)buf, len, 0, 0, 0);
    return r.status == RONDOS_OK ? (int)r.value : -(int)r.status;
}

static inline void rondos_close(struct rondos_handle h)
{
    rondos_syscall6(RONDOS_SYS_CLOSE, h.raw, 0, 0, 0, 0, 0);
}

static inline void rondos_sleep_ns(uint64_t ns)
{
    rondos_syscall6(RONDOS_SYS_SLEEP_NS, ns, 0, 0, 0, 0, 0);
}

/* The root directory capability, taken from the StartupBlock. */
static inline struct rondos_handle rondos_root_dir(const struct rondos_startup_block *b)
{
    const struct rondos_capdesc *caps = (const struct rondos_capdesc *)b->caps.ptr;
    for (uint64_t i = 0; i < b->caps.count; i++) {
        if (caps[i].kind == 6 /* ObjKind::Dir */) {
            return (struct rondos_handle){ caps[i].handle };
        }
    }
    return (struct rondos_handle){ ~0ull };
}

#endif /* RONDOS_H */
