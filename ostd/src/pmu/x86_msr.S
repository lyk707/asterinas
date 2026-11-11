/* SPDX-License-Identifier: MPL-2.0 */

.text
.code64

.global __pmu_read_msr
.balign 16
.type __pmu_read_msr, @function
__pmu_read_msr:
    mov ecx, edi
.global __pmu_rdmsr_site
__pmu_rdmsr_site:
    rdmsr
    shl rdx, 32
    or rax, rdx
    mov [rsi], rax
    xor eax, eax
    ret
.global __pmu_rdmsr_failed
__pmu_rdmsr_failed:
    mov eax, 1
    ret
.size __pmu_read_msr, .-__pmu_read_msr

.global __pmu_write_msr
.balign 16
.type __pmu_write_msr, @function
__pmu_write_msr:
    mov ecx, edi
    mov rax, rsi
    mov rdx, rsi
    shr rdx, 32
.global __pmu_wrmsr_site
__pmu_wrmsr_site:
    wrmsr
    xor eax, eax
    ret
.global __pmu_wrmsr_failed
__pmu_wrmsr_failed:
    mov eax, 1
    ret
.size __pmu_write_msr, .-__pmu_write_msr

