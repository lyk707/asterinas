// SPDX-License-Identifier: MPL-2.0

#ifndef ASTERINAS_PMU_COMMON_H
#define ASTERINAS_PMU_COMMON_H

#define _GNU_SOURCE
#include <errno.h>
#include <inttypes.h>
#include <linux/perf_event.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/syscall.h>
#include <unistd.h>

_Static_assert(PERF_ATTR_SIZE_VER0 == 64, "perf v0 ABI size");
_Static_assert(offsetof(struct perf_event_attr, config) == 8,
	       "perf config ABI");
_Static_assert(offsetof(struct perf_event_attr, read_format) == 32,
	       "perf read ABI");
_Static_assert(offsetof(struct perf_event_attr, wakeup_events) == 48,
	       "perf flags ABI");

struct pmu_sample {
	uint64_t count;
	uint64_t enabled_ns;
	uint64_t running_ns;
	uint64_t id;
};

static inline struct perf_event_attr pmu_attr(uint64_t event, char scope)
{
	struct perf_event_attr attr;
	memset(&attr, 0, sizeof(attr));
	attr.type = PERF_TYPE_HARDWARE;
	attr.size = PERF_ATTR_SIZE_VER0;
	attr.config = event;
	attr.disabled = 1;
	attr.exclude_kernel = scope == 'u';
	attr.exclude_user = scope == 'k';
	attr.exclude_hv = 1;
	attr.read_format = PERF_FORMAT_TOTAL_TIME_ENABLED |
			   PERF_FORMAT_TOTAL_TIME_RUNNING | PERF_FORMAT_ID;
	return attr;
}

static inline int pmu_open_attr(struct perf_event_attr *attr, pid_t tid,
				int cpu, int group, unsigned long flags)
{
	return (int)syscall(__NR_perf_event_open, attr, tid, cpu, group, flags);
}

static inline int pmu_open(uint64_t event, char scope, pid_t tid, int on_exec)
{
	struct perf_event_attr attr = pmu_attr(event, scope);
	attr.enable_on_exec = !!on_exec;
	return pmu_open_attr(&attr, tid, -1, -1, PERF_FLAG_FD_CLOEXEC);
}

static inline int pmu_read(int fd, struct pmu_sample *sample)
{
	ssize_t size;
	do {
		size = read(fd, sample, sizeof(*sample));
	} while (size < 0 && errno == EINTR);
	if (size < 0)
		return -1;
	if (size != (ssize_t)sizeof(*sample)) {
		errno = EIO;
		return -1;
	}
	return 0;
}

static inline int pmu_control(int fd, unsigned long op)
{
	int result;
	do {
		result = ioctl(fd, op, 0);
	} while (result < 0 && errno == EINTR);
	return result;
}

static inline int pmu_failure(const char *operation)
{
	int error = errno;
	fprintf(stderr, "%s: %s (errno=%d)\n", operation, strerror(error),
		error);
	if (error == EPERM || error == EACCES || error == EOPNOTSUPP ||
	    error == ENOSYS) {
		fprintf(stderr,
			"SKIP: check pmu feature, CAP_PERFMON/CAP_SYS_ADMIN and guest PMU exposure.\n");
		return 77;
	}
	return 1;
}

static inline uint64_t pmu_burn(uint64_t iterations)
{
	// Data dependence prevents replacing the workload with a constant result.
	// Volatile prevents removal when a caller only needs the execution itself.
	volatile uint64_t value = UINT64_C(0xabcdef123456789);
	for (uint64_t i = 0; i < iterations; ++i)
		value = value * UINT64_C(6364136223846793005) +
			(i ^ (value >> 13));
	return value;
}

static inline int pmu_byte(int fd, char *value, int writing)
{
	ssize_t n;
	do {
		n = writing ? write(fd, value, 1) : read(fd, value, 1);
	} while (n < 0 && errno == EINTR);
	if (n == 1)
		return 0;
	if (n == 0)
		errno = EPIPE;
	return -1;
}

#endif
