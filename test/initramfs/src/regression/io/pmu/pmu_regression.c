// SPDX-License-Identifier: MPL-2.0

#include "pmu_common.h"
#include <fcntl.h>
#include <limits.h>
#include <linux/capability.h>
#include <sched.h>
#include <signal.h>
#include <sys/mman.h>
#include <sys/wait.h>

#define CHECK(expr)                                                         \
	do {                                                                \
		if (!(expr)) {                                              \
			fprintf(stderr, "FAIL line %d: %s (errno=%d %s)\n", \
				__LINE__, #expr, errno, strerror(errno));   \
			exit(1);                                            \
		}                                                           \
	} while (0)
#define EXPECT_ERR(expr, expected)                          \
	do {                                                \
		errno = 0;                                  \
		long actual = (long)(expr);                 \
		CHECK(actual == -1 && errno == (expected)); \
	} while (0)

static void abi_layout(void)
{
	struct perf_event_attr attr = pmu_attr(PERF_COUNT_HW_CPU_CYCLES, 'u');
	uint64_t flags;
	memcpy(&flags, (char *)&attr + 40, sizeof(flags));
	CHECK(flags == 0x61);
	CHECK(sizeof(struct pmu_sample) == 32);
	CHECK(PERF_EVENT_IOC_ENABLE == 0x2400);
	CHECK(PERF_EVENT_IOC_DISABLE == 0x2401);
	CHECK(PERF_EVENT_IOC_RESET == 0x2403);
	CHECK(PERF_EVENT_IOC_ID == 0x80082407UL);
	puts("PASS: C ABI size, offsets, bitfield flags and ioctl encodings");
}

static void abi_errors(void)
{
	struct perf_event_attr attr = pmu_attr(0, 'u');
	EXPECT_ERR(pmu_open_attr(NULL, 0, -1, -1, 0), EFAULT);
	EXPECT_ERR(pmu_open_attr(&attr, -2, -1, -1, 0), EINVAL);
	EXPECT_ERR(pmu_open_attr(&attr, 0, -2, -1, 0), EINVAL);
	EXPECT_ERR(pmu_open_attr(&attr, 0, INT_MAX, -1, 0), EINVAL);
	EXPECT_ERR(pmu_open_attr(&attr, -1, -1, -1, 0), EINVAL);
	EXPECT_ERR(pmu_open_attr(&attr, -1, 0, -1, 0), EOPNOTSUPP);
	EXPECT_ERR(pmu_open_attr(&attr, 0, -1, 0, 0), EOPNOTSUPP);
	EXPECT_ERR(pmu_open_attr(&attr, 0, -1, -1, 1), EINVAL);
	EXPECT_ERR(pmu_open_attr(&attr, INT_MAX, -1, -1, 0), ESRCH);
	attr.config = 2;
	EXPECT_ERR(pmu_open_attr(&attr, 0, -1, -1, 0), EOPNOTSUPP);
	attr = pmu_attr(0, 'u');
	attr.inherit = 1;
	EXPECT_ERR(pmu_open_attr(&attr, 0, -1, -1, 0), EOPNOTSUPP);
	attr = pmu_attr(0, 'u');
	attr.inherit = 1;
	attr.enable_on_exec = 1;
	EXPECT_ERR(pmu_open_attr(&attr, 0, -1, -1, 0), EOPNOTSUPP);
	attr = pmu_attr(0, 'u');
	attr.exclude_host = 1;
	EXPECT_ERR(pmu_open_attr(&attr, 0, -1, -1, 0), EOPNOTSUPP);
	attr = pmu_attr(0, 'u');
	attr.pinned = 1;
	EXPECT_ERR(pmu_open_attr(&attr, 0, -1, -1, 0), EOPNOTSUPP);
	attr = pmu_attr(0, 'u');
	attr.sample_period = 1000;
	EXPECT_ERR(pmu_open_attr(&attr, 0, -1, -1, 0), EOPNOTSUPP);
	attr = pmu_attr(0, 'u');
	attr.read_format = PERF_FORMAT_GROUP;
	EXPECT_ERR(pmu_open_attr(&attr, 0, -1, -1, 0), EOPNOTSUPP);
	attr = pmu_attr(0, 'u');
	attr.exclusive = 1;
	EXPECT_ERR(pmu_open_attr(&attr, 0, -1, -1, 0), EOPNOTSUPP);
	attr = pmu_attr(0, 'u');
	attr.exclude_user = 1;
	EXPECT_ERR(pmu_open_attr(&attr, 0, -1, -1, 0), EINVAL);
	attr = pmu_attr(0, 'u');
	attr.size = 63;
	EXPECT_ERR(pmu_open_attr(&attr, 0, -1, -1, 0), E2BIG);
	CHECK(attr.size == 64);

	// Standard Linux `perf stat` compatibility checks (exclude_hv=0, exclude_guest=1, size>64).
	attr = pmu_attr(0, 'u');
	attr.size = sizeof(attr);
	attr.exclude_hv = 0;
	attr.exclude_guest = 1;
	int fd = pmu_open_attr(&attr, 0, -1, -1, 0);
	CHECK(fd >= 0);
	close(fd);
	attr.sample_type = PERF_SAMPLE_IDENTIFIER;
	fd = pmu_open_attr(&attr, 0, -1, -1, 0);
	CHECK(fd >= 0);
	close(fd);
	attr.sample_period = 1;
	EXPECT_ERR(pmu_open_attr(&attr, 0, -1, -1, 0), EOPNOTSUPP);
	attr.sample_period = 0;
	attr.sample_type |= PERF_SAMPLE_IP;
	EXPECT_ERR(pmu_open_attr(&attr, 0, -1, -1, 0), EOPNOTSUPP);
	attr.sample_type = PERF_SAMPLE_IDENTIFIER;

	((unsigned char *)&attr)[64] = 1;
	EXPECT_ERR(pmu_open_attr(&attr, 0, -1, -1, 0), E2BIG);
	CHECK(attr.size == 64);
	attr = pmu_attr(0, 'u');
	attr.size = 0;
	fd = pmu_open_attr(&attr, 0, -1, -1, 0);
	CHECK(fd >= 0);
	close(fd);

	long page_size = sysconf(_SC_PAGESIZE);
	CHECK(page_size >= 64);
	char *memory = mmap(NULL, (size_t)page_size * 2, PROT_READ | PROT_WRITE,
			    MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
	CHECK(memory != MAP_FAILED);
	CHECK(mprotect(memory + page_size, (size_t)page_size, PROT_NONE) == 0);
	attr = pmu_attr(0, 'u');
	memcpy(memory + page_size - 32, &attr, 32);
	EXPECT_ERR(pmu_open_attr((void *)(memory + page_size - 32), 0, -1, -1,
				 0),
		   EFAULT);
	CHECK(munmap(memory, (size_t)page_size * 2) == 0);
	puts("PASS: invalid arguments, zero extensions, size writeback and user page faults");
}

static void controls(void)
{
	int fd = pmu_open(0, 'u', 0, 0);
	CHECK(fd >= 0);
	CHECK(fcntl(fd, F_GETFD) & FD_CLOEXEC);
	struct pmu_sample first, second;
	CHECK(pmu_read(fd, &first) == 0 && first.count == 0 &&
	      first.enabled_ns == 0);
	CHECK(pmu_control(fd, PERF_EVENT_IOC_ENABLE) == 0);
	pmu_burn(100000);
	CHECK(pmu_read(fd, &first) == 0 && first.count > 0);
	CHECK(pmu_control(fd, PERF_EVENT_IOC_ENABLE) == 0);
	pmu_burn(100000);
	CHECK(pmu_read(fd, &second) == 0 && second.count > first.count);
	CHECK(pmu_control(fd, PERF_EVENT_IOC_DISABLE) == 0);
	CHECK(pmu_control(fd, PERF_EVENT_IOC_DISABLE) == 0);
	CHECK(pmu_read(fd, &first) == 0);
	pmu_burn(100000);
	CHECK(pmu_read(fd, &second) == 0 && first.count == second.count);
	CHECK(first.enabled_ns == second.enabled_ns &&
	      first.running_ns == second.running_ns);
	CHECK(second.running_ns > 0 && second.running_ns <= second.enabled_ns);
	CHECK(pmu_control(fd, PERF_EVENT_IOC_RESET) == 0);
	CHECK(pmu_read(fd, &second) == 0 && second.count == 0);
	CHECK(first.enabled_ns == second.enabled_ns &&
	      first.running_ns == second.running_ns);
	uint64_t id = 0;
	CHECK(ioctl(fd, PERF_EVENT_IOC_ID, &id) == 0 && id == second.id &&
	      id != 0);
	EXPECT_ERR(ioctl(fd, PERF_EVENT_IOC_ID, NULL), EFAULT);
	EXPECT_ERR(syscall(SYS_read, fd, (uintptr_t)1, sizeof(first)), EFAULT);
	char small[31];
	EXPECT_ERR(read(fd, small, sizeof(small)), ENOSPC);
	EXPECT_ERR(ioctl(fd, 0xdeadbeefUL, 0), ENOTTY);
	EXPECT_ERR(ioctl(fd, PERF_EVENT_IOC_ENABLE, 1), EOPNOTSUPP);
	EXPECT_ERR(pmu_open(0, 'u', 0, 0), EBUSY);
	int other = pmu_open(1, 'u', 0, 0);
	CHECK(other >= 0);
	close(other);
	int alias = dup(fd);
	CHECK(alias >= 0);
	close(fd);
	CHECK(pmu_control(alias, PERF_EVENT_IOC_ENABLE) == 0);
	pmu_burn(100000);
	CHECK(pmu_control(alias, PERF_EVENT_IOC_DISABLE) == 0);
	CHECK(pmu_read(alias, &second) == 0 && second.count > 0 &&
	      second.id == id);
	close(alias);
	fd = pmu_open(0, 'u', 0, 0);
	CHECK(fd >= 0 && pmu_read(fd, &first) == 0 && first.count == 0 &&
	      first.id != id);
	close(fd);
	puts("PASS: enable/disable/reset/read, IDs, dup, final close and resource reuse");
}

static void read_formats(void)
{
	for (uint64_t format = 0; format < 8; ++format) {
		struct perf_event_attr attr = pmu_attr(1, 'u');
		attr.read_format = format;
		int fd = pmu_open_attr(&attr, 0, -1, -1, 0);
		CHECK(fd >= 0);
		uint64_t data[4] = { 0 };
		size_t bytes = 8 * (1 + __builtin_popcountll(format));
		CHECK(read(fd, data, sizeof(data)) == (ssize_t)bytes);
		CHECK(data[0] == 0);
		if (format & PERF_FORMAT_ID)
			CHECK(data[bytes / 8 - 1] != 0);
		close(fd);
	}
	puts("PASS: all eight supported non-group read layouts");
}

static void enabled_time_ignores_sleep(void)
{
	int fd = pmu_open(1, 'u', 0, 0);
	CHECK(fd >= 0 && pmu_control(fd, PERF_EVENT_IOC_ENABLE) == 0);
	pmu_burn(100000);
	struct pmu_sample before, after;
	CHECK(pmu_read(fd, &before) == 0);
	struct timespec delay = { .tv_nsec = 200000000 };
	while (nanosleep(&delay, &delay) < 0)
		CHECK(errno == EINTR);
	CHECK(pmu_control(fd, PERF_EVENT_IOC_DISABLE) == 0);
	CHECK(pmu_read(fd, &after) == 0);
	CHECK(after.enabled_ns >= before.enabled_ns);
	// A wall-clock implementation adds at least 200 ms here. Allow generous
	// syscall/emulation overhead while requiring sleep to be excluded.
	CHECK(after.enabled_ns - before.enabled_ns < 100000000);
	CHECK(after.enabled_ns == after.running_ns);
	close(fd);
	puts("PASS: per-task enabled time excludes sleep and does not require perf scaling");
}

static uint64_t measure_filter(uint64_t event, char scope, int kernel_work)
{
	int fd = pmu_open(event, scope, 0, 0);
	CHECK(fd >= 0 && pmu_control(fd, PERF_EVENT_IOC_ENABLE) == 0);
	if (kernel_work) {
		for (int i = 0; i < 100000; ++i)
			CHECK(syscall(SYS_getpid) > 0);
	} else {
		pmu_burn(1000000);
	}
	CHECK(pmu_control(fd, PERF_EVENT_IOC_DISABLE) == 0);
	struct pmu_sample sample;
	CHECK(pmu_read(fd, &sample) == 0 && sample.running_ns > 0);
	CHECK(close(fd) == 0);
	return sample.count;
}

static void filters(void)
{
	const uint64_t event = PERF_COUNT_HW_INSTRUCTIONS;
	uint64_t user_burn = measure_filter(event, 'u', 0);
	uint64_t kernel_burn = measure_filter(event, 'k', 0);
	uint64_t all_burn = measure_filter(event, 'a', 0);
	uint64_t user_syscalls = measure_filter(event, 'u', 1);
	uint64_t kernel_syscalls = measure_filter(event, 'k', 1);
	uint64_t all_syscalls = measure_filter(event, 'a', 1);

	printf("Instruction filters (user/kernel/all): burn %" PRIu64
	       "/%" PRIu64 "/%" PRIu64 ", syscalls %" PRIu64 "/%" PRIu64
	       "/%" PRIu64 "\n",
	       user_burn, kernel_burn, all_burn, user_syscalls, kernel_syscalls,
	       all_syscalls);
	// Allow kernel work from interrupts during the burn and user work
	// from dispatching syscalls. The combined count must include the
	// dominant mode, with slack for overhead differences between runs.
	CHECK(user_burn >= 1000000 && kernel_burn < user_burn / 4);
	CHECK(all_burn >= user_burn - user_burn / 4);
	CHECK(user_syscalls > 0 && user_syscalls < kernel_syscalls / 2);
	CHECK(all_syscalls >= kernel_syscalls - kernel_syscalls / 4);
	CHECK(measure_filter(PERF_COUNT_HW_CPU_CYCLES, 'u', 0) > 0);
	CHECK(measure_filter(PERF_COUNT_HW_CPU_CYCLES, 'k', 1) > 0);
	CHECK(measure_filter(PERF_COUNT_HW_CPU_CYCLES, 'a', 1) > 0);
	puts("PASS: user/kernel instruction filtering and cycles liveness");
}

static void remote_and_exit(void)
{
	int gate[2], reply[2];
	CHECK(pipe(gate) == 0 && pipe(reply) == 0);
	pid_t child = fork();
	CHECK(child >= 0);
	if (!child) {
		close(gate[1]);
		close(reply[0]);
		for (;;) {
			char command = 0;
			if (pmu_read_byte(gate[0], &command) < 0 ||
			    command == 'Q')
				_exit(0);
			pmu_burn(200000);
			if (pmu_write_byte(reply[1], command) < 0)
				_exit(1);
		}
	}
	close(gate[0]);
	close(reply[1]);
	int fd = pmu_open(1, 'u', child, 0);
	CHECK(fd >= 0 && pmu_control(fd, PERF_EVENT_IOC_ENABLE) == 0);
	uint64_t previous = 0;
	for (int i = 0; i < 10; ++i) {
		char command = 'B';
		CHECK(pmu_write_byte(gate[1], command) == 0);
		CHECK(pmu_read_byte(reply[0], &command) == 0);
		struct pmu_sample sample;
		CHECK(pmu_read(fd, &sample) == 0 && sample.count > previous);
		previous = sample.count;
	}
	char command = 'Q';
	CHECK(pmu_write_byte(gate[1], command) == 0);
	int status;
	CHECK(waitpid(child, &status, 0) == child && WIFEXITED(status) &&
	      WEXITSTATUS(status) == 0);
	struct pmu_sample first, second;
	CHECK(pmu_read(fd, &first) == 0 && first.count >= previous);
	pmu_burn(200000);
	CHECK(pmu_read(fd, &second) == 0 && first.count == second.count);
	CHECK(first.enabled_ns == second.enabled_ns &&
	      first.running_ns == second.running_ns);
	EXPECT_ERR(pmu_control(fd, PERF_EVENT_IOC_ENABLE), ESRCH);
	close(fd);
	close(gate[1]);
	close(reply[0]);
	puts("PASS: remote reads, scheduled segments and frozen post-exit result");
}

static int pmu_test_getcpu(void)
{
	unsigned int cpu = 0;
	if (syscall(SYS_getcpu, &cpu, NULL, NULL) != 0)
		return -1;
	return (int)cpu;
}

static void set_cpu_affinity(int cpu)
{
	cpu_set_t one;
	CHECK(cpu >= 0 && cpu < CPU_SETSIZE);
	CPU_ZERO(&one);
	CPU_SET(cpu, &one);
	CHECK(sched_setaffinity(0, sizeof(one), &one) == 0);
}

static void fork_and_cloexec(const char *program)
{
	int fd = pmu_open(PERF_COUNT_HW_INSTRUCTIONS, 'u', 0, 0);
	CHECK(fd >= 0);
	uint64_t id;
	CHECK(ioctl(fd, PERF_EVENT_IOC_ID, &id) == 0);
	int gate[2];
	CHECK(pipe(gate) == 0);
	pid_t parent = getpid();
	pid_t child = fork();
	CHECK(child >= 0);
	if (!child) {
		close(gate[1]);
		char command;
		CHECK(pmu_read_byte(gate[0], &command) == 0 && command == 'G');
		close(gate[0]);
		uint64_t inherited_id;
		CHECK(ioctl(fd, PERF_EVENT_IOC_ID, &inherited_id) == 0 &&
		      inherited_id == id);
		// The enabled event belongs to the parent, which must remain in
		// waitpid throughout both reads and the child's workload.
		CHECK(pmu_wait_until_sleeping(parent, parent) == 0);
		struct pmu_sample before, after;
		CHECK(pmu_read(fd, &before) == 0 && before.count > 0);
		pmu_burn(100000);
		CHECK(pmu_read(fd, &after) == 0 && after.count == before.count);
		char number[24];
		snprintf(number, sizeof(number), "%d", fd);
		execl(program, program, "--fd-closed", number, (char *)NULL);
		_exit(127);
	}
	close(gate[0]);
	CHECK(pmu_control(fd, PERF_EVENT_IOC_ENABLE) == 0);
	pmu_burn(1000);
	CHECK(pmu_write_byte(gate[1], 'G') == 0);
	close(gate[1]);
	int status;
	CHECK(waitpid(child, &status, 0) == child && WIFEXITED(status) &&
	      WEXITSTATUS(status) == 0);
	struct pmu_sample before, after;
	CHECK(pmu_read(fd, &before) == 0 && before.id == id);
	pmu_burn(100000);
	CHECK(pmu_control(fd, PERF_EVENT_IOC_DISABLE) == 0);
	CHECK(pmu_read(fd, &after) == 0 && after.count > before.count &&
	      after.id == id);
	close(fd);
	puts("PASS: enabled parent event ignores child work and survives child exec with CLOEXEC");
}

static void permissions(void)
{
	pid_t child = fork();
	CHECK(child >= 0);
	if (!child) {
		struct __user_cap_header_struct header = {
			.version = _LINUX_CAPABILITY_VERSION_3, .pid = 0
		};
		struct __user_cap_data_struct data[2] = { { 0 }, { 0 } };
		CHECK(syscall(SYS_capset, &header, data) == 0);
		EXPECT_ERR(pmu_open(0, 'u', 0, 0), EPERM);
		_exit(0);
	}
	int status;
	CHECK(waitpid(child, &status, 0) == child && WIFEXITED(status) &&
	      WEXITSTATUS(status) == 0);
	puts("PASS: capability-free caller cannot open a hardware event");
}

#if defined(__riscv)
static void instruction_count_matches_guest_work(void)
{
	const uint64_t iterations = 1000000;
	uint64_t remaining = iterations;
	int fd = pmu_open(PERF_COUNT_HW_INSTRUCTIONS, 'u', 0, 0);
	CHECK(fd >= 0 && pmu_control(fd, PERF_EVENT_IOC_ENABLE) == 0);

	// Each iteration retires exactly two guest instructions.
	__asm__ volatile("1: addi %0, %0, -1\n\t"
			 "bnez %0, 1b"
			 : "+r"(remaining));

	CHECK(pmu_control(fd, PERF_EVENT_IOC_DISABLE) == 0);
	struct pmu_sample sample;
	CHECK(pmu_read(fd, &sample) == 0);
	printf("RISC-V instruction loop: %" PRIu64 " retired instructions\n",
	       sample.count);
	// Allow user instructions in libc's enable and disable calls.
	CHECK(sample.count >= 2 * iterations &&
	      sample.count <= 2 * iterations + 4096);
	CHECK(close(fd) == 0);
}

static void read_counter_csr(int counter)
{
	unsigned long value;
	switch (counter) {
	case 0:
		__asm__ volatile("rdcycle %0" : "=r"(value));
		break;
	case 2:
		__asm__ volatile("rdinstret %0" : "=r"(value));
		break;
#define PROBE_HPM(n)                                                      \
	case n:                                                           \
		__asm__ volatile("csrr %0, hpmcounter" #n : "=r"(value)); \
		break
		PROBE_HPM(3);
		PROBE_HPM(4);
		PROBE_HPM(5);
		PROBE_HPM(6);
		PROBE_HPM(7);
		PROBE_HPM(8);
		PROBE_HPM(9);
		PROBE_HPM(10);
		PROBE_HPM(11);
		PROBE_HPM(12);
		PROBE_HPM(13);
		PROBE_HPM(14);
		PROBE_HPM(15);
		PROBE_HPM(16);
		PROBE_HPM(17);
		PROBE_HPM(18);
		PROBE_HPM(19);
		PROBE_HPM(20);
		PROBE_HPM(21);
		PROBE_HPM(22);
		PROBE_HPM(23);
		PROBE_HPM(24);
		PROBE_HPM(25);
		PROBE_HPM(26);
		PROBE_HPM(27);
		PROBE_HPM(28);
		PROBE_HPM(29);
		PROBE_HPM(30);
		PROBE_HPM(31);
#undef PROBE_HPM
	default:
		abort();
	}
}

static void user_counter_access_is_denied(void)
{
	for (int counter = 0; counter <= 31; ++counter) {
		if (counter == 1) // time is not a perf counter.
			continue;
		int ready[2];
		CHECK(pipe(ready) == 0);
		pid_t child = fork();
		CHECK(child >= 0);
		if (!child) {
			close(ready[0]);
			int cpu = pmu_test_getcpu();
			set_cpu_affinity(cpu);
			CHECK(pmu_test_getcpu() == cpu);
			int cycles =
				pmu_open(PERF_COUNT_HW_CPU_CYCLES, 'u', 0, 0);
			int instructions =
				pmu_open(PERF_COUNT_HW_INSTRUCTIONS, 'u', 0, 0);
			CHECK(cycles >= 0 && instructions >= 0);
			CHECK(pmu_control(cycles, PERF_EVENT_IOC_ENABLE) == 0);
			CHECK(pmu_control(instructions,
					  PERF_EVENT_IOC_ENABLE) == 0);
			struct pmu_sample cycles_before, instructions_before,
				sample;
			CHECK(pmu_read(cycles, &cycles_before) == 0);
			CHECK(pmu_read(instructions, &instructions_before) ==
			      0);
			pmu_burn(10000);
			CHECK(pmu_read(cycles, &sample) == 0 &&
			      sample.count > cycles_before.count);
			CHECK(pmu_read(instructions, &sample) == 0 &&
			      sample.count > instructions_before.count);
			CHECK(pmu_test_getcpu() == cpu);
			CHECK(pmu_write_byte(ready[1], 'R') == 0);
			read_counter_csr(counter);
			fprintf(stderr,
				"FAIL: U-mode counter CSR %d is readable\n",
				counter);
			_exit(1);
		}
		close(ready[1]);
		char byte;
		CHECK(pmu_read_byte(ready[0], &byte) == 0 && byte == 'R');
		close(ready[0]);
		int status;
		CHECK(waitpid(child, &status, 0) == child &&
		      WIFSIGNALED(status) && WTERMSIG(status) == SIGILL);
	}
	puts("PASS: RISC-V U-mode cycle, instret and hpmcounter3-31 reads trap during active counting");
}
#endif

static void restrict_capabilities(uint64_t mask)
{
	struct __user_cap_header_struct header = {
		.version = _LINUX_CAPABILITY_VERSION_3, .pid = 0
	};
	struct __user_cap_data_struct data[2] = { { 0 }, { 0 } };
	data[0].effective = data[0].permitted = (uint32_t)mask;
	data[1].effective = data[1].permitted = (uint32_t)(mask >> 32);
	CHECK(syscall(SYS_capset, &header, data) == 0);
}

static void alien_access_permissions(void)
{
	// Asterinas deliberately requires both monitoring capability and alien
	// access permission. Linux's CAP_PERFMON bypass is not this test's contract.
	int gate[2], ready[2];
	CHECK(pipe(gate) == 0 && pipe(ready) == 0);
	pid_t target = fork();
	CHECK(target >= 0);
	if (!target) {
		close(gate[1]);
		close(ready[0]);
		CHECK(setgid(1001) == 0 && setuid(1001) == 0);
		char byte = 'R';
		CHECK(pmu_write_byte(ready[1], byte) == 0);
		CHECK(pmu_read_byte(gate[0], &byte) == 0);
		_exit(0);
	}
	close(gate[0]);
	close(ready[1]);
	char byte;
	CHECK(pmu_read_byte(ready[0], &byte) == 0);
	pid_t same_uid = getpid();
	for (int allow_ptrace = 0; allow_ptrace < 2; ++allow_ptrace) {
		pid_t observer = fork();
		CHECK(observer >= 0);
		if (!observer) {
			uint64_t mask = UINT64_C(1) << CAP_PERFMON;
			if (allow_ptrace)
				mask |= UINT64_C(1) << CAP_SYS_PTRACE;
			restrict_capabilities(mask);
			// These opens must succeed with CAP_PERFMON alone, including
			// an explicit self PID and a same-UID process outside our group.
			pid_t allowed[] = { 0, getpid(), same_uid };
			for (size_t i = 0;
			     i < sizeof(allowed) / sizeof(allowed[0]); ++i) {
				int fd = pmu_open(0, 'u', allowed[i], 0);
				CHECK(fd >= 0);
				CHECK(close(fd) == 0);
			}
			if (!allow_ptrace) {
				EXPECT_ERR(pmu_open(0, 'u', target, 0), EPERM);
			} else {
				int fd = pmu_open(0, 'u', target, 0);
				CHECK(fd >= 0);
				CHECK(close(fd) == 0);
			}
			_exit(0);
		}
		int status;
		CHECK(waitpid(observer, &status, 0) == observer &&
		      WIFEXITED(status) && WEXITSTATUS(status) == 0);
	}
	CHECK(pmu_write_byte(gate[1], byte) == 0);
	int status;
	CHECK(waitpid(target, &status, 0) == target && WIFEXITED(status) &&
	      WEXITSTATUS(status) == 0);
	close(gate[1]);
	close(ready[0]);
	puts("PASS: CAP_PERFMON self/same-UID access, cross-UID denial, SYS_PTRACE authorization");
}

static void alien_event_freezes_on_id_change(void)
{
	const uint64_t post_setuid_iterations = 200000;
	int gate[2], reply[2];
	CHECK(pipe(gate) == 0 && pipe(reply) == 0);
	pid_t target = fork();
	CHECK(target >= 0);
	if (!target) {
		close(gate[1]);
		close(reply[0]);
		char command;
		while (pmu_read_byte(gate[0], &command) == 0 &&
		       command != 'Q') {
			if (command == 'C') {
				CHECK(setuid(1001) == 0);
				pmu_burn(post_setuid_iterations);
			} else {
				pmu_burn(200000);
			}
			CHECK(pmu_write_byte(reply[1], command) == 0);
		}
		_exit(0);
	}
	close(gate[0]);
	close(reply[1]);
	int fd = pmu_open(PERF_COUNT_HW_INSTRUCTIONS, 'u', target, 0);
	CHECK(fd >= 0 && pmu_control(fd, PERF_EVENT_IOC_ENABLE) == 0);
	char command = 'B';
	struct pmu_sample before, frozen, after;
	CHECK(pmu_write_byte(gate[1], command) == 0);
	CHECK(pmu_read_byte(reply[0], &command) == 0);
	CHECK(pmu_read(fd, &before) == 0 && before.count > 0);
	command = 'C';
	CHECK(pmu_write_byte(gate[1], command) == 0);
	CHECK(pmu_read_byte(reply[0], &command) == 0);
	CHECK(pmu_read(fd, &frozen) == 0 && frozen.count >= before.count);
	// Each burn iteration retires at least one user instruction. Only
	// the short handoff before setuid can contribute to this delta.
	CHECK(frozen.count - before.count < post_setuid_iterations);
	EXPECT_ERR(pmu_control(fd, PERF_EVENT_IOC_ENABLE), EACCES);
	command = 'B';
	CHECK(pmu_write_byte(gate[1], command) == 0);
	CHECK(pmu_read_byte(reply[0], &command) == 0);
	CHECK(pmu_read(fd, &after) == 0 && after.count == frozen.count &&
	      after.running_ns == frozen.running_ns &&
	      after.enabled_ns == frozen.enabled_ns);
	command = 'Q';
	CHECK(pmu_write_byte(gate[1], command) == 0);
	int status;
	CHECK(waitpid(target, &status, 0) == target && WIFEXITED(status) &&
	      WEXITSTATUS(status) == 0);
	close(fd);
	close(gate[1]);
	close(reply[0]);
	puts("PASS: target UID change freezes existing cross-process events");
}

static void cpu_filter(void)
{
	cpu_set_t original;
	CHECK(sched_getaffinity(0, sizeof(original), &original) == 0);
	int cpu = pmu_test_getcpu();
	printf("CPU diagnostic: libc=%d syscall=%d\n", sched_getcpu(), cpu);
	CHECK(cpu >= 0 && cpu < CPU_SETSIZE);
	CHECK(CPU_ISSET(cpu, &original));
	set_cpu_affinity(cpu);
	struct perf_event_attr attr = pmu_attr(1, 'u');
	int fd = pmu_open_attr(&attr, 0, cpu, -1, 0);
	CHECK(fd >= 0 && pmu_control(fd, PERF_EVENT_IOC_ENABLE) == 0);
	pmu_burn(100000);
	CHECK(pmu_control(fd, PERF_EVENT_IOC_DISABLE) == 0);
	struct pmu_sample sample;
	CHECK(pmu_read(fd, &sample) == 0 && sample.count > 0);
	close(fd);
	int other = -1;
	for (int i = 0; i < CPU_SETSIZE; ++i)
		if (CPU_ISSET(i, &original) && i != cpu) {
			other = i;
			break;
		}
	if (other >= 0) {
		fd = pmu_open_attr(&attr, 0, other, -1, 0);
		CHECK(fd >= 0 && pmu_control(fd, PERF_EVENT_IOC_ENABLE) == 0);
		pmu_burn(100000);
		CHECK(pmu_control(fd, PERF_EVENT_IOC_DISABLE) == 0);
		CHECK(pmu_read(fd, &sample) == 0 && sample.count == 0 &&
		      sample.running_ns == 0);
		close(fd);
	} else {
		puts("INFO: only the matching-CPU filter branch was exercised (one allowed CPU)");
	}
	CHECK(sched_setaffinity(0, sizeof(original), &original) == 0);
	puts("PASS: requested per-thread CPU filters");
}

static int migration(void)
{
	const uint64_t iterations = 100000;
	cpu_set_t original;
	CHECK(sched_getaffinity(0, sizeof(original), &original) == 0);
	int source = pmu_test_getcpu();
	CHECK(source >= 0 && source < CPU_SETSIZE &&
	      CPU_ISSET(source, &original));
	int target = -1;
	for (int i = 0; i < CPU_SETSIZE; ++i)
		if (CPU_ISSET(i, &original) && i != source) {
			target = i;
			break;
		}
	if (target < 0) {
		puts("SKIP migration: no second allowed CPU");
		return 77;
	}

	set_cpu_affinity(source);
	CHECK(pmu_test_getcpu() == source);
	int fd = pmu_open(PERF_COUNT_HW_INSTRUCTIONS, 'u', 0, 0);
	CHECK(fd >= 0 && pmu_control(fd, PERF_EVENT_IOC_ENABLE) == 0);
	pmu_burn(iterations);
	struct pmu_sample before, baseline, after;
	CHECK(pmu_read(fd, &before) == 0 && before.count >= iterations);
	CHECK(pmu_test_getcpu() == source);

	set_cpu_affinity(target);
	int observed = pmu_test_getcpu();
	for (int attempt = 0; attempt < 100 && observed != target; ++attempt) {
		struct timespec delay = { .tv_nsec = 1000000 };
		nanosleep(&delay, NULL);
		sched_yield();
		observed = pmu_test_getcpu();
	}
	if (observed != target) {
		CHECK(close(fd) == 0);
		CHECK(sched_setaffinity(0, sizeof(original), &original) == 0);
		printf("SKIP migration: scheduler stayed on CPU %d (requested %d); migration is unverified\n",
		       observed, target);
		return 77;
	}

	CHECK(pmu_read(fd, &baseline) == 0 && baseline.count >= before.count);
	pmu_burn(iterations);
	CHECK(pmu_test_getcpu() == target);
	CHECK(pmu_control(fd, PERF_EVENT_IOC_DISABLE) == 0);
	CHECK(pmu_read(fd, &after) == 0 && after.count >= baseline.count &&
	      after.count - baseline.count >= iterations);
	CHECK(close(fd) == 0);
	CHECK(sched_setaffinity(0, sizeof(original), &original) == 0);
	printf("PASS: count continues across CPU %d -> %d\n", source, target);
	return 0;
}

int main(int argc, char **argv)
{
	if (argc == 3 && !strcmp(argv[1], "--fd-closed")) {
		int fd = atoi(argv[2]);
		EXPECT_ERR(fcntl(fd, F_GETFD), EBADF);
		return 0;
	}
	abi_layout();
	if (argc == 2 && !strcmp(argv[1], "--abi-only"))
		return 0;
	if (argc > 1 && (argc != 2 || strcmp(argv[1], "--migration")))
		return 2;
	alarm(90);
	int probe = pmu_open(0, 'u', 0, 0);
	if (probe < 0)
		return pmu_failure("PMU preflight");
	close(probe);
	if (argc == 2)
		return migration();
	abi_errors();
	controls();
	read_formats();
	filters();
	enabled_time_ignores_sleep();
	remote_and_exit();
	fork_and_cloexec(argv[0]);
	permissions();
#if defined(__riscv)
	instruction_count_matches_guest_work();
	user_counter_access_is_denied();
#endif
	alien_access_permissions();
	alien_event_freezes_on_id_change();
	cpu_filter();
	puts("PASS: counting regression suite (migration is a separate test)");
	return 0;
}
