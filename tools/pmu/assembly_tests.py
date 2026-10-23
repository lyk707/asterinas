# SPDX-License-Identifier: MPL-2.0
"""Compile production assembly and inspect fault sites in ELF objects, without executing it."""

from pathlib import Path
import os
import struct
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]
RUSTC = os.environ.get("RUSTC", "rustc")


def symbols(path):
    data = path.read_bytes()
    assert data[:6] == b"\x7fELF\x02\x01", "expected little-endian ELF64"
    offset = struct.unpack_from("<Q", data, 40)[0]
    size, count = struct.unpack_from("<HH", data, 58)
    sections = [struct.unpack_from("<IIQQQQIIQQ", data, offset + i * size) for i in range(count)]
    result = {}
    for section in sections:
        if section[1] != 2:  # SHT_SYMTAB
            continue
        names = sections[section[6]]
        strings = data[names[4]:names[4] + names[5]]
        for position in range(section[4], section[4] + section[5], section[9]):
            name, _, _, index, value, _ = struct.unpack_from("<IBBHQQ", data, position)
            if not name or not index or index >= len(sections):
                continue
            label = strings[name:strings.index(b"\0", name)].decode()
            result[label] = (index, value, sections[index][4] + value)
    return data, result


with tempfile.TemporaryDirectory(prefix="pmu-asm-") as directory:
    folder = Path(directory)
    for name, target in [("riscv_csr", "riscv64imac-unknown-none-elf"), ("x86_msr", "x86_64-unknown-none")]:
        assembly = (ROOT / "ostd/src/pmu" / (name + ".S")).as_posix()
        source = folder / (name + ".rs")
        source.write_text('#![no_std]\ncore::arch::global_asm!(include_str!(r"' + assembly + '"));\n')
        obj = folder / (name + ".o")
        subprocess.run([RUSTC, "--edition=2024", "--crate-type", "lib", "--target", target,
                        "--emit=obj", str(source), "-o", str(obj)], check=True)
        data, labels = symbols(obj)
        if name == "riscv_csr":
            start, end = labels["__pmu_csr_sites"], labels["__pmu_csr_sites_end"]
            assert start[0] == end[0] and end[1] - start[1] == 32 * 8
            for i in range(32):
                csr, jump = struct.unpack_from("<II", data, start[2] + 8 * i)
                assert csr & 0x707f == 0x2073 and csr >> 20 == 0xc00 + i
                assert jump & 0x7f == 0x6f
            assert labels["__pmu_csr_failed"][1] >= end[1]
        else:
            for label, opcode in [("__pmu_rdmsr_site", b"\x0f\x32"), ("__pmu_wrmsr_site", b"\x0f\x30")]:
                position = labels[label][2]
                assert data[position:position + 2] == opcode
        print("PASS", target, "assembly and exact fault-site layout")
print("Assembly compilation/layout only; traps have not been exercised in a guest.")
