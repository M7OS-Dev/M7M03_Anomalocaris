/******************************************************************************
Filename    : main.rs
Author      : yhy
Date        : 13/10/2026
Licence     : The Unlicense; see LICENSE for details.
Description : The first user-mode program (init) for the RV64V port on
              QEMU virt, written in Rust. Runs in U-mode as the init thread,
              entered by the kernel via __RME_User_Enter. Runs the thread-switch
              benchmark (a Rust port of rme_benchmark.c), then idles in a busy
              loop (WFI is an illegal instruction on QEMU).
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

use benchmark::rme_benchmark;
/* End Include ***************************************************************/

/* Entry:_start ***************************************************************
Description : The assembly entry point of the user-mode init program,
              mirroring the C start.S. The kernel's __RME_User_Enter srets
              here with:
                sp = INIT_STACK (0x20200000), a0 = CPUID, sstatus.SPIE set
                (U-mode).
              Sets up the runtime environment (gp, .bss), then calls main() -
              the standard RISC-V freestanding bootstrap.
              No .data relocation is needed: the kernel maps the program's
              virtual window straight onto the physical pages the loader
              filled, so the linked (virtual) addresses and the loaded
              (physical) bytes line up.
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

    /* Zero the .bss section */
    LA                  a1,__bss_start
    LA                  a2,__bss_end
    BEQ                 a1,a2,2f
1:
    SD                  zero,0(a1)
    ADDI                a1,a1,8
    BNE                 a1,a2,1b
2:

    /* Call the Rust main function */
    CALL                main

    /* main returned - should never happen; idle forever */
1:
    J                   1b
    "#
);
/* End Entry:_start **********************************************************/

/* Function:main **************************************************************
Description : The entry point of the init program. The assembly bootstrap in
              _start sets up the runtime environment, then calls this
              function. Runs the thread-switch benchmark, then idles forever.
Input       : None.
Output      : None.
Return      : ! - This function never returns.
******************************************************************************/
#[unsafe(no_mangle)]
pub extern "C" fn main() -> ! {
    rme_benchmark()
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
