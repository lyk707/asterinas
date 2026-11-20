// SPDX-License-Identifier: MPL-2.0

#define _GNU_SOURCE
#include <sys/syscall.h>
#include <unistd.h>

int main(void)
{
	volatile unsigned long value = 1;

	for (int i = 0; i < 4096; i++) {
		for (int j = 0; j < 256; j++)
			value = value * 3 + 1;

		// Enter the kernel on each iteration to exercise kernel-mode counting.
		if (syscall(SYS_gettid) <= 0)
			return 1;
	}

	return 0;
}
