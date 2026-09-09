/* Minimal C runtime: malloc/free over one sys_mem_map region (P2d).
 *
 * Same design as the Rust allocator in rondos-rt: first-fit free list, 16-byte
 * alignment, coalescing on free.  v1 has one thread per process, so no lock is
 * needed (see the comment in rondos-rt::heap).
 */

#include "rondos.h"

#define HEAP_SIZE  (64u * 1024u)
#define HDR        16u
#define ALIGNMENT  16u

struct block {
    size_t size;        /* usable bytes after this header */
    struct block *next;
};

static char *heap_base;
static struct block *free_list;
static struct rondos_handle heap_handle = { ~0ull };

static int heap_init(void)
{
    if (free_list)
        return 1;
    struct rondos_result r = rondos_syscall6(0x30 /* sys_mem_map */, HEAP_SIZE,
                                            1 | 2 /* READ|WRITE */, 0, 0, 0, 0);
    if (r.status != RONDOS_OK)
        return 0;
    heap_handle.raw = r.value;
    /* sys_stat gives us the mapping address. */
    struct { uint32_t hdr_size, hdr_version, kind, pad0;
             uint64_t len_bytes, va, rights, res0, res1; } st = {0};
    r = rondos_syscall6(0x54 /* sys_stat */, heap_handle.raw, (uint64_t)&st, 0, 0, 0, 0);
    if (r.status != RONDOS_OK || st.va == 0)
        return 0;
    heap_base = (char *)st.va;
    free_list = (struct block *)heap_base;
    free_list->size = HEAP_SIZE - HDR;
    free_list->next = 0;
    return 1;
}

void *malloc(size_t n)
{
    if (n == 0 || !heap_init())
        return 0;
    size_t need = (n + ALIGNMENT - 1) & ~(size_t)(ALIGNMENT - 1);

    struct block *prev = 0, *cur = free_list;
    while (cur) {
        if (cur->size >= need) {
            if (cur->size >= need + HDR) {
                struct block *rest = (struct block *)((char *)cur + HDR + need);
                rest->size = cur->size - need - HDR;
                rest->next = cur->next;
                cur->size = need;
                cur->next = rest;
                if (prev) prev->next = rest; else free_list = rest;
            } else {
                if (prev) prev->next = cur->next; else free_list = cur->next;
            }
            return (char *)cur + HDR;
        }
        prev = cur;
        cur = cur->next;
    }
    return 0;
}

void free(void *p)
{
    if (!p)
        return;
    struct block *blk = (struct block *)((char *)p - HDR);
    size_t size = blk->size;

    struct block *prev = 0, *cur = free_list;
    while (cur && (char *)cur < (char *)blk) {
        prev = cur;
        cur = cur->next;
    }
    blk->next = cur;
    if (prev) prev->next = blk; else free_list = blk;

    if (cur && (char *)blk + HDR + size == (char *)cur) {
        blk->size += HDR + cur->size;
        blk->next = cur->next;
    }
    if (prev && (char *)prev + HDR + prev->size == (char *)blk) {
        prev->size += HDR + blk->size;
        prev->next = blk->next;
    }
}
