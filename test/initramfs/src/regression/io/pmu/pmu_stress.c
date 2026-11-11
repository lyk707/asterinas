// SPDX-License-Identifier: MPL-2.0

#include "pmu_common.h"
#include <pthread.h>
#include <sched.h>
#include <signal.h>
#include <stdatomic.h>

struct worker {
	int fd;
	uint64_t id;
};
static atomic_int done, failed;
static atomic_int targets_ready, targets_go, targets_done;

struct target {
	pid_t tid;
	long loops;
};

static void *target_thread(void *argument)
{
	struct target *target = argument;
	target->tid = (pid_t)syscall(SYS_gettid);
	atomic_fetch_add(&targets_ready, 1);
	while (atomic_load(&targets_go) == 0)
		sched_yield();
	for (long i = 0; i < target->loops; ++i)
		pmu_burn(2000);
	atomic_fetch_add(&targets_done, 1);
	return NULL;
}

/*
 * Two targets with a 100x workload gap, each monitored by its own event. The
 * counts must stay apart: a shared counter, a lost migration, or an event bound
 * to the wrong task would collapse them into one value.
 */
static int counts_are_isolated(void)
{
	struct target targets[2] = { { .loops = 200 }, { .loops = 2 } };
	pthread_t threads[2];
	uint64_t ids[2] = { 0, 0 };
	int fds[2] = { -1, -1 };
	int failed = 0;

	atomic_store(&targets_ready, 0);
	atomic_store(&targets_go, 0);
	atomic_store(&targets_done, 0);
	for (int i = 0; i < 2; ++i) {
		int error = pthread_create(&threads[i], NULL, target_thread,
					   &targets[i]);
		if (error) {
			fprintf(stderr, "pthread_create: %s\n",
				strerror(error));
			return 1;
		}
	}
	while (atomic_load(&targets_ready) < 2)
		sched_yield();
	for (int i = 0; i < 2; ++i) {
		fds[i] = pmu_open(PERF_COUNT_HW_CPU_CYCLES, 'u', targets[i].tid,
				  0);
		if (fds[i] < 0)
			return pmu_failure("per-target event");
		if (ioctl(fds[i], PERF_EVENT_IOC_ID, &ids[i]) < 0)
			return pmu_failure("per-target event ID");
		if (pmu_control(fds[i], PERF_EVENT_IOC_ENABLE) < 0)
			return pmu_failure("enable per-target event");
	}
	atomic_store(&targets_go, 1);
	while (atomic_load(&targets_done) < 2)
		pmu_burn(1000);
	for (int i = 0; i < 2; ++i)
		pthread_join(threads[i], NULL);

	struct pmu_sample heavy, light;
	if (pmu_read(fds[0], &heavy) < 0 || pmu_read(fds[1], &light) < 0) {
		perror("read");
		failed = 1;
	} else if (heavy.count == 0 || heavy.count <= light.count) {
		fprintf(stderr,
			"FAIL: per-target counts leaked (heavy=%" PRIu64
			", light=%" PRIu64 ")\n",
			heavy.count, light.count);
		failed = 1;
	} else if (ids[0] == ids[1]) {
		fprintf(stderr, "FAIL: distinct events share one ID\n");
		failed = 1;
	}
	for (int i = 0; i < 2; ++i) {
		if (fds[i] >= 0)
			close(fds[i]);
	}
	return failed;
}

static void *control_loop(void *argument)
{
	struct worker *worker = argument;
	for (int i = 0; i < 250; ++i) {
		struct pmu_sample sample;
		if (pmu_control(worker->fd, PERF_EVENT_IOC_DISABLE) < 0 ||
		    pmu_control(worker->fd, PERF_EVENT_IOC_RESET) < 0 ||
		    pmu_control(worker->fd, PERF_EVENT_IOC_ENABLE) < 0 ||
		    pmu_read(worker->fd, &sample) < 0 ||
		    sample.id != worker->id ||
		    sample.running_ns > sample.enabled_ns) {
			atomic_store(&failed, 1);
			break;
		}
	}
	close(worker->fd);
	atomic_fetch_add(&done, 1);
	return NULL;
}

int main(void)
{
	alarm(90);
	int fd = pmu_open(0, 'u', 0, 0);
	if (fd < 0)
		return pmu_failure("PMU preflight");
	uint64_t id;
	if (ioctl(fd, PERF_EVENT_IOC_ID, &id) < 0)
		return pmu_failure("event ID");
	struct worker workers[2];
	pthread_t threads[2];
	for (int i = 0; i < 2; ++i) {
		workers[i] = (struct worker){ .fd = dup(fd), .id = id };
		if (workers[i].fd < 0) {
			perror("dup");
			return 1;
		}
		int error = pthread_create(&threads[i], NULL, control_loop,
					   &workers[i]);
		if (error) {
			fprintf(stderr, "pthread_create: %s\n",
				strerror(error));
			return 1;
		}
	}
	while (atomic_load(&done) < 2)
		pmu_burn(10000);
	for (int i = 0; i < 2; ++i)
		pthread_join(threads[i], NULL);
	close(fd);
	// Final-reference cleanup must make the event slot reusable each time.
	for (int i = 0; i < 250; ++i) {
		fd = pmu_open(0, 'u', 0, 0);
		if (fd < 0 || pmu_control(fd, PERF_EVENT_IOC_ENABLE) < 0) {
			atomic_store(&failed, 1);
			break;
		}
		pmu_burn(1000);
		close(fd); // Deliberately close while enabled.
	}
	if (atomic_load(&failed)) {
		fputs("FAIL: concurrent controls or slot cleanup\n", stderr);
		return 1;
	}
	if (counts_are_isolated())
		return 1;
	puts("PASS: concurrent dup/ioctl/read, enabled-event close/reopen and per-target isolation");
	return 0;
}
