// SPDX-License-Identifier: MPL-2.0

use sctrace::{CliReporterBuilder, Patterns, SctraceBuilder, StraceLogStream};

fn check_perf_event_open(scml: &str) {
    for (extra_fields, sample_type, read_format, pid, cpu, group_fd, supported) in [
        (
            "exclude_kernel=1, precise_ip=0 /* arbitrary skid */, ...",
            "PERF_SAMPLE_IDENTIFIER",
            "PERF_FORMAT_TOTAL_TIME_ENABLED|PERF_FORMAT_TOTAL_TIME_RUNNING",
            0,
            -1,
            -1,
            true,
        ),
        (
            "exclude_user=1, inherit=0, config1=0x0",
            "0",
            "PERF_FORMAT_ID",
            7,
            2,
            -1,
            true,
        ),
        ("disabled=1", "0", "0", 0, -1, -1, true),
        ("exclude_user=0x0", "0", "0", 0, -1, -1, true),
        ("config1=1", "0", "0", 0, -1, -1, false),
        ("disabled=1", "0", "0", -1, 0, -1, false),
        ("disabled=1", "0", "0", 0, -1, 3, false),
        ("disabled=1", "0", "0", 0, -2, -1, false),
        ("inherit=1", "0", "0", 0, -1, -1, false),
        (
            "precise_ip=1 /* constant skid */",
            "0",
            "0",
            0,
            -1,
            -1,
            false,
        ),
        ("mmap=1", "0", "0", 0, -1, -1, false),
        (
            "exclude_user=1, exclude_kernel=1",
            "0",
            "0",
            0,
            -1,
            -1,
            false,
        ),
        (
            "disabled=1",
            "PERF_SAMPLE_IDENTIFIER|0x800000",
            "0",
            0,
            -1,
            -1,
            false,
        ),
        (
            "disabled=1",
            "0",
            "PERF_FORMAT_TOTAL_TIME_RUNNING|0x40",
            0,
            -1,
            -1,
            false,
        ),
    ] {
        let result = if supported {
            "3"
        } else {
            "-1 EINVAL (Invalid argument)"
        };
        let trace = format!(
            "perf_event_open({{type=PERF_TYPE_HARDWARE, size=PERF_ATTR_SIZE_VER0, config=PERF_COUNT_HW_CPU_CYCLES, sample_period=0, sample_type={sample_type}, read_format={read_format}, {extra_fields}}}, {pid}, {cpu}, {group_fd}, PERF_FLAG_FD_CLOEXEC) = {result}"
        );
        let reports = SctraceBuilder::new()
            .patterns(Patterns::from_scml(scml).unwrap())
            .strace(StraceLogStream::from_string(&trace).unwrap())
            .reporter(CliReporterBuilder::new().quiet().collect().build())
            .build()
            .run()
            .unwrap()
            .unwrap();

        if supported {
            assert!(reports.is_empty(), "{trace}: {reports:?}");
        } else {
            assert_eq!(reports, [format!("Unsupported syscall: {trace}")]);
        }
    }
}

#[test]
fn test_perf_event_open_traces() {
    let fixture = include_str!("fixtures/perf_event_open.scml");
    check_perf_event_open(fixture);

    // The repository check also exercises the book's pattern. The packaged
    // crate can run this test without a checkout of the book.
    if let Some(path) = std::env::var_os("PMU_BOOK_SCML") {
        let book_pattern = std::fs::read_to_string(path).unwrap();
        assert!(
            book_pattern.lines().eq(fixture.lines()),
            "the packaged SCML must match the book"
        );
    }
}

#[test]
fn test_perf_ioctl_traces() {
    let Some(path) = std::env::var_os("PMU_IOCTL_SCML") else {
        return;
    };
    let scml = std::fs::read_to_string(path).unwrap();
    let trace = "\
ioctl(3, PERF_EVENT_IOC_ENABLE, 0) = 0\n\
ioctl(3, PERF_EVENT_IOC_DISABLE, 0) = 0\n\
ioctl(3, PERF_EVENT_IOC_RESET, 0) = 0\n\
ioctl(3, PERF_EVENT_IOC_ID, [1]) = 0\n\
ioctl(3, PERF_EVENT_IOC_ENABLE, PERF_IOC_FLAG_GROUP) = -1 EOPNOTSUPP (Operation not supported)\n\
ioctl(3, PERF_EVENT_IOC_SET_OUTPUT, 4) = -1 ENOTTY (Inappropriate ioctl for device)";
    let reports = SctraceBuilder::new()
        .patterns(Patterns::from_scml(&scml).unwrap())
        .strace(StraceLogStream::from_string(trace).unwrap())
        .reporter(CliReporterBuilder::new().quiet().collect().build())
        .build()
        .run()
        .unwrap()
        .unwrap();
    assert_eq!(
        reports,
        trace
            .lines()
            .skip(4)
            .map(|line| format!("Unsupported syscall: {line}"))
            .collect::<Vec<_>>()
    );
}
