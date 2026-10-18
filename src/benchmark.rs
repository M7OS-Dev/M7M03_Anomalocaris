/******************************************************************************
Filename    : benchmark.rs
Author      : yhy
Date        : 14/10/2026
Licence     : The Unlicense; see LICENSE for details.
Description : The benchmark test for the RV64V port, mirroring the C
              rme_benchmark.c in M7M01_Eukaron/Source/Benchmark/.
              Tests the intra-process thread switching performance:
                - create a thread in the init process (THD_CRT)
                - bind it to the current core, with the init thread acting
                  as its scheduler (THD_SCHED_BIND)
                - set its entry point and stack (THD_EXEC_SET)
                - delegate some timeslice to it (THD_TIME_XFER)
                - measure the round-trip switch time of THD_SWT over 10000
                  iterations, using the RISC-V cycle counter as the TSC.
              Syscall ABI (mirror of the kernel's __RME_Svc_Param_Get):
                a0 = (Svc_Num << 16) | Cid
                a1 = Param0, a2 = Param1, a3 = Param2
                ecall
                return value comes back in a0
              Parameter packing follows the kernel's RME_PARAM_* convention
              (64-bit halves / 16-bit quarters / 8-bit eighths).
******************************************************************************/

/* Attribute *****************************************************************/
/* This module mirrors the whole C rme_benchmark.c. The same-process test and
 * the cross-process test are both wired up (the entry point runs the
 * cross-process test); dead-code warnings are suppressed on purpose for the
 * parts that mirror the original but are not exercised. */
#![allow(dead_code)]
/* End Attribute *************************************************************/

/* Define ********************************************************************/
/* The test objects - free capability slots after the RV64V boot caps
 * (0..8 are CPT/PGT/PRC/THD/KFN/KOM/VCT/PGT_L1/PGT_L2, see
 * rme_platform_rv64v.h). The init captbl now has RME_RV64V_INIT_CPT_SIZE
 * (64) entries, so slots 9..63 are free for the benchmark. */
const RME_BOOT_BENCH_THD: u64 = 9;
const RME_BOOT_BENCH_PGT_TOP: u64 = 10;
const RME_BOOT_BENCH_PGT_SRAM: u64 = 11;

/* Cross-process objects - each gets its own slot in the init captbl */
const RME_BOOT_BENCH_CPT: u64 = 12;
const RME_BOOT_BENCH_PGT_TOP_NEW: u64 = 13;
const RME_BOOT_BENCH_PGT_L1: u64 = 14;
const RME_BOOT_BENCH_PGT_L2: u64 = 15;
const RME_BOOT_BENCH_PRC: u64 = 16;
const RME_BOOT_BENCH_DIFF_THD: u64 = 17;

/* Slots inside the NEW process's own capability table. Per the RME manual,
 * every process has an independent capability-ID namespace: the cross-process
 * thread's cap lives in the new captbl (slot 0), and the init thread's cap is
 * delegated into the new captbl (slot 1) so the new thread can switch back. */
const RME_BENCH_NEW_CPT_THD: u64 = 0;
const RME_BENCH_NEW_CPT_INIT_THD: u64 = 1;

/* Kernel memory frontier for the newly created objects - relative to the
 * init KOM capability's Begin (RME_KOM_VA_BASE). The kernel's own boot-time
 * objects (init captbl, 3 Sv39 page tables, init thread) occupy roughly the
 * first 20KB of KOM, so the benchmark starts allocating at 0x8000 (32KB) to
 * stay clear of them. */
const RME_BOOT_BENCH_KOM_FRONTIER: u64 = 0x8000;

/* The stack safe size */
const RME_STACK_SAFE_SIZE: u64 = 16;

/* The benchmark loop count and the recorded sample count */
const RME_BENCH_LOOP_NUM: u64 = 10000;

/* TSC read overhead measured on the old ARM port - kept for symmetry with
 * the C benchmark which subtracts it from every sample. */
const RME_TSC_OVERHEAD: u64 = 8;

/* Sv39 page table geometry - every table has 512 entries, a top-level table
 * uses 1GB entries, the middle level 2MB and the leaf level 4KB. */
const RME_RV64V_PGT_NUM: u64 = 512;
/* Top-level index of the user window (0x20000000 >> 30) */
const RME_RV64V_TOP_POS: u64 = 0;
/* Middle-level index of the user window (0x20000000 >> 21) */
const RME_RV64V_L1_POS: u64 = 256;
/* The number of code/bss pages mapped for the init image (64KB) */
const RME_RV64V_IMAGE_PAGES: u64 = 16;
/* End Define ****************************************************************/

/* Include *******************************************************************/
use crate::syscall::{
    RME_BOOT_INIT_CPT, RME_BOOT_INIT_KFN, RME_BOOT_INIT_KOM, RME_BOOT_INIT_PGT_L2,
    RME_BOOT_INIT_PRC, RME_BOOT_INIT_THD, RME_CID_NULL, RME_KFN_DEBUG_PRINT, RME_PGT_ALL_DYN,
    RME_PGT_ALL_PERM, RME_PGT_NUM_512, RME_PGT_SIZE_1G, RME_PGT_SIZE_2M, RME_PGT_SIZE_4K,
    RME_SVC_CPT_ADD, RME_SVC_CPT_CRT, RME_SVC_KFN, RME_SVC_PGT_ADD, RME_SVC_PGT_CON,
    RME_SVC_PGT_CRT, RME_SVC_PRC_CRT, RME_SVC_THD_CRT, RME_SVC_THD_EXEC_SET,
    RME_SVC_THD_SCHED_BIND, RME_SVC_THD_SWT, RME_SVC_THD_TIME_XFER, RME_THD_FLAG_ALL,
    RME_THD_FLAG_SWT, rme_param_d0, rme_param_d1, rme_param_q0, rme_param_q1, rme_syscall,
};
/* End Include ***************************************************************/

/* Private Variables *********************************************************/
/* The stack of the threads - 2048 words as in the C benchmark (16KB on RV64) */
static mut RME_STACK: [u64; 2048] = [0; 2048];
/* The recorded switch time of each round-trip, in TSC ticks */
static mut RME_BENCH_TIME: [u16; 10000] = [0; 10000];
/* End Private Variables *****************************************************/

/* Function:rme_tsc ***********************************************************
Description : Read the timestamp counter. On RV64V this is the RISC-V cycle
              counter, the analog of the ARM TIM2->CNT used by the C
              benchmark - a free-running counter at CPU clock frequency.
Input       : None.
Output      : None.
Return      : u64 - The current cycle count.
******************************************************************************/
#[inline(always)]
fn rme_tsc() -> u64 {
    let tsc: u64;

    unsafe {
        core::arch::asm!(
            "csrr {0}, cycle",
            out(reg) tsc,
            options(nostack),
        );
    }

    tsc
}
/* End Function:rme_tsc ******************************************************/

/* Function:rme_print_char *****************************************************
Description : Print one character by invoking the DEBUG_PRINT kernel function.
              Param0 packs D0 = Func_ID (DEBUG_PRINT) and D1 = Sub_ID (the
              character), the same convention as the C init's Print_Char.
Input       : ch - The character to print.
Output      : None.
Return      : None.
******************************************************************************/
#[inline(always)]
fn rme_print_char(ch: u8) {
    /* Param0 = (Sub_ID << 32) | Func_ID */
    let param0: u64 = ((ch as u64) << 32) | RME_KFN_DEBUG_PRINT;

    let _ = rme_syscall(RME_SVC_KFN, RME_BOOT_INIT_KFN, param0, 0, 0);
}
/* End Function:rme_print_char ***********************************************/

/* Function:rme_print_str *****************************************************
Description : Print a byte slice, one syscall per character.
Input       : s - The characters to print.
Output      : None.
Return      : None.
******************************************************************************/
#[inline(always)]
fn rme_print_str(s: &[u8]) {
    for &ch in s {
        rme_print_char(ch);
    }
}
/* End Function:rme_print_str ************************************************/

/* Function:rme_print_uint ****************************************************
Description : Print an unsigned integer in decimal, mirroring the C
              RME_Sprint_Uint + debug print path.
Input       : value - The integer to print.
Output      : None.
Return      : None.
******************************************************************************/
fn rme_print_uint(value: u64) {
    /* Print the digits in reverse, then emit them in order */
    let mut buf: [u8; 20] = [0; 20];
    let mut len: usize = 0;
    let mut val: u64 = value;

    /* Print at least one digit even for zero */
    loop {
        buf[len] = b'0' + ((val % 10) as u8);
        len += 1;
        val /= 10;
        if val == 0 {
            break;
        }
    }

    while len > 0 {
        len -= 1;
        rme_print_char(buf[len]);
    }
}
/* End Function:rme_print_uint ***********************************************/

/* Function:rme_bench_report **************************************************
Description : Print a summary of the measured switch times: total, minimum,
              maximum and average in TSC ticks.
Input       : count - The number of valid samples in RME_BENCH_TIME.
Output      : None.
Return      : None.
******************************************************************************/
fn rme_bench_report(count: usize, title: &[u8]) {
    let mut sum: u64 = 0;
    let mut min: u64 = 0xFFFF_FFFF_FFFF_FFFF;
    let mut max: u64 = 0;
    let mut i: usize = 0;

    while i < count {
        let t = unsafe { RME_BENCH_TIME[i] } as u64;
        if t < min {
            min = t;
        }
        if t > max {
            max = t;
        }
        sum += t;
        i += 1;
    }

    rme_print_str(b"RME benchmark: ");
    rme_print_str(title);
    rme_print_str(b"\r\n");
    rme_print_str(b"  count  = ");
    rme_print_uint(count as u64);
    rme_print_str(b"\r\n");
    rme_print_str(b"  min    = ");
    rme_print_uint(min);
    rme_print_str(b" cycles\r\n");
    rme_print_str(b"  max    = ");
    rme_print_uint(max);
    rme_print_str(b" cycles\r\n");
    rme_print_str(b"  average= ");
    rme_print_uint(sum / (count as u64));
    rme_print_str(b" cycles\r\n");
}
/* End Function:rme_bench_report *********************************************/

/* Function:rme_cap_op ********************************************************
Description : The system call wrapper, mirroring the RME_CAP_OP macro of the
              C benchmark. rme_syscall() already packs (OP<<16)|CAPID into
              a0, matching the RV64V kernel ABI.
Input       : op - The system call number.
              capid - The capability ID.
              arg1 - The first system call parameter.
              arg2 - The second system call parameter.
              arg3 - The third system call parameter.
Output      : None.
Return      : RmePtr - The kernel return value (in a0).
******************************************************************************/
#[inline(always)]
fn rme_cap_op(op: u64, capid: u64, arg1: u64, arg2: u64, arg3: u64) -> u64 {
    rme_syscall(op, capid, arg1, arg2, arg3)
}
/* End Function:rme_cap_op ***************************************************/

/* Function:RME_Same_Prc_Thd_Switch_Test_Thd **********************************
Description : The thread for testing same-process thread switching
              performance. Immediately switches back to the init thread.
Input       : _param - The parameter passed by THD_EXEC_SET (unused).
Output      : None.
Return      : ! - This function never returns.
******************************************************************************/
extern "C" fn rme_same_prc_thd_switch_test_thd(_param: u64) -> ! {
    /* Now we switch back to the init thread, immediately */
    loop {
        let _ = rme_cap_op(RME_SVC_THD_SWT, 0, RME_BOOT_INIT_THD, 0, 0);
    }
}
/* End Function:RME_Same_Prc_Thd_Switch_Test_Thd *****************************/

/* Function:RME_Same_Prc_Thd_Switch_Test *************************************
Description : The same-process thread switch test code.
Input       : None.
Output      : None.
Return      : ! - This function never returns.
******************************************************************************/
pub fn rme_same_prc_thd_switch_test() -> ! {
    /* Intra-process thread switching time */
    let mut count: u64;
    let stack_addr: u64;
    let entry_addr: u64;

    /* Initialize the thread's stack - on RV64V the kernel sets PC/SP/A0
     * directly through THD_EXEC_SET, so no crafted return frame is needed,
     * just the top of a free stack area. The stack pointer must stay 16-byte
     * aligned for the RISC-V ABI. */
    stack_addr = unsafe { ((&RME_STACK[2047] as *const u64) as u64) & !0xF };

    /* There are still many bugs in the kernel. Need a white-box test to
     * guarantee that it is free of bugs. Find a scheme to do that */
    let _ = rme_cap_op(
        RME_SVC_THD_CRT,
        RME_BOOT_INIT_CPT,
        rme_param_d1(RME_BOOT_INIT_KOM) | rme_param_d0(RME_BOOT_BENCH_THD),
        rme_param_d1(RME_BOOT_INIT_PRC) | rme_param_d0(31),
        RME_BOOT_BENCH_KOM_FRONTIER,
    );

    /* Bind the thread to the processor - the init thread becomes its
     * scheduler. The priority must match the init thread's priority (0) so
     * that THD_SWT between the two threads is allowed. */
    let _ = rme_cap_op(
        RME_SVC_THD_SCHED_BIND,
        RME_BOOT_BENCH_THD,
        rme_param_d1(RME_BOOT_INIT_THD) | rme_param_d0(RME_CID_NULL),
        rme_param_d1(0) | rme_param_d0(0),
        0,
    );

    /* Set the execution information */
    entry_addr = rme_same_prc_thd_switch_test_thd as extern "C" fn(u64) -> ! as u64;
    let _ = rme_cap_op(
        RME_SVC_THD_EXEC_SET,
        RME_BOOT_BENCH_THD,
        entry_addr,
        stack_addr,
        0,
    );

    /* Delegate some timeslice to it */
    let _ = rme_cap_op(
        RME_SVC_THD_TIME_XFER,
        0,
        RME_BOOT_BENCH_THD,
        RME_BOOT_INIT_THD,
        10000000,
    );

    /* Try to switch to that thread - should fail */
    let _ = rme_cap_op(RME_SVC_THD_SWT, 0, RME_BOOT_BENCH_THD, 0, 0);

    count = 0;
    while count < RME_BENCH_LOOP_NUM {
        let temp = rme_tsc();
        let _ = rme_cap_op(RME_SVC_THD_SWT, 0, RME_BOOT_BENCH_THD, 0, 0);
        let delta = rme_tsc() - temp;

        /* Store the raw round-trip switch time, subtracting the TSC read
         * overhead measured on the reference platform */
        unsafe {
            RME_BENCH_TIME[count as usize] = (delta - RME_TSC_OVERHEAD) as u16;
        }

        count += 1;
    }

    /* Report the result */
    rme_bench_report(count as usize, b"same-process thread switch");

    /* Idle forever - busy loop, no WFI (illegal instruction on QEMU) */
    loop {}
}
/* End Function:RME_Same_Prc_Thd_Switch_Test ********************************/

/* Function:RME_Diff_Prc_Thd_Switch_Test_Thd *********************************
Description : The thread for testing different-process thread switching
              performance. It lives in the separate process created by
              RME_Diff_Prc_Thd_Switch_Test, but shares the init image's code
              and stack pages (the new process maps the same physical pages),
              so this thread behaves exactly like the same-process one.
Input       : _param - The parameter passed by THD_EXEC_SET (unused).
Output      : None.
Return      : ! - This function never returns.
******************************************************************************/
extern "C" fn rme_diff_prc_thd_switch_test_thd(_param: u64) -> ! {
    /* Switch back to the init thread, immediately. This thread runs in the
     * NEW process's capability namespace, where the init thread's cap was
     * delegated to RME_BENCH_NEW_CPT_INIT_THD - not the init captbl's slot. */
    loop {
        let _ = rme_cap_op(RME_SVC_THD_SWT, 0, RME_BENCH_NEW_CPT_INIT_THD, 0, 0);
    }
}
/* End Function:RME_Diff_Prc_Thd_Switch_Test_Thd ****************************/

/* Function:rme_pgt_crt *******************************************************
Description : Create a page table. On RV64V every table has 512 entries
              (Num_Order = 9). The Num_Order is packed into the high bits of
              the system call number (mirroring the kernel's RME_PARAM_PC).
Input       : cap_pgt - The capability slot for the new page table.
              raddr - The relative KOM address for the new table object.
              base - The virtual base address this table covers.
              is_top - Whether this is the top-level (root) table.
              size_order - The page size order (RME_PGT_SIZE_4K/2M/1G).
Output      : None.
Return      : u64 - The kernel return value (in a0).
******************************************************************************/
#[inline(always)]
fn rme_pgt_crt(cap_pgt: u64, raddr: u64, base: u64, is_top: u64, size_order: u64) -> u64 {
    /* Num_Order = 9 (2^9 = 512 entries), packed into the syscall number high
     * bits. NOTE: Num_Order is an EXPONENT, so RME_PGT_NUM_512 (9) must be
     * used - NOT RME_RV64V_PGT_NUM (512), which is the entry count. */
    let svc_num: u64 = RME_SVC_PGT_CRT | (RME_PGT_NUM_512 << 16);

    /* Param0 = D1(Cap_Kom) | Q1(Cap_Pgt) | Q0(Size_Order)
     * Param1 = Raddr
     * Param2 = Base | Is_Top (Is_Top in bit 0) */
    let param0: u64 =
        rme_param_d1(RME_BOOT_INIT_KOM) | rme_param_q1(cap_pgt) | rme_param_q0(size_order);

    rme_syscall(svc_num, RME_BOOT_INIT_CPT, param0, raddr, base | is_top)
}
/* End Function:rme_pgt_crt **************************************************/

/* Function:rme_pgt_con *******************************************************
Description : Map a child page table into the parent page table, building the
              address space tree.
Input       : cap_parent - The parent page table capability slot.
              pos - The parent table position to map the child to.
              cap_child - The child page table capability slot.
Output      : None.
Return      : u64 - The kernel return value (in a0).
******************************************************************************/
#[inline(always)]
fn rme_pgt_con(cap_parent: u64, pos: u64, cap_child: u64) -> u64 {
    /* Param0 = D1(Cap_Pgt_Parent) | D0(Cap_Pgt_Child)
     * Param1 = Pos
     * Param2 = Flag_Child */
    let param0: u64 = rme_param_d1(cap_parent) | rme_param_d0(cap_child);

    rme_syscall(
        RME_SVC_PGT_CON,
        RME_BOOT_INIT_CPT,
        param0,
        pos,
        RME_PGT_ALL_PERM,
    )
}
/* End Function:rme_pgt_con **************************************************/

/* Function:rme_pgt_add *******************************************************
Description : Delegate a page from one page table to another. Used to copy the
              init image's code/bss pages into the new process's leaf table,
              so the cross-process thread can execute the same code and use
              the same stack/buffers.
Input       : cap_dst - The destination page table capability slot.
              pos_dst - The destination table position.
              cap_src - The source page table capability slot.
              pos_src - The source table position.
Output      : None.
Return      : u64 - The kernel return value (in a0).
******************************************************************************/
#[inline(always)]
fn rme_pgt_add(cap_dst: u64, pos_dst: u64, cap_src: u64, pos_src: u64) -> u64 {
    /* Param0 = D1(Cap_Pgt_Dst) | D0(Pos_Dst)
     * Cid    = Flag_Dst (page permissions)
     * Param1 = D1(Cap_Pgt_Src) | D0(Pos_Src)
     * Param2 = Index (0: same granularity).
     * NOTE: Flag_Dst must be a SUBSET of the source page's permissions - the
     * init image pages are mapped without the STATIC bit, so RME_PGT_ALL_DYN
     * (no STATIC) must be passed instead of RME_PGT_ALL_PERM. */
    let param0: u64 = rme_param_d1(cap_dst) | rme_param_d0(pos_dst);
    let param1: u64 = rme_param_d1(cap_src) | rme_param_d0(pos_src);

    rme_syscall(RME_SVC_PGT_ADD, RME_PGT_ALL_DYN, param0, param1, 0)
}
/* End Function:rme_pgt_add **************************************************/

/* Function:RME_Diff_Prc_Thd_Switch_Test *************************************
Description : The different-process thread switch test code. Creates a second
              process with its own capability table, its own three-level Sv39
              page table (root + L1 + L2) and its own thread, then measures
              the round-trip switch time between the init thread and the new
              process's thread. Because the two threads live in different
              address spaces, every THD_SWT switches the satp, which is what
              this benchmark is really measuring.
              The new process shares the init image's physical code/bss pages
              (delegated through PGT_ADD), so the new thread can execute the
              same entry function and use the same static stack/buffers.
Input       : None.
Output      : None.
Return      : ! - This function never returns.
******************************************************************************/
pub fn rme_diff_prc_thd_switch_test() -> ! {
    /* Inter-process thread switching time */
    let mut count: u64;
    let stack_addr: u64;
    let entry_addr: u64;
    let mut raddr: u64;

    /* Initialize the thread's stack - shares the init image's bss pages, so
     * the same VA works in the new process's address space */
    stack_addr = unsafe { ((&RME_STACK[2047] as *const u64) as u64) & !0xF };

    /* ---- Create the new process's capability table (16 entries) ---- */
    /* CPT_CRT: Cid=Cap_Cpt_Crt, Param0=D1(Cap_Kom)|D0(Cap_Crt),
     * Param1=Raddr, Param2=Entry_Num */
    raddr = RME_BOOT_BENCH_KOM_FRONTIER;
    let _ = rme_cap_op(
        RME_SVC_CPT_CRT,
        RME_BOOT_INIT_CPT,
        rme_param_d1(RME_BOOT_INIT_KOM) | rme_param_d0(RME_BOOT_BENCH_CPT),
        raddr,
        16,
    );
    raddr += 0x400;

    /* ---- Create the new process's page tables (root + L1 + L2) ---- */
    /* Root: 1GB entries, covers the low half. Raddr must be 4KB aligned
     * because Sv39 finds a table through its PPN. */
    raddr = (raddr + 0xFFF) & !0xFFF;
    let _ = rme_pgt_crt(RME_BOOT_BENCH_PGT_TOP_NEW, raddr, 0, 1, RME_PGT_SIZE_1G);
    raddr += 0x1000;
    /* L1: 2MB entries, covers the user window [0,1GB) */
    let _ = rme_pgt_crt(RME_BOOT_BENCH_PGT_L1, raddr, 0, 0, RME_PGT_SIZE_2M);
    raddr += 0x1000;
    /* L2: 4KB entries, covers [0x20000000,0x20200000) */
    let _ = rme_pgt_crt(RME_BOOT_BENCH_PGT_L2, raddr, 0x20000000, 0, RME_PGT_SIZE_4K);
    raddr += 0x1000;

    /* ---- Connect the tables (build the address space tree) ---- */
    /* root[0] -> L1 (0x20000000>>30 = 0) */
    let _ = rme_pgt_con(
        RME_BOOT_BENCH_PGT_TOP_NEW,
        RME_RV64V_TOP_POS,
        RME_BOOT_BENCH_PGT_L1,
    );
    /* L1[256] -> L2 (0x20000000>>21 = 256) */
    let _ = rme_pgt_con(
        RME_BOOT_BENCH_PGT_L1,
        RME_RV64V_L1_POS,
        RME_BOOT_BENCH_PGT_L2,
    );

    /* ---- Delegate the init image's code/bss pages into the new L2 ---- */
    /* Copy pages 0..15 (VA 0x20000000..0x20010000) from the init L2 table */
    count = 0;
    while count < RME_RV64V_IMAGE_PAGES {
        let _ = rme_pgt_add(RME_BOOT_BENCH_PGT_L2, count, RME_BOOT_INIT_PGT_L2, count);
        count += 1;
    }

    /* ---- Create the new process ---- */
    /* PRC_CRT: Cid=Cap_Cpt_Crt, Param0=Cap_Prc, Param1=Cap_Cpt, Param2=Cap_Pgt.
     * The new process gets its OWN capability table (RME_BOOT_BENCH_CPT) and
     * its OWN page table, per the RME manual. */
    let _ = rme_cap_op(
        RME_SVC_PRC_CRT,
        RME_BOOT_INIT_CPT,
        RME_BOOT_BENCH_PRC,
        RME_BOOT_BENCH_CPT,
        RME_BOOT_BENCH_PGT_TOP_NEW,
    );

    /* ---- Create the thread in the new process's OWN captbl ---- */
    /* THD_CRT: Cid=Cap_Cpt, Param0=D1(Cap_Kom)|D0(Cap_Thd),
     * Param1=D1(Cap_Prc)|D0(Prio_Max), Param2=Raddr.
     * Cid now points at the NEW captbl, so the new thread's cap slot (0) is
     * created in the new process's independent capability namespace. */
    raddr = (raddr + 0xFFF) & !0xFFF;
    let _ = rme_cap_op(
        RME_SVC_THD_CRT,
        RME_BOOT_BENCH_CPT,
        rme_param_d1(RME_BOOT_INIT_KOM) | rme_param_d0(RME_BENCH_NEW_CPT_THD),
        rme_param_d1(RME_BOOT_BENCH_PRC) | rme_param_d0(31),
        raddr,
    );

    /* ---- Delegate caps between the two independent captbls ---- */
    /* CPT_ADD: Param0=D1(Cap_Cpt_Dst)|D0(Cap_Dst),
     * Param1=D1(Cap_Cpt_Src)|D0(Cap_Src), Param2=Flag.
     *
     * 1) Delegate the new thread cap (new captbl slot 0) into the init captbl
     *    slot RME_BOOT_BENCH_DIFF_THD, so the init thread can bind it, set its
     *    execution info, transfer time and switch to it.
     * 2) Delegate the init thread cap (init captbl slot 3) into the new captbl
     *    slot RME_BENCH_NEW_CPT_INIT_THD, so the cross-process thread can
     *    switch back to the init thread from inside its own namespace. */
    let _ = rme_cap_op(
        RME_SVC_CPT_ADD,
        RME_BOOT_INIT_CPT,
        rme_param_d1(RME_BOOT_INIT_CPT) | rme_param_d0(RME_BOOT_BENCH_DIFF_THD),
        rme_param_d1(RME_BOOT_BENCH_CPT) | rme_param_d0(RME_BENCH_NEW_CPT_THD),
        RME_THD_FLAG_ALL,
    );
    let _ = rme_cap_op(
        RME_SVC_CPT_ADD,
        RME_BOOT_INIT_CPT,
        rme_param_d1(RME_BOOT_BENCH_CPT) | rme_param_d0(RME_BENCH_NEW_CPT_INIT_THD),
        rme_param_d1(RME_BOOT_INIT_CPT) | rme_param_d0(RME_BOOT_INIT_THD),
        /* Only SWT: the boot init-thread cap lacks EXEC_SET/SCHED_CHILD/
         * SCHED_FREE, so RME_THD_FLAG_ALL is not a valid subset and would be
         * rejected with RME_ERR_CPT_FLAG. The cross-process thread only ever
         * switches back, so SWT alone is both sufficient and correct. */
        RME_THD_FLAG_SWT,
    );

    /* Bind the thread to the processor - from the init side via the delegated
     * cap in the init captbl */
    let _ = rme_cap_op(
        RME_SVC_THD_SCHED_BIND,
        RME_BOOT_BENCH_DIFF_THD,
        rme_param_d1(RME_BOOT_INIT_THD) | rme_param_d0(RME_CID_NULL),
        rme_param_d1(0) | rme_param_d0(0),
        0,
    );

    /* Set the execution information */
    entry_addr = rme_diff_prc_thd_switch_test_thd as extern "C" fn(u64) -> ! as u64;
    let _ = rme_cap_op(
        RME_SVC_THD_EXEC_SET,
        RME_BOOT_BENCH_DIFF_THD,
        entry_addr,
        stack_addr,
        0,
    );

    /* Delegate some timeslice to it */
    let _ = rme_cap_op(
        RME_SVC_THD_TIME_XFER,
        0,
        RME_BOOT_BENCH_DIFF_THD,
        RME_BOOT_INIT_THD,
        10000000,
    );

    /* Try to switch to that thread - should fail */
    let _ = rme_cap_op(RME_SVC_THD_SWT, 0, RME_BOOT_BENCH_DIFF_THD, 0, 0);

    count = 0;
    while count < RME_BENCH_LOOP_NUM {
        let temp = rme_tsc();
        let _ = rme_cap_op(RME_SVC_THD_SWT, 0, RME_BOOT_BENCH_DIFF_THD, 0, 0);
        let delta = rme_tsc() - temp;

        unsafe {
            RME_BENCH_TIME[count as usize] = (delta - RME_TSC_OVERHEAD) as u16;
        }

        count += 1;
    }

    /* Report the result */
    rme_bench_report(count as usize, b"different-process thread switch");

    /* Idle forever - busy loop, no WFI (illegal instruction on QEMU) */
    loop {}
}
/* End Function:RME_Diff_Prc_Thd_Switch_Test ********************************/

/* Function:RME_Benchmark *****************************************************
Description : The benchmark entry, also the init thread.
Input       : None.
Output      : None.
Return      : ! - This function never returns.
******************************************************************************/
pub fn rme_benchmark() -> ! {
    // rme_same_prc_thd_switch_test()
    rme_diff_prc_thd_switch_test()
}
/* End Function:RME_Benchmark ************************************************/

/* End Of File ***************************************************************/

/* Copyright (C) Evo-Devo Instrum. All rights reserved ***********************/
