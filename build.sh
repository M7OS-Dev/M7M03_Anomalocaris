#!/bin/sh
set -e

# Emit build/init.bin for the kernel (M7M01_Eukaron): its makefile invokes
# this script and copies the result into its Object/ folder.
cd "$(dirname "$0")" # run from anywhere; lets cargo find .cargo/config.toml

cargo build --release # riscv64gc-unknown-none-elf release profile

mkdir -p build

riscv64-unknown-elf-objcopy -O binary -S \
    target/riscv64gc-unknown-none-elf/release/init \
    build/init.bin

echo "Built init.bin -> $(pwd)/build/init.bin"
