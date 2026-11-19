// SPDX-License-Identifier: MPL-2.0

#define _GNU_SOURCE
#include <assert.h>
#include <errno.h>
#include <fcntl.h>
#include <linux/perf_event.h>
#include <sched.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/ioctl.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

struct event_reading {
	uint64_t count;
	uint64_t enabled_ns;
	uint64_t running_ns;
	uint64_t id;
};

static int open_event(pid_t tid)
{
	struct perf_event_attr attr = {
		.type = PERF_TYPE_HARDWARE,
		.size = sizeof(attr),
		.config = PERF_COUNT_HW_INSTRUCTIONS,
		.disabled = 1,
		.exclude_kernel = 1,
		.read_format = PERF_FORMAT_TOTAL_TIME_ENABLED |
			       PERF_FORMAT_TOTAL_TIME_RUNNING | PERF_FORMAT_ID,
	};
	return syscall(SYS_perf_event_open, &attr, tid, -1, -1,
		       PERF_FLAG_FD_CLOEXEC);
}

static void pin_to_cpu(int cpu)
{
	cpu_set_t affinity;
	CPU_ZERO(&affinity);
	CPU_SET(cpu, &affinity);
	assert(sched_setaffinity(0, sizeof(affinity), &affinity) == 0);

	for (int attempt = 0; attempt < 1000; attempt++) {
		unsigned int current_cpu;
		assert(syscall(SYS_getcpu, &current_cpu, NULL, NULL) == 0);
		if (current_cpu == (unsigned int)cpu)
			return;
		usleep(1000);
	}
	fprintf(stderr, "the thread did not run on requested CPU %d\n", cpu);
	abort();
}

static void run_iterations(int iterations)
{
	volatile uint64_t value = 1;
	for (int i = 0; i < iterations; i++)
		value = value * 3 + 1;
}

static void run_workload(int ready_pipe, int stop_pipe)
{
	assert(fcntl(stop_pipe, F_SETFL, O_NONBLOCK) == 0);
	assert(write(ready_pipe, "r", 1) == 1);
	close(ready_pipe);

	for (;;) {
		run_iterations(4096);

		char byte;
		if (read(stop_pipe, &byte, 1) >= 0)
			_exit(0);
		assert(errno == EAGAIN || errno == EINTR);
	}
}

static struct event_reading read_event(int fd)
{
	struct event_reading reading;
	assert(read(fd, &reading, sizeof(reading)) == sizeof(reading));
	assert(reading.running_ns <= reading.enabled_ns);
	return reading;
}

static struct event_reading wait_for_increment(int fd, uint64_t previous)
{
	for (int attempt = 0; attempt < 1000; attempt++) {
		struct event_reading reading = read_event(fd);
		if (reading.count > previous)
			return reading;
		usleep(1000);
	}
	fputs("the remote counter did not advance\n", stderr);
	abort();
}

static void check_migration(int source_cpu, int destination_cpu)
{
	const int before_iterations = 2000000;
	const int after_iterations = 100000;
	int fd = open_event(0);
	assert(fd >= 0);
	assert(ioctl(fd, PERF_EVENT_IOC_ENABLE, 0) == 0);

	run_iterations(before_iterations);
	struct event_reading before = read_event(fd);
	assert(before.count >= (uint64_t)before_iterations);

	// The event stays enabled while the same thread blocks and wakes on the
	// destination CPU. pin_to_cpu verifies the actual CPU with getcpu.
	pin_to_cpu(destination_cpu);
	struct event_reading moved = read_event(fd);
	assert(moved.count >= before.count);
	// The affinity change must preserve the first segment exactly once.
	assert(moved.count - before.count < (uint64_t)before_iterations);
	assert(moved.running_ns >= before.running_ns);

	run_iterations(after_iterations);
	struct event_reading after = read_event(fd);
	assert(after.count >= moved.count);
	assert(after.count - moved.count >= (uint64_t)after_iterations);
	assert(after.running_ns >= moved.running_ns);
	assert(before.id != 0 && moved.id == before.id &&
	       after.id == before.id);
	assert(close(fd) == 0);

	printf("PASS: perf instructions across CPU %d -> %d "
	       "(before=%llu, moved=%llu, after=%llu)\n",
	       source_cpu, destination_cpu, (unsigned long long)before.count,
	       (unsigned long long)moved.count,
	       (unsigned long long)after.count);
}

int main(void)
{
	int require_migration = getenv("ASTERINAS_REQUIRE_PERF_MIGRATION") !=
				NULL;
	cpu_set_t affinity;
	assert(sched_getaffinity(0, sizeof(affinity), &affinity) == 0);
	int local_cpu = -1;
	int remote_cpu = -1;
	for (int cpu = 0; cpu < CPU_SETSIZE; cpu++) {
		if (!CPU_ISSET(cpu, &affinity))
			continue;
		if (local_cpu >= 0) {
			remote_cpu = cpu;
			break;
		}
		local_cpu = cpu;
	}
	if (remote_cpu == -1) {
		fprintf(stderr, "%s: perf migration requires two CPUs\n",
			require_migration ? "FAIL" : "SKIP");
		return require_migration ? EXIT_FAILURE : EXIT_SUCCESS;
	}

	int probe = open_event(0);
	if (probe == -1 &&
	    (errno == ENOSYS || errno == EOPNOTSUPP || errno == ENODEV ||
	     errno == ENOENT || errno == EPERM || errno == EACCES)) {
		fprintf(stderr, "%s: hardware perf counting is unavailable\n",
			require_migration ? "FAIL" : "SKIP");
		return require_migration ? EXIT_FAILURE : EXIT_SUCCESS;
	}
	assert(probe >= 0);
	close(probe);

	pin_to_cpu(local_cpu);
	int ready[2], stop[2];
	assert(pipe(ready) == 0);
	assert(pipe(stop) == 0);
	pid_t child = fork();
	assert(child >= 0);
	if (child == 0) {
		close(ready[0]);
		close(stop[1]);
		pin_to_cpu(remote_cpu);
		run_workload(ready[1], stop[0]);
	}
	close(ready[1]);
	close(stop[0]);
	char byte;
	assert(read(ready[0], &byte, 1) == 1);
	close(ready[0]);

	int fd = open_event(child);
	assert(fd >= 0);
	assert(fcntl(fd, F_GETFD) == FD_CLOEXEC);
	uint64_t id = 0;
	assert(ioctl(fd, PERF_EVENT_IOC_ID, &id) == 0);
	assert(id != 0 && read_event(fd).id == id);

	assert(ioctl(fd, PERF_EVENT_IOC_ENABLE, 0) == 0);
	struct event_reading first = wait_for_increment(fd, 0);
	struct event_reading second = wait_for_increment(fd, first.count);
	assert(second.running_ns >= first.running_ns);

	assert(ioctl(fd, PERF_EVENT_IOC_DISABLE, 0) == 0);
	struct event_reading stopped = read_event(fd);
	assert(stopped.count >= second.count);
	usleep(5000);
	assert(read_event(fd).count == stopped.count);

	assert(ioctl(fd, PERF_EVENT_IOC_RESET, 0) == 0);
	struct event_reading reset = read_event(fd);
	assert(reset.count == 0);
	assert(reset.enabled_ns == stopped.enabled_ns);
	assert(reset.running_ns == stopped.running_ns);

	assert(ioctl(fd, PERF_EVENT_IOC_ENABLE, 0) == 0);
	struct event_reading resumed = wait_for_increment(fd, 0);
	close(stop[1]);
	int status;
	assert(waitpid(child, &status, 0) == child);
	assert(WIFEXITED(status) && WEXITSTATUS(status) == 0);
	assert(read_event(fd).count >= resumed.count);
	close(fd);

	printf("PASS: live perf reads from CPU %d to CPU %d\n", local_cpu,
	       remote_cpu);
	check_migration(local_cpu, remote_cpu);
	return 0;
}
