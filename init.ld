/******************************************************************************
Filename    : init.ld
Author      : yhy
Date        : 13/10/2026
Licence     : The Unlicense; see LICENSE for details.
Description : The linker script for the user-mode init program, mirroring the
              C init.ld in M7M01_Eukaron/Project/GCC-RV64V/init/.
              QEMU loads the raw binary at the physical 0x81030000 (see the
              kernel's qemu.sh), but the program is linked and runs at the
              user virtual address 0x20000000 (same as X64); the kernel maps
              it and its stack (VA 0x201F0000..0x20200000) onto the physical
              pages.
******************************************************************************/

OUTPUT_FORMAT("elf64-littleriscv")
OUTPUT_ARCH(riscv)
ENTRY(_start)

SECTIONS
{
  /* User virtual address of the init program (matches X64's 0x20000000) */
  . = 0x20000000;

  /* The entry section must come first so that the image starts exactly at
   * 0x20000000, where the kernel's RME_RV64V_INIT_ENTRY srets to. */
  .text : {
    *(.text._start)
    *(.text .text.* .gnu.linkonce.t.*)
  }

  . = ALIGN(0x8);

  .rodata : {
    *(.rodata .rodata.* .gnu.linkonce.r.*)
  }

  . = ALIGN(0x8);

  .data : {
    *(.data .data.* .gnu.linkonce.d.*)
  }

  . = ALIGN(0x8);

  .sdata : {
    *(.sdata .sdata.*)
  }

  /* Global pointer - set by the assembly bootstrap in _start; placed after
   * the small-data sections so gp-relative accesses stay within range. */
  . = ALIGN(0x8);
  PROVIDE(__global_pointer$ = .);

  . = ALIGN(0x8);

  .bss : {
    PROVIDE(__bss_start = .);
    *(.sbss .sbss.*)
    *(.bss .bss.*)
    PROVIDE(__bss_end = .);
  }
}

/* End Of File ***************************************************************/

/* Copyright (C) Evo-Devo Instrum. All rights reserved ***********************/
