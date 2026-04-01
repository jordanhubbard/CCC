/* SPDX-License-Identifier: BSD-2-Clause
 * init_agent.c — agentOS root task / agent initialization stub
 *
 * This file is the initialization entry point for the agentOS root task.
 * It spawns and configures the system protection domains on boot.
 *
 * Protection domains registered here (in priority order):
 *
 *   Priority  PD              Description
 *   --------  --------------- ------------------------------------------------
 *   200       cap_audit_log   Capability grant/revoke audit trail
 *   180       watchdog        Heartbeat monitor — resets stalled agent slots
 *   160       quota_pd        Per-slot CPU time quota enforcement
 *   150       mem_profiler    Memory usage profiler / high-watermark tracker
 *   140       vmm_shmem       VMM shared memory — zero-copy WASM ↔ CUDA buffer
 *                             sharing via Linux mmap of VMM-mapped regions
 *                             (see vmm_shmem.h / vmm_shmem.c)
 *   100       fault_handler   WASM slot exception / SIGSEGV handler
 *
 * NOTE: This file is a skeleton stub. Full root-task initialization logic
 * (seL4 untyped management, CNode construction, scheduling context setup)
 * lives on the production seL4 build on Sparky and will be merged here when
 * the kernel submodule is committed.
 *
 * Target: riscv64-unknown-elf (seL4 Microkit)
 */

#include <microkit.h>
#include "vmm_shmem.h"

/* vmm_shmem channel assignment — see agentos.system SDF */
#define CH_VMM_SHMEM_WASM0   100
#define CH_VMM_SHMEM_WASM1   101
#define CH_VMM_SHMEM_WASM2   102
#define CH_VMM_SHMEM_WASM3   103
#define CH_VMM_SHMEM_WASM4   104
#define CH_VMM_SHMEM_WASM5   105
#define CH_VMM_SHMEM_WASM6   106
#define CH_VMM_SHMEM_WASM7   107

void
init(void)
{
    /*
     * TODO: full root task init sequence:
     *  1. Enumerate untyped memory from bootinfo
     *  2. Retype frames for each PD's IPC endpoints and shared MRs
     *  3. Assign scheduling contexts (passive PDs get none)
     *  4. Bootstrap vmm_shmem region table (vmm_shmem.c::init called by Microkit)
     *  5. Mint capabilities for vmm_shmem shared MR to Linux VMM driver
     */
}

void
notified(microkit_channel ch)
{
    (void)ch;
}
