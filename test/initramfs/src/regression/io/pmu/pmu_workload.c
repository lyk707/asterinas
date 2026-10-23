// SPDX-License-Identifier: MPL-2.0

#include "pmu_common.h"

int main(int argc, char **argv)
{
	uint64_t iterations = 2000000;
	if (argc > 2) {
		fprintf(stderr, "usage: %s [iterations]\n", argv[0]);
		return 2;
	}
	if (argc == 2) {
		char *end;
		errno = 0;
		iterations = strtoull(argv[1], &end, 10);
		if (errno || *end || end == argv[1] || argv[1][0] == '-' ||
		    iterations > 1000000000) {
			fprintf(stderr,
				"iterations must be in 0..1000000000\n");
			return 2;
		}
	}
	printf("checksum=%" PRIu64 "\n", pmu_burn(iterations));
	return 0;
}
