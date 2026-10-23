// SPDX-License-Identifier: MPL-2.0

#include "pmu_common.h"
#include <pthread.h>
#include <signal.h>
#include <stdatomic.h>

struct worker {
	int fd;
	uint64_t id;
};
static atomic_int done, failed;

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
	puts("PASS: concurrent dup/ioctl/read and repeated enabled-event close/reopen");
	return 0;
}
