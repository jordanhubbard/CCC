/* SPDX-License-Identifier: BSD-2-Clause
 * vmm_shmem.c — VMM Shared Memory Protection Domain
 *
 * Passive PD (priority 140) that manages a table of shared memory regions
 * for zero-copy buffer handoff between agentOS WASM agent slots and Linux/CUDA
 * workloads on the host side.
 *
 * Opcodes:
 *   OP_VMM_MAP    (0x90)  Allocate a region, return slot index + simulated phys_addr
 *   OP_VMM_UNMAP  (0x91)  Free a region by slot index
 *   OP_VMM_SYNC   (0x92)  Issue memory barrier, mark region MAPPED again
 *   OP_VMM_STATUS (0x93)  Serialize region table into the 512 KB status ring
 *
 * Build target: riscv64-unknown-elf (seL4 Microkit)
 * Priority:     140  (passive — only runs when ppcalled)
 */

#include "vmm_shmem.h"
#include <microkit.h>

/* ─── Static region table ─────────────────────────────────────────────────── */

static vmm_region_t g_regions[VMM_MAX_REGIONS];

/* 512 KB status ring exported to the Linux side via a shared MR capability.
 * In a real seL4 build this would be a seL4_FrameObject delegated to the PD;
 * here we declare it as a flat array so the linker places it in .data. */
static vmm_status_ring_t g_ring __attribute__((aligned(4096)));

/* Monotonic sequence counter for ring updates */
static uint32_t g_seq = 0;

/* ─── Helpers ─────────────────────────────────────────────────────────────── */

static void
regions_init(void)
{
    for (int i = 0; i < VMM_MAX_REGIONS; i++) {
        g_regions[i].phys_addr = VMM_PHYS_BASE + (uint64_t)i * VMM_PHYS_STRIDE;
        g_regions[i].size      = 0;
        g_regions[i].slot_id   = 0xFFFFFFFFu;
        g_regions[i].state     = (uint8_t)VMM_STATE_FREE;
    }
}

static int
find_free_slot(void)
{
    for (int i = 0; i < VMM_MAX_REGIONS; i++) {
        if ((vmm_region_state_t)g_regions[i].state == VMM_STATE_FREE)
            return i;
    }
    return -1;
}

/* Publish the current region table into the status ring. */
static void
update_ring(void)
{
    g_ring.magic        = VMM_STATUS_MAGIC;
    g_ring.version      = 1;
    g_ring.seq          = ++g_seq;
    g_ring.region_count = VMM_MAX_REGIONS;

    for (int i = 0; i < VMM_MAX_REGIONS; i++) {
        g_ring.regions[i] = g_regions[i];
    }
}

/* Portable memory barrier.
 * On bare-metal RISC-V use "fence rw,rw"; on ARM64 simulation hosts this
 * would be "dsb sy".  The __asm__ volatile here ensures the compiler does
 * not reorder surrounding loads/stores regardless of target. */
static inline void
memory_barrier(void)
{
#if defined(__riscv)
    __asm__ volatile ("fence rw,rw" ::: "memory");
#elif defined(__aarch64__)
    __asm__ volatile ("dsb sy" ::: "memory");
#else
    __asm__ volatile ("" ::: "memory");
#endif
}

/* ─── Opcode handlers ─────────────────────────────────────────────────────── */

/*
 * OP_VMM_MAP
 *   In:  mr1 = requested size (bytes), mr2 = caller's WASM slot_id
 *   Out: mr0 = VMM_OK / VMM_ERR_*
 *        mr1 = assigned region index
 *        mr2 = phys_addr[31:0]
 *        mr3 = phys_addr[63:32]
 */
static microkit_msginfo
handle_map(microkit_msginfo msginfo)
{
    uint64_t req_size = (uint64_t)microkit_mr_get(1);
    uint32_t caller_slot = (uint32_t)microkit_mr_get(2);

    if (req_size == 0) {
        microkit_mr_set(0, VMM_ERR_INVAL);
        return microkit_msginfo_new(0, 1);
    }

    int idx = find_free_slot();
    if (idx < 0) {
        microkit_mr_set(0, VMM_ERR_FULL);
        return microkit_msginfo_new(0, 1);
    }

    g_regions[idx].size    = req_size;
    g_regions[idx].slot_id = caller_slot;
    g_regions[idx].state   = (uint8_t)VMM_STATE_MAPPED;

    uint64_t paddr = g_regions[idx].phys_addr;
    microkit_mr_set(0, VMM_OK);
    microkit_mr_set(1, (uint64_t)idx);
    microkit_mr_set(2, (uint64_t)(paddr & 0xFFFFFFFFu));
    microkit_mr_set(3, (uint64_t)(paddr >> 32));

    update_ring();

    return microkit_msginfo_new(0, 4);
}

/*
 * OP_VMM_UNMAP
 *   In:  mr1 = region index to free
 *   Out: mr0 = VMM_OK / VMM_ERR_INVAL
 */
static microkit_msginfo
handle_unmap(microkit_msginfo msginfo)
{
    uint32_t idx = (uint32_t)microkit_mr_get(1);

    if (idx >= VMM_MAX_REGIONS) {
        microkit_mr_set(0, VMM_ERR_INVAL);
        return microkit_msginfo_new(0, 1);
    }

    if ((vmm_region_state_t)g_regions[idx].state == VMM_STATE_FREE) {
        microkit_mr_set(0, VMM_ERR_STATE);
        return microkit_msginfo_new(0, 1);
    }

    g_regions[idx].size    = 0;
    g_regions[idx].slot_id = 0xFFFFFFFFu;
    g_regions[idx].state   = (uint8_t)VMM_STATE_FREE;

    update_ring();

    microkit_mr_set(0, VMM_OK);
    return microkit_msginfo_new(0, 1);
}

/*
 * OP_VMM_SYNC
 *   In:  mr1 = region index
 *   Out: mr0 = VMM_OK / VMM_ERR_INVAL / VMM_ERR_STATE
 *
 * Transitions MAPPED → SYNCING → MAPPED with a full memory barrier in between.
 * This is the signal to the Linux side that the WASM slot has finished writing
 * to the buffer and the host can safely read it (or vice versa).
 */
static microkit_msginfo
handle_sync(microkit_msginfo msginfo)
{
    uint32_t idx = (uint32_t)microkit_mr_get(1);

    if (idx >= VMM_MAX_REGIONS) {
        microkit_mr_set(0, VMM_ERR_INVAL);
        return microkit_msginfo_new(0, 1);
    }

    if ((vmm_region_state_t)g_regions[idx].state != VMM_STATE_MAPPED) {
        microkit_mr_set(0, VMM_ERR_STATE);
        return microkit_msginfo_new(0, 1);
    }

    g_regions[idx].state = (uint8_t)VMM_STATE_SYNCING;
    update_ring();

    memory_barrier();

    g_regions[idx].state = (uint8_t)VMM_STATE_MAPPED;
    update_ring();

    microkit_mr_set(0, VMM_OK);
    return microkit_msginfo_new(0, 1);
}

/*
 * OP_VMM_STATUS
 *   In:  (none)
 *   Out: mr0 = VMM_OK
 *        mr1 = number of regions written into ring
 *
 * The full region table is always in g_ring (updated on every MAP/UNMAP/SYNC).
 * This call just returns the live count so callers can confirm freshness.
 */
static microkit_msginfo
handle_status(microkit_msginfo msginfo)
{
    update_ring();
    microkit_mr_set(0, VMM_OK);
    microkit_mr_set(1, VMM_MAX_REGIONS);
    return microkit_msginfo_new(0, 2);
}

/* ─── Microkit entry points ───────────────────────────────────────────────── */

void
init(void)
{
    regions_init();

    /* Zero-init ring with magic + version so consumers can detect readiness. */
    g_ring.magic        = VMM_STATUS_MAGIC;
    g_ring.version      = 1;
    g_ring.seq          = 0;
    g_ring.region_count = VMM_MAX_REGIONS;
    for (int i = 0; i < VMM_MAX_REGIONS; i++) {
        g_ring.regions[i] = g_regions[i];
    }
}

/* Protected procedure call handler — this PD is passive, so all work happens
 * here.  The channel parameter identifies the caller PD. */
microkit_msginfo
protected(microkit_channel ch, microkit_msginfo msginfo)
{
    uint64_t opcode = microkit_mr_get(0);

    switch (opcode) {
    case OP_VMM_MAP:
        return handle_map(msginfo);
    case OP_VMM_UNMAP:
        return handle_unmap(msginfo);
    case OP_VMM_SYNC:
        return handle_sync(msginfo);
    case OP_VMM_STATUS:
        return handle_status(msginfo);
    default:
        microkit_mr_set(0, VMM_ERR_INVAL);
        return microkit_msginfo_new(0, 1);
    }
}

/* vmm_shmem is passive — it never receives async notifications. */
void
notified(microkit_channel ch)
{
    (void)ch;
}
