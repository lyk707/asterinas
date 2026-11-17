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
static pthread_mutex_t targets_lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t targets_changed = PTHREAD_COND_INITIALIZER;
static int targets_ready;

struct target {
	pid_t tid;
	pthread_cond_t ready_to_run;
	int go;
};

static void *target_thread(void *argument)
{
	struct target *target = argument;
	pthread_mutex_lock(&targets_lock);
	target->tid = (pid_t)syscall(SYS_gettid);
	++targets_ready;
	pthread_cond_signal(&targets_changed);
	while (!target->go)
		pthread_cond_wait(&target->ready_to_run, &targets_lock);
	pthread_mutex_unlock(&targets_lock);
	pmu_burn(400000);
	return NULL;
}

static void start_target(struct target *target)
{
	target->go = 1;
	pthread_cond_signal(&target->ready_to_run);
}

/* Only the running thread's count may advance while its peer is blocked. */
static int counts_are_isolated(int first)
{
	struct target targets[2] = {
		{ .ready_to_run = PTHREAD_COND_INITIALIZER },
		{ .ready_to_run = PTHREAD_COND_INITIALIZER },
	};
	pthread_t threads[2];
	uint64_t ids[2] = { 0, 0 };
	int fds[2] = { -1, -1 };
	int failed = 0;
	int started = 0;
	int blocked = 1 - first;
	struct pmu_sample before, after, active;

	targets_ready = 0;
	for (int i = 0; i < 2; ++i) {
		int error = pthread_create(&threads[i], NULL, target_thread,
					   &targets[i]);
		if (error) {
			fprintf(stderr, "pthread_create: %s\n",
				strerror(error));
			failed = 1;
			break;
		}
		++started;
	}
	pthread_mutex_lock(&targets_lock);
	while (targets_ready < started)
		pthread_cond_wait(&targets_changed, &targets_lock);
	// Readiness only confirms that the target published its TID.
	// Do not arm it until it has finished entering the futex wait.
	if (!failed &&
	    pmu_wait_until_sleeping(getpid(), targets[blocked].tid) < 0) {
		perror("wait for blocked target");
		failed = 1;
	}
	for (int i = 0; !failed && i < 2; ++i) {
		fds[i] = pmu_open(PERF_COUNT_HW_CPU_CYCLES, 'u', targets[i].tid,
				  0);
		if (fds[i] < 0 ||
		    ioctl(fds[i], PERF_EVENT_IOC_ID, &ids[i]) < 0 ||
		    pmu_control(fds[i], PERF_EVENT_IOC_ENABLE) < 0)
			failed = pmu_failure("arm per-target event");
	}
	if (!failed && pmu_read(fds[blocked], &before) < 0)
		failed = pmu_failure("read blocked target before workload");

	if (failed) {
		for (int i = 0; i < started; ++i)
			start_target(&targets[i]);
		pthread_mutex_unlock(&targets_lock);
		for (int i = 0; i < started; ++i)
			pthread_join(threads[i], NULL);
		goto close_events;
	}

	start_target(&targets[first]);
	pthread_mutex_unlock(&targets_lock);
	pthread_join(threads[first], NULL);
	if (pmu_read(fds[blocked], &after) < 0 ||
	    pmu_read(fds[first], &active) < 0) {
		failed = pmu_failure("read per-target counters");
	} else if (after.count != before.count || active.count == 0 ||
		   active.running_ns == 0 || ids[0] == ids[1]) {
		fprintf(stderr,
			"FAIL: per-target isolation (blocked count %" PRIu64
			" -> %" PRIu64 ", active count %" PRIu64 ")\n",
			before.count, after.count, active.count);
		failed = 1;
	}

	pthread_mutex_lock(&targets_lock);
	start_target(&targets[blocked]);
	pthread_mutex_unlock(&targets_lock);
	pthread_join(threads[blocked], NULL);

close_events:
	for (int i = 0; i < 2; ++i) {
		if (fds[i] >= 0)
			close(fds[i]);
		pthread_cond_destroy(&targets[i].ready_to_run);
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
	if (counts_are_isolated(0) || counts_are_isolated(1))
		return 1;
	puts("PASS: concurrent dup/ioctl/read, enabled-event close/reopen and per-target isolation");
	return 0;
}
