/******************************************************************************
Filename    : main.rs
Author      : yhy
Date        : 13/10/2026
Licence     : The Unlicense; see LICENSE for details.
Description : The first user-mode program (init) for the RV64V port on
              QEMU virt, written in Rust. Runs in U-mode as the init thread,
              entered by the kernel via __RME_User_Enter. Prints the uppercase
              alphabet (A-Z) on a single line through the DEBUG_PRINT
              kernel-function capability syscall, then idles in a busy loop
              (WFI is an illegal instruction on QEMU).
              This is a Rust replacement for the C init program that lives in
              the RME microkernel repository (M7M01_Eukaron) at
              Project/GCC-RV64V/init/. See the README for the build and
              deployment flow, which references M7M01_Eukaron the same way
              M7M02_Ammonite does.
******************************************************************************/

/* Attribute *****************************************************************/
#![no_std]
#![no_main]
/* End Attribute *************************************************************/

/* Include *******************************************************************/
mod syscall;

use core::panic::PanicInfo;

use syscall::{RME_BOOT_INIT_KFN, RME_KFN_DEBUG_PRINT, RME_SVC_KFN, rme_syscall};
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

/* Function:print_char ********************************************************
Description : Print one character by invoking the DEBUG_PRINT kernel function.
              Param0 packs D0 = Func_ID (DEBUG_PRINT) and D1 = Sub_ID (the
              character).
Input       : ch - The character to print.
Output      : None.
Return      : None.
******************************************************************************/
fn print_char(ch: u8) {
    /* Param0 = (Sub_ID << 32) | Func_ID */
    let param0: u64 = ((ch as u64) << 32) | RME_KFN_DEBUG_PRINT;

    let _ = rme_syscall(RME_SVC_KFN, RME_BOOT_INIT_KFN, param0, 0, 0);
}
/* End Function:print_char ***************************************************/

/* Function:print_str *********************************************************
Description : Print a byte slice, one syscall per character.
Input       : s - The characters to print.
Output      : None.
Return      : None.
******************************************************************************/
fn print_str(s: &[u8]) {
    for &ch in s {
        print_char(ch);
    }
}
/* End Function:print_str ****************************************************/

/* Function:main **************************************************************
Description : The entry point of the init program. The assembly bootstrap in
              _start sets up the runtime environment, then calls this
              function. Prints the uppercase alphabet, then idles forever.
Input       : None.
Output      : None.
Return      : ! - This function never returns.
******************************************************************************/
#[unsafe(no_mangle)]
pub extern "C" fn main() -> ! {
    /* Print the uppercase alphabet A-Z on a single line */
    for ch in b'A'..=b'Z' {
        print_char(ch);
    }
    print_str(b"\r\n");

    /* Idle forever - busy loop, no WFI (illegal instruction on QEMU) */
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
