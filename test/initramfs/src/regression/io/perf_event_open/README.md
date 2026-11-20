# Hardware counting with `perf stat`

Run the matrix in an x86-64 Asterinas guest with an exposed PMU, a stock Linux
`perf` binary in `PATH`, and permission to open perf events. The regression
build packages the runner and its single-threaded workload together. The workload
runs a user-space loop and issues a system call on each outer iteration.

```sh
cd /test/io/perf_event_open
sh ./perf_stat_matrix.sh ./stat_workload /tmp/perf-stat-matrix
```

Use a new results directory for each run. The matrix runs each of the 15
nonempty combinations ten times. The two events are independent, and each
can count user space, kernel space, or both. Each invocation must exit
successfully and return exactly one positive count with positive running time
for each requested event.

`cases.csv` records the event selection for each case and `counts.csv` holds
the 240 individual event measurements. The directory also contains the raw
`perf stat` CSV for each invocation and the kernel and perf version strings.
