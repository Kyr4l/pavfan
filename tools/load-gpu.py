#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Headless NVIDIA GPU load generator (CUDA driver API via ctypes, no nvcc needed).

Usage: load-gpu.py <seconds>

Runs a pure-ALU FMA loop on all SMs for the given duration. Used only to heat
the GPU while recording EC behaviour. Runs as a normal user.
"""
import ctypes
import sys
import time

PTX = rb"""
.version 6.0
.target sm_50
.address_size 64
.visible .entry burn(.param .u32 iters, .param .u64 out)
{
    .reg .u32 %i, %n;
    .reg .f32 %a, %b, %c;
    .reg .u64 %o;
    .reg .pred %p;
    ld.param.u32 %n, [iters];
    ld.param.u64 %o, [out];
    mov.f32 %a, 1.0001;
    mov.f32 %b, 0.9999;
    mov.f32 %c, 0.5;
    mov.u32 %i, 0;
L:
    fma.rn.f32 %c, %a, %c, %b;
    fma.rn.f32 %c, %b, %c, %a;
    fma.rn.f32 %c, %a, %c, %b;
    fma.rn.f32 %c, %b, %c, %a;
    add.u32 %i, %i, 1;
    setp.lt.u32 %p, %i, %n;
    @%p bra L;
    st.global.f32 [%o], %c;
    ret;
}
"""


def check(rc, what):
    if rc != 0:
        sys.exit(f"CUDA error {rc} in {what}")


def main():
    secs = float(sys.argv[1])
    cuda = ctypes.CDLL("libcuda.so.1")
    check(cuda.cuInit(0), "cuInit")
    dev = ctypes.c_int()
    check(cuda.cuDeviceGet(ctypes.byref(dev), 0), "cuDeviceGet")
    ctx = ctypes.c_void_p()
    check(cuda.cuCtxCreate_v2(ctypes.byref(ctx), 0, dev), "cuCtxCreate")
    mod = ctypes.c_void_p()
    check(cuda.cuModuleLoadData(ctypes.byref(mod), ctypes.c_char_p(PTX)), "cuModuleLoadData")
    fn = ctypes.c_void_p()
    check(cuda.cuModuleGetFunction(ctypes.byref(fn), mod, b"burn"), "cuModuleGetFunction")
    out = ctypes.c_uint64()
    check(cuda.cuMemAlloc_v2(ctypes.byref(out), 4), "cuMemAlloc")

    iters = ctypes.c_uint32(20_000_000)  # ~ short kernels so we can stop promptly
    args = (ctypes.c_void_p * 2)(ctypes.cast(ctypes.byref(iters), ctypes.c_void_p),
                                 ctypes.cast(ctypes.byref(out), ctypes.c_void_p))
    end = time.monotonic() + secs
    while time.monotonic() < end:
        check(cuda.cuLaunchKernel(fn, 64, 1, 1, 1024, 1, 1, 0, None, args, None), "launch")
        check(cuda.cuCtxSynchronize(), "sync")
    cuda.cuCtxDestroy_v2(ctx)


if __name__ == "__main__":
    main()
