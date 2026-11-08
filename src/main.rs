/******************************************************************************
Filename    : main.rs
Author      : yhy
Date        : 13/10/2026
Licence     : The Unlicense; see LICENSE for details.
Description : The first user-mode program (init) for the RV64V port on
              QEMU virt, written in Rust. Runs in U-mode as the init thread,
              entered by the kernel via __RME_User_Enter. Runs the standard
              RVM benchmark suite (a Rust port of rme_benchmark.c, mirroring
              the scenario list of the Ammonite rvm_benchmark.h), then idles
              in a busy loop (WFI is an illegal instruction on QEMU).
              This is the init program (first user process) for the RV64V port
              of the RME microkernel (M7M01_Eukaron), written in Rust. The
              kernel's Project/GCC-RV64V build invokes this project's build.sh
              and copies the resulting build/init.bin into its Object/ folder,
              where qemu.sh loads it at 0x81030000.
******************************************************************************/

/* Attribute *****************************************************************/
#![no_std]
#![no_main]
/* End Attribute *************************************************************/

/* Include *******************************************************************/
mod benchmark;
mod syscall;

use core::panic::PanicInfo;

use benchmark::{bench_main, bench_print_char};
/* End Include ***************************************************************/

/* Entry:_start ***************************************************************
Description : The assembly entry point of the user-mode init program,
              mirroring the C start.S. The kernel's __RME_User_Enter srets
              here with:
                sp = this CPU's initial user stack, a0 = CPUID,
                sstatus.SPIE set (U-mode).
              Sets up the runtime environment (gp, .bss), then calls main() -
              the standard RISC-V freestanding bootstrap.
              No .data relocation is needed: the kernel maps the program's
              virtual window straight onto the physical pages the loader
              filled, so the linked (virtual) addresses and the loaded
              (physical) bytes line up.
              Every hart of the SMP port enters this same program in the same
              address space, so only CPU0 owns the shared .bss; the secondary
              harts must not clear it while CPU0 is running the benchmark.
******************************************************************************/
core::arch::global_asm!(
    r#"
    .section .text._start
    .align              2
    .global             _start

_start:
    /* Set up the global pointer for small-data accesses */
    .option push
    .option norelax
    LA                  gp,__global_pointer$
    .option pop

    /* Only CPU0 owns the shared .bss */
    BNEZ                a0,2f

    /* Zero the .bss section */
    LA                  a1,__bss_start
    LA                  a2,__bss_end
    BEQ                 a1,a2,2f
1:
    SD                  zero,0(a1)
    ADDI                a1,a1,8
    BNE                 a1,a2,1b
2:

    /* Call the Rust main function. a0 still holds the CPUID the kernel passed
     * in __RME_User_Enter and becomes main's first argument. */
    CALL                main

    /* main returned - should never happen; idle forever */
1:
    J                   1b
    "#
);
/* End Entry:_start **********************************************************/

/* Function:main **************************************************************
Description : The entry point of the init program. CPU0 runs the benchmark
              suite, whose static data has a single owner; every secondary
              hart prints a-z once and then idles, which still proves that all
              harts reach user mode.
Input       : cpuid - The logical CPU index passed in a0 by the kernel.
Output      : None.
Return      : ! - This function never returns.
******************************************************************************/
#[unsafe(no_mangle)]
pub extern "C" fn main(cpuid: u64) -> ! {
    if cpuid == 0 {
        bench_main()
    }

    /* Secondary harts: print a-z once, then idle */
    let mut ch: u8 = b'a';
    while ch <= b'z' {
        bench_print_char(ch);
        ch += 1;
    }
    bench_print_char(b'\r');
    bench_print_char(b'\n');

    loop {}
}
/* End Function:main *********************************************************/

/* Function:panic *************************************************************
Description : The panic handler. Idles forever - there is no way to abort a
              user-mode thread from here, and WFI is an illegal instruction
              on QEMU.
Input       : _info - The panic information (unused).
Output      : None.
Return      : ! - This function never returns.
******************************************************************************/
#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {}
}
/* End Function:panic ********************************************************/

/* End Of File ***************************************************************/

/* Copyright (C) Evo-Devo Instrum. All rights reserved ***********************/
