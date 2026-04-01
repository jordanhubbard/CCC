/* SPDX-License-Identifier: BSD-2-Clause
 * vmm_shmem.h — VMM Shared Memory PD public interface
 *
 * Opcodes, structs, and constants for the vmm_shmem protection domain.
 * Enables zero-copy shared memory between agentOS WASM slots and Linux/CUDA
 * workloads via VMM-mapped memory regions.
 *
 * Target: riscv64-unknown-elf (seL4 Microkit)
 */
#pragma once

#include <stdint.h>
#include <stddef.h>

/* ─── IPC Opcodes ─────────────────────────────────────────────────────────── */

#define OP_VMM_MAP      0x90u   /* Map a shared region, return slot + phys_addr */
#define OP_VMM_UNMAP    0x91u   /* Unmap a region by slot index                */
#define OP_VMM_SYNC     0x92u   /* Memory barrier + update sync state          */
#define OP_VMM_STATUS   0x93u   /* Copy region table into ring buffer          */

/* ─── Return Codes ────────────────────────────────────────────────────────── */

#define VMM_OK          0u
#define VMM_ERR_FULL    1u      /* No free region slots                        */
#define VMM_ERR_INVAL   2u      /* Bad slot index or size                      */
#define VMM_ERR_STATE   3u      /* Region not in expected state                */

/* ─── Region Constants ────────────────────────────────────────────────────── */

#define VMM_MAX_REGIONS     8u
#define VMM_REGION_SIZE_MB  256u
#define VMM_RING_SIZE       (512u * 1024u)  /* 512 KB status ring */

/* Base simulated physical address for region table (non-overlapping) */
#define VMM_PHYS_BASE       0x40000000ULL
#define VMM_PHYS_STRIDE     (256ULL * 1024ULL * 1024ULL)  /* 256 MB per slot */

/* ─── Region State ────────────────────────────────────────────────────────── */

typedef enum {
    VMM_STATE_FREE    = 0,
    VMM_STATE_MAPPED  = 1,
    VMM_STATE_SYNCING = 2,
} vmm_region_state_t;

/* ─── Region Descriptor ───────────────────────────────────────────────────── */

typedef struct __attribute__((packed)) {
    uint64_t            phys_addr;  /* Simulated physical base address         */
    uint64_t            size;       /* Region size in bytes                    */
    uint32_t            slot_id;    /* WASM slot that owns this region (0-7)   */
    uint8_t             state;      /* vmm_region_state_t                      */
    uint8_t             _pad[3];
} vmm_region_t;

/* ─── Status Ring Header ──────────────────────────────────────────────────── */

typedef struct __attribute__((packed)) {
    uint32_t    magic;              /* 0x564D4D53 ('VMMS')                     */
    uint32_t    version;            /* Format version, currently 1             */
    uint32_t    region_count;       /* Number of valid entries in regions[]    */
    uint32_t    seq;                /* Monotonically increasing update counter */
    vmm_region_t regions[VMM_MAX_REGIONS];
} vmm_status_ring_t;

#define VMM_STATUS_MAGIC 0x564D4D53u

/* ─── IPC Message Layout ──────────────────────────────────────────────────── */
/*
 * All messages use seL4 Microkit msginfo registers:
 *   mr0 = opcode
 *   mr1 = arg0  (MAP: requested size; UNMAP/SYNC/STATUS: slot_id)
 *   mr2 = arg1  (MAP: caller slot_id; STATUS: unused)
 *
 * Replies:
 *   mr0 = return code (VMM_OK / VMM_ERR_*)
 *   mr1 = result0 (MAP: assigned slot index; STATUS: region_count written)
 *   mr2 = result1 (MAP: phys_addr low 32 bits)
 *   mr3 = result2 (MAP: phys_addr high 32 bits)
 */
