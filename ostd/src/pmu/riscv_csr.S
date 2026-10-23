// SPDX-License-Identifier: MPL-2.0

    .section .text
    .option push
    .option norvc
    .option norelax
    .balign 4
    .global __pmu_read_counter_csr
    .type __pmu_read_counter_csr, @function
__pmu_read_counter_csr:
    la t0, __pmu_csr_sites
    slli a0, a0, 3
    add t0, t0, a0
    jr t0

    .global __pmu_csr_sites
__pmu_csr_sites:
    .irp csr, 0xc00, 0xc01, 0xc02, 0xc03, 0xc04, 0xc05, 0xc06, 0xc07, 0xc08, 0xc09, 0xc0a, 0xc0b, 0xc0c, 0xc0d, 0xc0e, 0xc0f, 0xc10, 0xc11, 0xc12, 0xc13, 0xc14, 0xc15, 0xc16, 0xc17, 0xc18, 0xc19, 0xc1a, 0xc1b, 0xc1c, 0xc1d, 0xc1e, 0xc1f
    csrr t0, \csr
    j .Lpmu_csr_success
    .endr
    .global __pmu_csr_sites_end
__pmu_csr_sites_end:
.Lpmu_csr_success:
    sd t0, 0(a1)
    li a0, 0
    ret

    .global __pmu_csr_failed
__pmu_csr_failed:
    li a0, 1
    ret
    .size __pmu_read_counter_csr, .-__pmu_read_counter_csr
    .option pop
