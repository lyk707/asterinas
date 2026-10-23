// SPDX-License-Identifier: MPL-2.0

// Monitor one child's initial thread, from successful exec to exit. This tool
// deliberately does not imply aggregation of forked processes or new threads.
#include "pmu_common.h"
#include <fcntl.h>
#include <signal.h>
#include <sys/wait.h>

struct event {
	uint64_t code;
	char scope;
	const char *name;
	int fd;
};
static volatile sig_atomic_t child_pid;

static void forward_signal(int sig)
{
	int saved = errno;
	if (child_pid > 0)
		kill((pid_t)child_pid, sig);
	errno = saved;
}

static void usage(const char *program)
{
	fprintf(stderr,
		"usage: %s [-e cycles:u,instructions:u] -- program [args...]\n"
		"Scopes: :u user, :k kernel, :uk both. One monitored thread, no inheritance.\n",
		program);
}

static int parse_events(const char *text, struct event events[2])
{
	char buffer[128];
	if (strlen(text) >= sizeof(buffer) || !*text || text[0] == ',' ||
	    text[strlen(text) - 1] == ',' || strstr(text, ",,"))
		return -1;
	strcpy(buffer, text);
	unsigned int used = 0;
	int n = 0;
	char *save = NULL;
	for (char *part = strtok_r(buffer, ",", &save); part;
	     part = strtok_r(NULL, ",", &save)) {
		if (n == 2)
			return -1;
		char *scope = strchr(part, ':');
		char mode = 'u';
		if (scope) {
			*scope++ = 0;
			if (!strcmp(scope, "u"))
				mode = 'u';
			else if (!strcmp(scope, "k"))
				mode = 'k';
			else if (!strcmp(scope, "uk"))
				mode = 'a';
			else
				return -1;
		}
		int code;
		if (!strcmp(part, "cycles"))
			code = 0;
		else if (!strcmp(part, "instructions"))
			code = 1;
		else
			return -1;
		if (used & (1u << code))
			return -1;
		used |= 1u << code;
		events[n++] = (struct event){ .code = (uint64_t)code,
					      .scope = mode,
					      .name = code ? "instructions" :
							     "cycles",
					      .fd = -1 };
	}
	return n;
}

int main(int argc, char **argv)
{
	struct event events[2];
	const char *selection = "cycles:u,instructions:u";
	int arg = 1;
	while (arg < argc && strcmp(argv[arg], "--")) {
		if (!strcmp(argv[arg], "--help")) {
			usage(argv[0]);
			return 0;
		}
		if (!strcmp(argv[arg], "-e") && arg + 1 < argc) {
			selection = argv[arg + 1];
			arg += 2;
		} else {
			usage(argv[0]);
			return 2;
		}
	}
	int n = parse_events(selection, events);
	if (n < 1 || arg + 1 >= argc) {
		usage(argv[0]);
		return 2;
	}
	++arg;

	int gate[2], exec_status[2];
	if (pipe2(gate, O_CLOEXEC) < 0) {
		perror("pipe");
		return 1;
	}
	if (pipe2(exec_status, O_CLOEXEC) < 0) {
		perror("pipe");
		close(gate[0]);
		close(gate[1]);
		return 1;
	}
	pid_t child = fork();
	if (child < 0) {
		perror("fork");
		close(gate[0]);
		close(gate[1]);
		close(exec_status[0]);
		close(exec_status[1]);
		return 1;
	}
	if (!child) {
		close(gate[1]);
		close(exec_status[0]);
		char command;
		if (pmu_byte(gate[0], &command, 0) < 0 || command != 'G')
			_exit(125);
		close(gate[0]);
		execvp(argv[arg], &argv[arg]);
		int error = errno;
		ssize_t ignored = write(exec_status[1], &error, sizeof(error));
		(void)ignored;
		_exit(127);
	}
	child_pid = child;
	close(gate[0]);
	close(exec_status[1]);
	struct sigaction action = { .sa_handler = forward_signal };
	sigemptyset(&action.sa_mask);
	sigaction(SIGINT, &action, NULL);
	sigaction(SIGTERM, &action, NULL);
	struct sigaction ignore = { .sa_handler = SIG_IGN };
	sigemptyset(&ignore.sa_mask);
	sigaction(SIGPIPE, &ignore, NULL);
	int result = 0;
	for (int i = 0; i < n; ++i) {
		events[i].fd =
			pmu_open(events[i].code, events[i].scope, child, 1);
		if (events[i].fd < 0) {
			result = pmu_failure("open child event");
			break;
		}
	}
	if (!result) {
		char command = 'G';
		if (pmu_byte(gate[1], &command, 1) < 0) {
			perror("start child");
			result = 1;
		}
	}
	close(gate[1]);
	int exec_error = 0;
	ssize_t length;
	do {
		length = read(exec_status[0], &exec_error, sizeof(exec_error));
	} while (length < 0 && errno == EINTR);
	close(exec_status[0]);
	if (!result && length != 0) {
		if (length == sizeof(exec_error))
			fprintf(stderr, "exec %s: %s\n", argv[arg],
				strerror(exec_error));
		else
			fprintf(stderr, "exec-status pipe failed\n");
		result = 127;
	}
	int status = 0;
	pid_t waited;
	do {
		waited = waitpid(child, &status, 0);
	} while (waited < 0 && errno == EINTR);
	child_pid = 0;
	if (waited < 0) {
		perror("waitpid");
		result = 1;
	}
	for (int i = 0; i < n; ++i) {
		if (events[i].fd < 0)
			continue;
		if (!result) {
			struct pmu_sample sample;
			if (pmu_read(events[i].fd, &sample) < 0)
				result = pmu_failure("read final event");
			else
				printf("%20" PRIu64 "  %s:%s  enabled=%" PRIu64
				       "ns running=%" PRIu64 "ns id=%" PRIu64
				       "\n",
				       sample.count, events[i].name,
				       events[i].scope == 'a' ? "uk" :
				       events[i].scope == 'u' ? "u" :
								"k",
				       sample.enabled_ns, sample.running_ns,
				       sample.id);
		}
		close(events[i].fd);
	}
	if (result)
		return result;
	if (WIFEXITED(status))
		return WEXITSTATUS(status);
	return WIFSIGNALED(status) ? 128 + WTERMSIG(status) : 1;
}
