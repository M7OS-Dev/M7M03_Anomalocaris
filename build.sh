#!/bin/sh

# Build the Rust init program (M7M03_Anomalocaris) and emit the raw binary
# into this project's build/ folder. This project is self-contained: it does
# NOT reference the kernel. Instead, the kernel (M7M01_Eukaron) references
# this project - its build invokes this script and copies the resulting
# build/init.bin into its own Object/ folder.

# Work from this script's directory so it can be called from anywhere
# (e.g. from the kernel's GCC-RV64V folder during `make`), and so that
# cargo picks up this project's .cargo/config.toml (target + rustflags).
SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
cd "$SCRIPT_DIR"

# Build the init program (release profile, riscv64gc-unknown-none-elf)
cargo build --release

# Convert the ELF to a raw binary (the kernel's qemu.sh loads it at 0x81030000)
mkdir -p build
riscv64-unknown-elf-objcopy -O binary -S \
    target/riscv64gc-unknown-none-elf/release/init \
    build/init.bin

echo "Built init.bin -> $(pwd)/build/init.bin"
