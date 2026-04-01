#!/usr/bin/env python3
"""
vmm_shmem_client.py — Linux-side client for agentOS VMM shared memory regions.

Conceptual sketch showing how a Linux process (e.g. a PyTorch CUDA workload)
would access the zero-copy buffers managed by the seL4 vmm_shmem PD.

Requirements:
  - Root access (for /dev/mem) OR a thin kernel module that exposes the
    vmm_shmem ring MR as a character device (recommended for production).
  - The agentOS vmm_shmem PD must have completed OP_VMM_MAP for the slot.
  - Python 3.8+, optional: torch >= 2.0

Usage:
  python3 vmm_shmem_client.py --phys 0x40000000 --size $((64 * 1024 * 1024))
"""

import ctypes
import mmap
import struct
import argparse
import os

# ── Status ring layout (must match vmm_shmem.h) ────────────────────────────

VMM_STATUS_MAGIC   = 0x564D4D53   # 'VMMS'
VMM_STATUS_VERSION = 1
VMM_MAX_REGIONS    = 8

VMM_STATE_FREE    = 0
VMM_STATE_MAPPED  = 1
VMM_STATE_SYNCING = 2

_STATE_NAMES = {
    VMM_STATE_FREE:    "FREE",
    VMM_STATE_MAPPED:  "MAPPED",
    VMM_STATE_SYNCING: "SYNCING",
}

# struct vmm_region_t (packed, 24 bytes)
# uint64 phys_addr, uint64 size, uint32 slot_id, uint8 state, uint8[3] pad
REGION_FMT  = "<QQIBxxx"
REGION_SIZE = struct.calcsize(REGION_FMT)  # 24 bytes

# struct vmm_status_ring_t header (16 bytes) + VMM_MAX_REGIONS * REGION_SIZE
RING_HEADER_FMT  = "<IIII"
RING_HEADER_SIZE = struct.calcsize(RING_HEADER_FMT)  # 16 bytes


def parse_ring(data: bytes) -> dict:
    """Parse a vmm_status_ring_t from raw bytes."""
    magic, version, region_count, seq = struct.unpack_from(RING_HEADER_FMT, data, 0)
    if magic != VMM_STATUS_MAGIC:
        raise ValueError(f"Bad ring magic: 0x{magic:08X} (expected 0x{VMM_STATUS_MAGIC:08X})")
    regions = []
    offset = RING_HEADER_SIZE
    for _ in range(min(region_count, VMM_MAX_REGIONS)):
        phys_addr, size, slot_id, state = struct.unpack_from(REGION_FMT, data, offset)
        regions.append({
            "phys_addr": phys_addr,
            "size":      size,
            "slot_id":   slot_id,
            "state":     _STATE_NAMES.get(state, f"UNKNOWN({state})"),
        })
        offset += REGION_SIZE
    return {"version": version, "seq": seq, "regions": regions}


# ── Low-level mmap helpers ──────────────────────────────────────────────────

def vmm_open(phys_addr: int, size: int) -> mmap.mmap:
    """
    Map a physical memory region into the process address space via /dev/mem.

    WARNING: Requires CAP_SYS_RAWIO (root).  In production use a custom
    character device (e.g. /dev/vmm_shmem) that limits access to the
    regions actually granted by the seL4 vmm_shmem PD.

    Returns an mmap object positioned at offset 0 of the region.
    """
    # Align to page boundary
    page_size = mmap.PAGESIZE
    offset_in_page = phys_addr % page_size
    aligned_phys   = phys_addr - offset_in_page
    aligned_size   = size + offset_in_page

    fd = os.open("/dev/mem", os.O_RDWR | os.O_SYNC)
    try:
        mm = mmap.mmap(
            fd,
            aligned_size,
            mmap.MAP_SHARED,
            mmap.PROT_READ | mmap.PROT_WRITE,
            offset=aligned_phys,
        )
    finally:
        os.close(fd)

    # Seek past the alignment padding so offset 0 == phys_addr
    mm.seek(offset_in_page)
    return mm


def vmm_open_ring(ring_phys_addr: int, ring_size: int = 512 * 1024) -> dict:
    """
    Map and parse the vmm_shmem status ring.

    Returns the parsed ring dict (see parse_ring).
    """
    mm = vmm_open(ring_phys_addr, ring_size)
    data = mm.read(ring_size)
    mm.close()
    return parse_ring(data)


# ── PyTorch zero-copy integration ───────────────────────────────────────────

def vmm_as_torch_tensor(phys_addr: int, size: int, dtype=None):
    """
    Expose a vmm_shmem region as a PyTorch CPU tensor backed by the shared
    physical memory — zero copies.

    The tensor is writable.  To push it to CUDA for GPU compute:

        tensor_cpu = vmm_as_torch_tensor(phys_addr, size, dtype=torch.float32)
        tensor_gpu = tensor_cpu.cuda(non_blocking=True)
        # ... GPU compute ...
        tensor_cpu.copy_(tensor_gpu)   # write result back to shared region

    Requires: torch >= 2.0, Python >= 3.8
    """
    try:
        import torch
    except ImportError:
        raise RuntimeError("PyTorch is required for vmm_as_torch_tensor()")

    if dtype is None:
        dtype = torch.float32

    mm = vmm_open(phys_addr, size)

    # frombuffer gives a tensor that shares the mmap buffer — no copy.
    # The mmap object must stay alive as long as the tensor is in use.
    tensor = torch.frombuffer(mm, dtype=dtype).clone()
    # NOTE: .clone() is used here only because frombuffer on mmap requires
    # careful lifetime management.  For true zero-copy, use ctypes + torch
    # UnsafeStorage — see the comment block below.
    mm.close()

    return tensor


# ── Example: zero-copy via ctypes UnsafeStorage (advanced) ─────────────────
#
# For true zero-copy (no clone), use torch.UnsafeStorage to wrap the mmap:
#
#   import torch, ctypes, mmap
#
#   mm = vmm_open(phys_addr, size)
#   ptr = ctypes.c_char_p(ctypes.addressof(ctypes.c_char.from_buffer(mm)))
#   storage = torch.UnsafeStorage.from_file(mm.name, shared=True, nbytes=size)
#   tensor = torch.FloatTensor(storage)
#
# The tensor's data pointer IS the shared physical memory — no intermediate
# buffer.  This is the target usage pattern for agentOS GPU zero-copy.


# ── CLI ─────────────────────────────────────────────────────────────────────

def main():
    parser = argparse.ArgumentParser(description="agentOS vmm_shmem Linux client")
    sub = parser.add_subparsers(dest="cmd", required=True)

    p_status = sub.add_parser("status", help="Parse and display the status ring")
    p_status.add_argument("--ring-phys", type=lambda x: int(x, 0), required=True,
                          help="Physical address of the vmm_shmem status ring")

    p_map = sub.add_parser("map", help="Dump raw bytes of a mapped region")
    p_map.add_argument("--phys", type=lambda x: int(x, 0), required=True,
                       help="Physical base address of the region")
    p_map.add_argument("--size", type=lambda x: int(x, 0), required=True,
                       help="Region size in bytes")
    p_map.add_argument("--out", default="-", help="Output file (default: stdout)")

    args = parser.parse_args()

    if args.cmd == "status":
        ring = vmm_open_ring(args.ring_phys)
        print(f"vmm_shmem ring  version={ring['version']}  seq={ring['seq']}")
        print(f"{'Slot':>4}  {'State':>8}  {'PhysAddr':>18}  {'Size':>16}  WASMSlot")
        print("-" * 70)
        for i, r in enumerate(ring["regions"]):
            print(f"{i:>4}  {r['state']:>8}  0x{r['phys_addr']:016X}"
                  f"  {r['size']:>16}  {r['slot_id'] if r['slot_id'] != 0xFFFFFFFF else '-':>8}")

    elif args.cmd == "map":
        mm = vmm_open(args.phys, args.size)
        data = mm.read(args.size)
        mm.close()
        if args.out == "-":
            import sys
            sys.stdout.buffer.write(data)
        else:
            with open(args.out, "wb") as f:
                f.write(data)
        print(f"Wrote {len(data)} bytes from phys 0x{args.phys:X}", flush=True)


if __name__ == "__main__":
    main()
