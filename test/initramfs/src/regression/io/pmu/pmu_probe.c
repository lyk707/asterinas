// SPDX-License-Identifier: MPL-2.0

#include "pmu_common.h"
#ifdef __x86_64__
#include <cpuid.h>
#endif

int main(void)
{
#ifdef __x86_64__
	unsigned int a, b, c, d;
	char vendor[13] = { 0 };
	__cpuid(0, a, b, c, d);
	memcpy(vendor, &b, 4);
	memcpy(vendor + 4, &d, 4);
	memcpy(vendor + 8, &c, 4);
	printf("CPU vendor: %s\n", vendor);
	if (__get_cpuid(0xa, &a, &b, &c, &d))
		printf("CPUID.0A: version=%u fixed=%u width=%u\n", a & 255,
		       d & 31, (d >> 5) & 255);
#elif defined(__riscv)
	puts("RV64: check the per-hart [pmu] boot messages and the trusted SBI platform contract.");
#endif
	int fd = pmu_open(PERF_COUNT_HW_CPU_CYCLES, 'u', 0, 0);
	if (fd < 0)
		return pmu_failure("perf_event_open(cycles:u)");
	if (pmu_control(fd, PERF_EVENT_IOC_ENABLE) < 0) {
		int result = pmu_failure("enable");
		close(fd);
		return result;
	}
	pmu_burn(1000000);
	struct pmu_sample sample;
	if (pmu_control(fd, PERF_EVENT_IOC_DISABLE) < 0 ||
	    pmu_read(fd, &sample) < 0) {
		int result = pmu_failure("stop/read");
		close(fd);
		return result;
	}
	close(fd);
	printf("cycles:u=%" PRIu64 " enabled_ns=%" PRIu64 " running_ns=%" PRIu64
	       "\n",
	       sample.count, sample.enabled_ns, sample.running_ns);
	if (!sample.count || !sample.running_ns ||
	    sample.running_ns > sample.enabled_ns) {
		fputs("FAIL: no valid hardware measurement; zero is not a successful PMU probe.\n",
		      stderr);
		return 1;
	}
	puts("PASS: a nonzero hardware count was returned through the perf fd.");
	return 0;
}
