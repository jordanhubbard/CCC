# VMM Shared Memory — agentOS ↔ Linux/CUDA Zero-Copy

**Status:** Design + PD stub committed
**Author:** Natasha
**Date:** 2026-03-31
**PD source:** `kernel/agentos-root-task/src/vmm_shmem.{c,h}`

---

## Problem

agentOS WASM agent slots execute in isolated seL4 protection domains with no
direct access to host physical memory.  When an agent needs to hand a large
tensor (e.g. a 64 MB activation map from a CUDA inference run) back to its
WASM slot — or vice versa — the naive approach is a copy through the seL4 IPC
ring, which is:

- **Slow** — seL4 message registers hold at most ~120 bytes per call; streaming
  64 MB takes tens of thousands of IPC round-trips.
- **Latency-bound** — each round-trip adds scheduler overhead even on a passive PD.
- **Wasteful** — two copies of every tensor in memory simultaneously.

For GPU workloads on Sparky (128 GB unified memory, CUDA + RTX), the copy cost
dominates inference time for anything larger than a few kilobytes.

---

## Solution: VMM-Mapped Shared Memory Region

The `vmm_shmem` passive PD maintains a table of **8 shared memory regions**
(conceptually 256 MB each, backed by seL4 frame capabilities).  Each region
has a stable simulated physical address that both the agentOS VMM and the Linux
userspace can map simultaneously.

```
┌──────────────────────────────────────────────────────────────────┐
│                     agentOS (seL4 Microkit)                      │
│                                                                  │
│  WASM Slot 0          vmm_shmem PD (passive, prio 140)           │
│  ┌──────────┐         ┌──────────────────────────────────────┐   │
│  │  Agent   │──ppc──▶ │ region table [8]                     │   │
│  │  WASM    │◀──ret── │  [0] phys=0x40000000 MAPPED slot=0  │   │
│  └──────────┘         │  [1] phys=0x50000000 FREE           │   │
│        │              │  ...                                  │   │
│        │ mmap         │ 512 KB status ring (read by Linux)   │   │
│        ▼              └──────────────────────────────────────┘   │
│  [shared MR cap]               │ status ring MR                  │
│        │                       │                                  │
└────────│───────────────────────│──────────────────────────────────┘
         │                       │
         │   VMM (Sparky)        │ /dev/vmm_shmem or /dev/mem
         │                       ▼
         │              ┌──────────────────────────────────────┐
         └─────────────▶│   Linux userspace (PyTorch / CUDA)   │
                        │   mm = vmm_open(0x40000000, 64M)     │
                        │   t = torch.frombuffer(mm, ...)      │
                        │   t_gpu = t.cuda(non_blocking=True)  │
                        └──────────────────────────────────────┘
```

**Key insight:** the WASM slot writes into the shared region (which it has a
seL4 frame capability for), calls `OP_VMM_SYNC` to issue a full memory barrier,
then the Linux side reads the same physical frames via its own mapping — **zero
copies through the kernel**.

---

## Region Lifecycle

```
  FREE
   │
   │  OP_VMM_MAP (caller provides size + WASM slot_id)
   ▼
  MAPPED  ◀────────────────────────────────────┐
   │                                           │
   │  OP_VMM_SYNC (WASM done writing)          │
   ▼                                           │
  SYNCING  ── memory_barrier() ──▶  MAPPED ───┘
   │
   │  OP_VMM_UNMAP
   ▼
  FREE
```

---

## API Reference

All calls are seL4 protected procedure calls (ppc) to the `vmm_shmem` PD.
Message registers are 64-bit values.

### OP_VMM_MAP (0x90)

Allocate the next free region.

| Register | Direction | Value |
|----------|-----------|-------|
| mr0      | in        | `0x90` |
| mr1      | in        | requested size (bytes) |
| mr2      | in        | caller's WASM slot ID (0–7) |
| mr0      | out       | `VMM_OK` (0) or error code |
| mr1      | out       | assigned region index (0–7) |
| mr2      | out       | `phys_addr[31:0]` |
| mr3      | out       | `phys_addr[63:32]` |

**Error codes:** `VMM_ERR_FULL` (1) — all 8 slots occupied; `VMM_ERR_INVAL` (2) — size is 0.

### OP_VMM_UNMAP (0x91)

Release a region.

| Register | Direction | Value |
|----------|-----------|-------|
| mr0      | in        | `0x91` |
| mr1      | in        | region index |
| mr0      | out       | `VMM_OK` or `VMM_ERR_INVAL` / `VMM_ERR_STATE` |

### OP_VMM_SYNC (0x92)

Issue a full memory barrier and transition `MAPPED → SYNCING → MAPPED`.
Call this after the WASM slot finishes writing so the Linux side can safely read.

| Register | Direction | Value |
|----------|-----------|-------|
| mr0      | in        | `0x92` |
| mr1      | in        | region index |
| mr0      | out       | `VMM_OK` or error |

### OP_VMM_STATUS (0x93)

Refresh the 512 KB status ring.  The ring is also updated automatically on
every MAP/UNMAP/SYNC, so polling is not normally required.

| Register | Direction | Value |
|----------|-----------|-------|
| mr0      | in        | `0x93` |
| mr0      | out       | `VMM_OK` |
| mr1      | out       | region count (always 8) |

---

## Status Ring Format

The ring is a flat `vmm_status_ring_t` struct at the base of the 512 KB
shared MR (see `vmm_shmem.h`):

```c
typedef struct {
    uint32_t magic;          // 0x564D4D53 ('VMMS')
    uint32_t version;        // 1
    uint32_t region_count;   // 8
    uint32_t seq;            // monotonic update counter
    vmm_region_t regions[8]; // packed, 24 bytes each
} vmm_status_ring_t;
```

The Linux side polls `seq` to detect updates without needing an IPC call.

---

## Usage Example — PyTorch Zero-Copy

```python
# tools/vmm_shmem_client.py (see file for full implementation)
from vmm_shmem_client import vmm_open, vmm_open_ring
import torch

# 1. WASM agent has already called OP_VMM_MAP via seL4 ppc.
#    Root task has minted a read capability for the ring MR to the VMM.
RING_PHYS = 0x3C000000   # configured at boot by root task
MAP_PHYS  = 0x40000000   # returned by OP_VMM_MAP for slot 0
MAP_SIZE  = 64 * 1024 * 1024  # 64 MB

# 2. Check region is MAPPED (WASM has finished writing, SYNC issued)
ring = vmm_open_ring(RING_PHYS)
slot0 = ring["regions"][0]
assert slot0["state"] == "MAPPED", f"Region not ready: {slot0['state']}"

# 3. Map the physical region into Linux userspace — zero copy
mm = vmm_open(MAP_PHYS, MAP_SIZE)

# 4. Wrap as PyTorch tensor — still zero copy (shares the mmap buffer)
tensor_cpu = torch.frombuffer(mm, dtype=torch.float32,
                               count=MAP_SIZE // 4)

# 5. Push to GPU for CUDA compute — one DMA transfer, no CPU-side copy
tensor_gpu = tensor_cpu.cuda(non_blocking=True)
torch.cuda.synchronize()

# 6. Run CUDA kernel on tensor_gpu ...
output_gpu = my_model(tensor_gpu)

# 7. Write result back to shared region so WASM slot can read it
tensor_cpu.copy_(output_gpu.cpu())

# 8. WASM slot calls OP_VMM_SYNC to observe the updated data
mm.close()
```

---

## Build Notes

The `vmm_shmem` PD targets `riscv64-unknown-elf` with the seL4 Microkit SDK.
It has no external dependencies beyond `microkit.h`.

```sh
# Cross-compile (from seL4 Microkit build environment)
riscv64-unknown-elf-gcc \
    -march=rv64imac -mabi=lp64 \
    -ffreestanding -nostdlib \
    -I path/to/microkit/include \
    -O2 -Wall -Wextra \
    -c kernel/agentos-root-task/src/vmm_shmem.c \
    -o vmm_shmem.o
```

On ARM64 (Sparky), replace the RISC-V flags with:
```sh
aarch64-none-elf-gcc -march=armv8-a -mabi=aapcs ...
```

The memory barrier in `handle_sync()` automatically selects `fence rw,rw`
(RISC-V) or `dsb sy` (ARM64) via `#ifdef __riscv` / `#elif defined(__aarch64__)`.

---

## Security Notes

- The Linux-side `/dev/mem` approach requires `CAP_SYS_RAWIO`.  For production,
  the root task should mint a narrowly-scoped capability and expose it via a
  thin character device driver (`/dev/vmm_shmem`) that enforces the region table.
- The seL4 capability model guarantees that the WASM slot can only write to
  frames it has been explicitly granted — it cannot reach outside its region.
- `OP_VMM_SYNC` acts as the ordering point: the Linux side must not read until
  the WASM slot has issued SYNC (observed via `seq` increment in the ring).
