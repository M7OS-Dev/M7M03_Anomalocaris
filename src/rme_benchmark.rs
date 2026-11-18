/******************************************************************************
Filename    : benchmark.rs
Author      : yhy
Date        : 08/11/2026
Licence     : The Unlicense; see LICENSE for details.
Description : The benchmark framework and all benchmark tests for the RV64V
              port of RME, written in Rust for the user-mode init program.
              The framework rules are:
                - success paths only;
                - every test has prepare / measurement / cleanup;
                - the first negative return stops the current test;
                - a failed test is reported and the next test still runs;
                - no coverage mask is maintained or printed.
              The test list mirrors the C benchmark and the Ammonite scenario
              list (M7M02_Ammonite/Include/Test/rvm_benchmark.h):
                - Yield (self, one-way)    THD_SWT onto the calling thread
                - Signal (self)            SIG_SND + SIG_RCV, self to self
                - Invocation (round-trip)  INV_ACT + INV_RET
                - Signal (intra)            SIG_SND + SIG_RCV inside a process
                - Signal (inter)            SIG_SND + SIG_RCV across processes
                - Yield (inter, one-way)   THD_SWT into another process
              The topology mirrors the C benchmark as well: the init process
              runs the measurement, and process 2 is a second process created
              by the benchmark itself. Both processes share the init image's
              physical pages, so the very same Rust code runs in either of
              them.
              Two properties of the kernel shape the whole design:
              1. A boot-time thread may not block on a signal endpoint at
                 all, so the init thread can only ever be a sender. The
                 receiving side of every signal test is a helper thread.
              2. A thread switch to a designated thread does not rotate the
                 run queue, so the init thread stays the head of its priority
                 level and is always picked first when a higher priority
                 thread goes to sleep. A helper that has to be woken up
                 again after it let the init thread run therefore has to sit
                 on a higher priority level than the init thread.
              The helper threads are consequently laid out like this:
                - The init thread (priority 0) is the coordinator, the
                  sender of the signal tests and the reporter.
                - The helper thread of the init process (priority 1) receives
                  the signals of the intra-process test.
                - The helper thread of process 2 (priority 2) receives the
                  signals of the inter-process test.
                - The switch target of the init process (priority 0) is the
                  target of the intra-process yield test, and the switch
                  target of process 2 (priority 0) is the target of the
                  inter-process yield test, because a thread switch needs the
                  very same priority on both sides.
              Syscall ABI (mirror of the kernel's __RME_Svc_Param_Get):
                a0 = (Svc << 32) | Cid
                a1 = Param0, a2 = Param1, a3 = Param2
                ecall
                return value comes back in a0; some calls also use a1
*****************************************************************************/

/* Attribute *****************************************************************/
/* This module mirrors the whole C rme_benchmark.c. Not every constant is
 * used by every platform, so dead-code warnings are suppressed on purpose. */
#![allow(dead_code)]
/* End Attribute *************************************************************/

/* Define ********************************************************************/
/* Rounds for the normal tests; AVG/MAX/MIN are computed over these rounds. */
const RME_BENCH_LOOP_NUM: u64 = 10000;

/* Reduced rounds for the wall-clock/console-bound tests: IDLE_SLEEP (one 10ms
 * tick per round) and DEBUG_PRINT (one console character per round). Every
 * other test uses RME_BENCH_LOOP_NUM. */
const RME_BENCH_LOOP_REDUCED_NUM: u64 = 100;

/* The number of words reserved for each helper thread's stack. */
const RME_BENCH_STACK_WORD_NUM: usize = 512;

/* Capability slots inside the init process' capability table. The table has
 * 64 entries and the boot code consumes slots 0..9, so the benchmark starts
 * at slot 10. */
const RME_BENCH_CAP_THD: rme_ptr_t = 10; /* The helper thread of the init process */
const RME_BENCH_CAP_CPT: rme_ptr_t = 13; /* The capability table of process 2 */
const RME_BENCH_CAP_PGT_TOP_NEW: rme_ptr_t = 14; /* The root page table of process 2 */
const RME_BENCH_CAP_PGT_L1: rme_ptr_t = 15; /* The second level page table of process 2 */
const RME_BENCH_CAP_PGT_L2: rme_ptr_t = 16; /* The third level page table of process 2 */
const RME_BENCH_CAP_PRC: rme_ptr_t = 17; /* Process 2 itself */
const RME_BENCH_CAP_P2_THD: rme_ptr_t = 18; /* The switch target of process 2 */
const RME_BENCH_CAP_SIG_SELF: rme_ptr_t = 19; /* The self-to-self signal endpoint */
const RME_BENCH_CAP_SIG_PARK: rme_ptr_t = 20; /* The parking endpoint of the init process */
const RME_BENCH_CAP_SIG_DATA: rme_ptr_t = 21; /* The data channel signal endpoint */
const RME_BENCH_CAP_SIG_PARK2: rme_ptr_t = 22; /* The parking endpoint of process 2 */
const RME_BENCH_CAP_INV: rme_ptr_t = 23; /* The invocation stub */
const RME_BENCH_CAP_THD2: rme_ptr_t = 24; /* The switch target of the init process */
const RME_BENCH_CAP_INV2: rme_ptr_t = 59; /* The second invocation stub */
const RME_BENCH_CAP_SIG_SND_ONLY: rme_ptr_t = 60; /* Send-only signal endpoint */
const RME_BENCH_CAP_SIG_RCV_ONLY: rme_ptr_t = 61; /* Receive-only signal endpoint */

/* Capability slots inside process 2's own capability table. Every process
 * owns an independent capability ID namespace, so the capabilities that
 * process 2 needs have to be delegated into its table one by one. */
const RME_BENCH_CAP2_THD: rme_ptr_t = 0; /* The receiver thread of process 2 */
const RME_BENCH_CAP2_INIT_THD: rme_ptr_t = 1; /* The init thread, delegated */
const RME_BENCH_CAP2_SIG_DATA: rme_ptr_t = 2; /* The data channel endpoint, delegated */
const RME_BENCH_CAP2_SIG_PARK: rme_ptr_t = 3; /* The parking endpoint, delegated */
const RME_BENCH_CAP2_THD2: rme_ptr_t = 4; /* The switch target thread of process 2 */
/* Slot 5 is unused. */
const RME_BENCH_CAP2_KFN: rme_ptr_t = 6; /* The KFN capability, used by DEBUG_INV_MOD */

/* The preemptive priorities the benchmark uses. The init thread runs at
 * priority 0, and a thread can only be switched to with THD_SWT when it
 * has the very same priority. */
const RME_BENCH_PRIO_INIT: rme_ptr_t = 0;
const RME_BENCH_PRIO_RECV: rme_ptr_t = 1; /* One level above the init thread */
const RME_BENCH_PRIO_RECV2: rme_ptr_t = 2; /* Two levels above the init thread */

/* The phases the helper threads dispatch on. The phase is published by the
 * measuring thread through a shared variable before it wakes the helper
 * thread up with a signal on its parking endpoint. */
const RME_BENCH_PHASE_IDLE: u32 = 0;
const RME_BENCH_PHASE_SIG_SELF: u32 = 1; /* Process 1's helper signals itself */
const RME_BENCH_PHASE_SIG_RECV: u32 = 2; /* Process 1's helper receives and measures */
const RME_BENCH_PHASE_SIG_RECV2: u32 = 3; /* Process 2's helper receives and measures */
const RME_BENCH_PHASE_SIG_RCV_ONLY: u32 = 4; /* Process 1's helper measures receive only */

/* The kernel memory frontier for the objects the benchmark creates. The
 * embedded init image is outside KOM. Start above the reserved boot objects.
 * Every kernel object the benchmark creates is carved out of this region by
 * the bump allocator: the second process, its page tables, the helper
 * threads and the invocation stubs. One frontier is enough, because the bump
 * pointer hands out a distinct range to every object. */
const RME_BENCH_KOM_FRONTIER: rme_ptr_t = 0x60000;

/* The alignment every kernel object needs inside the KOM. The KOM hands out
 * its objects in 64 byte slots (RME_KOM_SLOT_ORDER = 6), and _RME_Kot_Mark
 * rejects an address that is not slot aligned. */
const RME_BENCH_KOM_ALIGN: rme_ptr_t = 64;

/* A page table needs 4KB alignment on top of that: Sv39 finds the table
 * through its PPN, so the kernel rejects a misaligned one (__RME_Pgt_Check
 * tests Vaddr & 0xFFF). */
const RME_BENCH_PGT_ALIGN: rme_ptr_t = 0x1000;

/* The sizes of the kernel objects the benchmark creates. The kernel marks
 * exactly its own size in the KOT bitmap (_RME_Kot_Mark), so a request below
 * the real size of the object is reported as RME_ERR_KOT_BMP at the next
 * allocation. The margins below are headroom against a kernel-side growth:
 *   PGT : RME_PGT_SIZE_NOM(9) = 512 entries x 8 B = 4096
 *   THD : RME_THD_SIZE        = 728
 *   INV : RME_INV_SIZE        = 72
 * The size of a capability table is derived from its entry count instead,
 * see RME_Bench_Kom_Alloc_Cpt.
 * The thread object holds the register context at offset 176 and the FPU
 * context at offset 464, so it must stay 8 byte aligned for the FSD of the
 * coprocessor save; the 64 byte slot alignment covers that. */
const RME_BENCH_PGT_SIZE: rme_ptr_t = 0x1000;
const RME_BENCH_THD_SIZE: rme_ptr_t = 0x2000;
const RME_BENCH_INV_SIZE: rme_ptr_t = 0x1000;

/* The number of timeslices the measuring thread donates to a helper thread.
 * It is deliberately large enough that a helper never runs out of time
 * during a test, and small enough that the measuring thread keeps a healthy
 * share of the CPU. */
const RME_BENCH_TIME_XFER: rme_ptr_t = 10000000;

/* Sv39 page table geometry - every table has 512 entries, a top-level table
 * uses 1GB entries, the middle level 2MB and the leaf level 4KB. */
const RME_RV64V_PGT_NUM: u64 = 512;
/* Top-level index of the user window (0x20000000 >> 30) */
const RME_RV64V_TOP_POS: u64 = 0;
/* Middle-level index of the user window (0x20000000 >> 21) */
const RME_RV64V_L1_POS: u64 = 256;
/* The number of code/bss pages mapped for the init image (128KB) */
const RME_RV64V_IMAGE_PAGES: u64 = 32;

/* End Define ****************************************************************/

/* Include *******************************************************************/
use core::fmt::Write;

use crate::rme_syscall::{
    RME_BOOT_INIT_CPT, RME_BOOT_INIT_KFN, RME_BOOT_INIT_KOM, RME_BOOT_INIT_PGT,
    RME_BOOT_INIT_PGT_L2, RME_BOOT_INIT_PRC, RME_BOOT_INIT_THD, RME_BOOT_INIT_THD_SLOT,
    RME_BOOT_TBL_THD, RME_CID_Build, RME_CID_NULL, RME_Cpt_Add, RME_Cpt_Crt, RME_Cpt_Frz,
    RME_Inv_Act, RME_Inv_Crt, RME_Inv_Set, RME_KFN_CACHE_MAINT,
    RME_KFN_DEBUG_EXC_GET, RME_KFN_DEBUG_INV_MOD, RME_KFN_DEBUG_PRINT, RME_KFN_DEBUG_REG_MOD,
    RME_KFN_FLAG_FULL_RANGE, RME_KFN_IDLE_SLEEP, RME_KFN_PGT_ASID_SET, RME_KFN_PGT_CACHE_CLR,
    RME_KFN_PGT_ENTRY_MOD, RME_KFN_PGT_LINE_CLR, RME_Kfn, RME_PGT_ALL_DYN, RME_PGT_ALL_PERM,
    RME_PGT_SIZE_1G, RME_PGT_SIZE_2M, RME_PGT_SIZE_4K, RME_Pgt_Add, RME_Pgt_Con, RME_Pgt_Crt,
    RME_Prc_Crt, RME_RCV_BM, RME_RCV_NS, RME_RV64V_CACHE_BLOCK_SIZE,
    RME_RV64V_KFN_CACHE_CLEAN_ADDR, RME_RV64V_KFN_CACHE_CLEAN_INV_ADDR, RME_RV64V_KFN_CACHE_DCACHE,
    RME_RV64V_KFN_CACHE_ICACHE, RME_RV64V_KFN_CACHE_INV_ADDR, RME_RV64V_KFN_CACHE_INV_ALL,
    RME_RV64V_KFN_DEBUG_EXC_ADDR_GET, RME_RV64V_KFN_DEBUG_EXC_CAUSE_GET,
    RME_RV64V_KFN_DEBUG_EXC_VALUE_GET, RME_RV64V_KFN_DEBUG_INV_MOD_SP_GET,
    RME_RV64V_KFN_DEBUG_REG_MOD_X10_A0, RME_RV64V_KFN_PGT_ENTRY_MOD_FLAG_GET,
    RME_RV64V_KFN_PGT_ENTRY_MOD_NMORD_GET, RME_RV64V_KFN_PGT_ENTRY_MOD_SZORD_GET,
    RME_RV64V_SATP_ASID_MAX, RME_SIG_FLAG_ALL, RME_Sig_Crt, RME_Sig_Del, RME_Sig_Rcv, RME_Sig_Snd,
    RME_THD_FLAG_ALL, RME_THD_FLAG_SWT, RME_Thd_Crt, RME_Thd_Exec_Set, RME_Thd_Sched_Bind,
    RME_Thd_Swt, RME_Thd_Time_Xfer, rme_ptr_t,
};
/* End Include ***************************************************************/

/* Private Variables *********************************************************/
/* The stacks of the helper threads and of the invocation. They live in the
 * init image's bss, which the second process maps physically one to one, so
 * the very same addresses are valid in both processes. */
static mut RME_Bench_Stack_P1T2: [u64; RME_BENCH_STACK_WORD_NUM] = [0; RME_BENCH_STACK_WORD_NUM];
static mut RME_Bench_Stack_P1T3: [u64; RME_BENCH_STACK_WORD_NUM] = [0; RME_BENCH_STACK_WORD_NUM];
static mut RME_Bench_Stack_P2T1: [u64; RME_BENCH_STACK_WORD_NUM] = [0; RME_BENCH_STACK_WORD_NUM];
static mut RME_Bench_Stack_P2T2: [u64; RME_BENCH_STACK_WORD_NUM] = [0; RME_BENCH_STACK_WORD_NUM];
static mut RME_Bench_Stack_INV: [u64; RME_BENCH_STACK_WORD_NUM] = [0; RME_BENCH_STACK_WORD_NUM];
static mut RME_Bench_Stack_INV2: [u64; RME_BENCH_STACK_WORD_NUM] = [0; RME_BENCH_STACK_WORD_NUM];
static mut RME_Bench_Stack_TEST: [u64; RME_BENCH_STACK_WORD_NUM] = [0; RME_BENCH_STACK_WORD_NUM];

/* The shared phase flag, published by the measuring thread and observed by
 * the helper threads. */
static mut RME_Bench_Phase: u32 = RME_BENCH_PHASE_IDLE;

/* The timestamp the sender latches before it hands the workload over, and
 * the timestamp the receiver latches when the workload arrives. */
static mut RME_Bench_Start: u64 = 0;
static mut RME_Bench_End: u64 = 0;

/* The statistics the receiving side accumulates. They live in the shared
 * image, so the measuring thread can pick them up once the test is over. */
static mut RME_Bench_Stat_Total: u64 = 0;
static mut RME_Bench_Stat_Max: u32 = 0;
static mut RME_Bench_Stat_Min: u32 = 0;
static mut RME_Bench_Stat_Cnt: u64 = 0;

/* The return value of the timed system call on the shared/helper path.
 * Direct tests keep it in RME_Bench_Stat; helper threads write this shared slot and
 * RME_Bench_Stat::load_shared copies it back. */
static mut RME_Bench_Stat_Ret: i64 = 0;

/* Number of rounds whose timed call returned a negative value. Nonzero stops
 * the current test and prevents the failed sample from entering the stats. */
static mut RME_Bench_Stat_Fail: u64 = 0;

/* The return value of the DEBUG_INV_MOD call made inside the invocation stub.
 * The stub runs in process 2's capability namespace, the invoker reads this
 * shared slot after the invocation returns. */
static mut RME_Bench_Kfn_Inv_Ret: i64 = 0;

/* The capability IDs the helper threads need. They are resolved once during
 * the preparation, because a capability ID cannot be derived from a slot
 * number that lives in another capability table. */
static mut RME_Bench_Cap_P1T3: rme_ptr_t = 0;
static mut RME_Bench_Cap_P2_THD: rme_ptr_t = 0;
static mut RME_Bench_Cap_P1T1_In_P2: rme_ptr_t = 0;
static mut RME_Bench_Cap_Sig_Self: rme_ptr_t = 0;
static mut RME_Bench_Cap_Sig_Park: rme_ptr_t = 0;
static mut RME_Bench_Cap_Sig_Park2: rme_ptr_t = 0;
static mut RME_Bench_Cap_Sig_Data: rme_ptr_t = 0;

/* The monotonic KOM allocator. Every kernel object the benchmark creates is
 * carved out of this bump pointer, and each fixture consumes one distinct
 * range for the lifetime of the run. */
static mut RME_Bench_Kom_Next: rme_ptr_t = RME_BENCH_KOM_FRONTIER;

/* The caller-owned ASID allocator. The kernel gives the init address space
 * the smallest ASID and the KFN refuses it, so the benchmark starts above it. */
static mut RME_Bench_Asid_Next: rme_ptr_t = 2;
/* The ASID assigned to process 2's root page table. */
static mut RME_Bench_Asid_P2: rme_ptr_t = 0;
/* End Private Variables *****************************************************/

/* Function:RME_Bench_Read_Cycle ***************************************************
Description : Read the RISC-V cycle counter. "TSC" is x86 terminology, so the
              function is named after the CSR it actually reads. The cycle CSR
              is a free-running counter at CPU clock frequency.
Input       : None.
Output      : None.
Return      : u64 - The current cycle count.
*****************************************************************************/
#[inline(always)]
fn RME_Bench_Read_Cycle() -> u64 {
    let Cycle: u64;

    unsafe {
        core::arch::asm!(
            "csrr {0}, cycle",
            out(reg) Cycle,
            options(nostack),
        );
    }

    Cycle
}
/* End Function:RME_Bench_Read_Cycle ***********************************************/

/* Function:RME_Bench_Read_Time ****************************************************
Description : Read the RISC-V wall-clock time counter. Unlike the cycle CSR,
              the time CSR runs from an always-on timebase and keeps counting
              while the hart is stopped in WFI, so it is the correct instrument
              for measurements that span a sleep. The tick rate is platform
              specific: QEMU virt uses 10 MHz, SpacemiT K3 uses 24 MHz.
Input       : None.
Output      : None.
Return      : u64 - The current time count (platform timebase ticks).
*****************************************************************************/
#[inline(always)]
fn RME_Bench_Read_Time() -> u64 {
    let Time: u64;

    unsafe {
        core::arch::asm!(
            "csrr {0}, time",
            out(reg) Time,
            options(nostack),
        );
    }

    Time
}
/* End Function:RME_Bench_Read_Time ***********************************************/

/* Function:RME_Bench_Kom_Alloc **************************************************
Description : Allocate a distinct KOM range for a kernel object. The address is
              relative to the KOM capability's Begin, as the kernel expects.
              KOM is never reclaimed here; the bump pointer is enough.
              The size must cover what the kernel writes into the object: the
              kernel marks exactly its own size in the KOT bitmap, so a size
              below that is not reported here but at the next allocation, as
              RME_ERR_CPT_KOT. The alignment must be a power of two.
Input       : Size - The number of bytes needed.
              Align - The required alignment.
Output      : None.
Return      : rme_ptr_t - The aligned relative KOM address.
*****************************************************************************/
#[inline(always)]
fn RME_Bench_Kom_Alloc(Size: rme_ptr_t, Align: rme_ptr_t) -> rme_ptr_t {
    unsafe {
        /* The address is rounded up to the requested alignment, and the bump
         * pointer moves on by the size rounded up to the 64 byte slot of the
         * KOM, so that the next object never overlaps this one. */
        let Addr = (RME_Bench_Kom_Next + Align - 1) & !(Align - 1);
        RME_Bench_Kom_Next = Addr + ((Size + RME_BENCH_KOM_ALIGN - 1) & !(RME_BENCH_KOM_ALIGN - 1));
        Addr
    }
}
/* End Function:RME_Bench_Kom_Alloc **********************************************/

/* Function:RME_Bench_Kom_Alloc_Cpt *********************************************
Description : Allocate the kernel object of a capability table with the given
              number of entries. The size is derived from the count, so the
              object is never smaller than what the kernel writes into it.
              NOTE: the same count has to be passed to CPT_CRT - the kernel
              writes exactly Entry_Num slots into the object, so the two
              numbers must agree.
Input       : Entry_Num - The number of entries of the table.
Output      : None.
Return      : rme_ptr_t - The relative KOM address of the new table.
*****************************************************************************/
#[inline(always)]
fn RME_Bench_Kom_Alloc_Cpt(Entry_Num: rme_ptr_t) -> rme_ptr_t {
    /* One entry is 64 bytes: Cap_Head (5 words) + Info[3]. */
    RME_Bench_Kom_Alloc(Entry_Num * 64, RME_BENCH_KOM_ALIGN)
}
/* End Function:RME_Bench_Kom_Alloc_Cpt *****************************************/

/* Function:RME_Bench_Kom_Alloc_Pgt *********************************************
Description : Allocate the kernel object of a page table.
Input       : None.
Output      : None.
Return      : rme_ptr_t - The relative KOM address of the new table.
*****************************************************************************/
#[inline(always)]
fn RME_Bench_Kom_Alloc_Pgt() -> rme_ptr_t {
    RME_Bench_Kom_Alloc(RME_BENCH_PGT_SIZE, RME_BENCH_PGT_ALIGN)
}
/* End Function:RME_Bench_Kom_Alloc_Pgt *****************************************/

/* Function:RME_Bench_Kom_Alloc_Thd *********************************************
Description : Allocate the kernel object of a thread.
Input       : None.
Output      : None.
Return      : rme_ptr_t - The relative KOM address of the new thread.
*****************************************************************************/
#[inline(always)]
fn RME_Bench_Kom_Alloc_Thd() -> rme_ptr_t {
    RME_Bench_Kom_Alloc(RME_BENCH_THD_SIZE, RME_BENCH_KOM_ALIGN)
}
/* End Function:RME_Bench_Kom_Alloc_Thd *****************************************/

/* Function:RME_Bench_Kom_Alloc_Inv *********************************************
Description : Allocate the kernel object of an invocation stub.
Input       : None.
Output      : None.
Return      : rme_ptr_t - The relative KOM address of the new stub.
*****************************************************************************/
#[inline(always)]
fn RME_Bench_Kom_Alloc_Inv() -> rme_ptr_t {
    RME_Bench_Kom_Alloc(RME_BENCH_INV_SIZE, RME_BENCH_KOM_ALIGN)
}
/* End Function:RME_Bench_Kom_Alloc_Inv *****************************************/

/* Function:RME_Bench_Print_Char ***************************************************
Description : Print one character by invoking the DEBUG_PRINT kernel function.
              Param0 packs D0 = Func_ID (DEBUG_PRINT) and D1 = Sub_ID (the
              character), the same convention as the C init's Print_Char.
Input       : Ch - The character to print.
Output      : None.
Return      : None.
*****************************************************************************/
#[inline(always)]
pub fn RME_Bench_Print_Char(Ch: u8) {
    let _ = RME_Kfn(
        RME_BOOT_INIT_KFN,
        RME_KFN_DEBUG_PRINT,
        Ch as rme_ptr_t,
        0,
        0,
    );
}
/* End Function:RME_Bench_Print_Char ***********************************************/

/* Function:RME_Bench_Print_Str ****************************************************
Description : Print a string, one syscall per character.
Input       : S - The string to print.
Output      : None.
Return      : None.
*****************************************************************************/
#[inline(always)]
fn RME_Bench_Print_Str(S: &str) {
    for &Ch in S.as_bytes() {
        RME_Bench_Print_Char(Ch);
    }
}
/* End Function:RME_Bench_Print_Str ************************************************/

/* Type:RME_Bench_Writer **********************************************************
Description : A core::fmt::Write implementation that forwards formatted output
              to the KFN debug-print path. This makes write!/format_args!
              usable without an allocator; format! itself would need String.
*****************************************************************************/
struct RME_Bench_Writer;

impl Write for RME_Bench_Writer {
    fn write_str(&mut self, S: &str) -> core::fmt::Result {
        RME_Bench_Print_Str(S);
        Ok(())
    }
}
/* End Type:RME_Bench_Writer ******************************************************/

/* Macro:RME_BENCH_PRINT *********************************************************
Description : Formatted printing without allocation. Uses format_args! and the
              RME_Bench_Writer, not format! (which requires an allocator).
*****************************************************************************/
macro_rules! RME_BENCH_PRINT {
    ($($arg:tt)*) => {{
        let _ = RME_Bench_Writer.write_fmt(format_args!($($arg)*));
    }};
}
/* End Macro:RME_BENCH_PRINT *****************************************************/

/* Function:RME_Bench_Setup_Check ************************************************
Description : Check the result of one setup system call of the global
              preparation stage. A failed setup is fatal: the error is printed
              and the benchmark stops right there, because the measurement
              environment is broken and every test that follows would only
              produce misleading numbers. The program spins forever instead of
              returning, because a user-mode thread cannot be aborted from
              here.
Input       : Label - The setup step name.
              Ret - The value the setup system call returned.
Output      : None.
Return      : None.
*****************************************************************************/
fn RME_Bench_Setup_Check(Label: &str, Ret: rme_ptr_t) {
    let Signed = Ret as i64;

    if Signed < 0 {
        RME_BENCH_PRINT!("bench setup failed: {} ret={}\r\n", Label, Signed);
        RME_BENCH_PRINT!("RME BENCH REPORT ABORTED\r\n");

        loop {
            core::hint::spin_loop();
        }
    }
}
/* End Function:RME_Bench_Setup_Check *******************************************/

/* Function:RME_Bench_Asid_Alloc *************************************************
Description : Allocate a caller-owned ASID. The RV64V kernel no longer assigns
              ASIDs automatically; ASID 0 stays reserved for untagged tables,
              so the first usable value is 1. The benchmark keeps its address
              spaces for the whole run, so a monotonic bump allocator matches
              its lifetime model.
Input       : None.
Output      : None.
Return      : rme_ptr_t - A unique non-zero ASID.
*****************************************************************************/
fn RME_Bench_Asid_Alloc() -> rme_ptr_t {
    unsafe {
        let Asid = core::ptr::read_volatile(&raw const RME_Bench_Asid_Next);

        if Asid > RME_RV64V_SATP_ASID_MAX {
            RME_BENCH_PRINT!("bench setup failed: ASID space exhausted\r\n");
            RME_BENCH_PRINT!("RME BENCH REPORT ABORTED\r\n");

            loop {
                core::hint::spin_loop();
            }
        }

        core::ptr::write_volatile(&raw mut RME_Bench_Asid_Next, Asid + 1);
        Asid
    }
}
/* End Function:RME_Bench_Asid_Alloc *********************************************/

/* Function:RME_Bench_Asid_Set ***************************************************
Description : Program one top-level page table with a caller-allocated ASID.
              The KFN flushes the old and new ASID before the change, so the
              caller only has to guarantee that the ASID is unique among live
              address spaces.
Input       : Label - The setup step name, printed on failure.
              Cap_Pgt - The top-level page table capability.
              Asid - The caller-allocated ASID.
Output      : None.
Return      : None.
*****************************************************************************/
fn RME_Bench_Asid_Set(Label: &str, Cap_Pgt: rme_ptr_t, Asid: rme_ptr_t) {
    RME_Bench_Setup_Check(
        Label,
        RME_Kfn(RME_BOOT_INIT_KFN, RME_KFN_PGT_ASID_SET, Cap_Pgt, Asid, 0),
    );
}
/* End Function:RME_Bench_Asid_Set ***********************************************/

/* Function:RME_Bench_Setup_Try **************************************************
Description : Check the result of one setup system call of a single test. A
              failed setup only fails that test: the error is recorded in the
              statistics, so the caller can leave the test and the suite
              continues with the next one.
Input       : Stat - The statistics accumulator of the current test.
              Label - The setup step name.
              Ret - The value the setup system call returned.
Output      : None.
Return      : bool - true when the call succeeded.
*****************************************************************************/
fn RME_Bench_Setup_Try(Stat: &mut RME_Bench_Stat, Label: &str, Ret: rme_ptr_t) -> bool {
    let Signed = Ret as i64;

    if Signed < 0 {
        RME_BENCH_PRINT!("bench setup failed: {} ret={}\r\n", Label, Signed);
        let _ = Stat.Set_Ret(Signed);
        false
    } else {
        true
    }
}
/* End Function:RME_Bench_Setup_Try *********************************************/

/* Function:RME_Bench_Phase_Get **************************************************
Description : Read the shared phase flag.
Input       : None.
Output      : None.
Return      : u32 - The current phase.
*****************************************************************************/
#[inline(always)]
fn RME_Bench_Phase_Get() -> u32 {
    unsafe { core::ptr::read_volatile(&raw const RME_Bench_Phase) }
}
/* End Function:RME_Bench_Phase_Get **********************************************/

/* Function:RME_Bench_Phase_Set **************************************************
Description : Publish the shared phase flag.
Input       : Phase - The phase to publish.
Output      : None.
Return      : None.
*****************************************************************************/
#[inline(always)]
fn RME_Bench_Phase_Set(Phase: u32) {
    unsafe { core::ptr::write_volatile(&raw mut RME_Bench_Phase, Phase) }
}
/* End Function:RME_Bench_Phase_Set **********************************************/

/* Function:RME_Bench_Start_Set **************************************************
Description : Latch the timestamp of the workload's departure.
Input       : Value - The timestamp.
Output      : None.
Return      : None.
*****************************************************************************/
#[inline(always)]
fn RME_Bench_Start_Set(Value: u64) {
    unsafe { core::ptr::write_volatile(&raw mut RME_Bench_Start, Value) }
}
/* End Function:RME_Bench_Start_Set **********************************************/

/* Function:RME_Bench_Start_Get **************************************************
Description : Read the timestamp of the workload's departure.
Input       : None.
Output      : None.
Return      : u64 - The timestamp.
*****************************************************************************/
#[inline(always)]
fn RME_Bench_Start_Get() -> u64 {
    unsafe { core::ptr::read_volatile(&raw const RME_Bench_Start) }
}
/* End Function:RME_Bench_Start_Get **********************************************/

/* Function:RME_Bench_End_Set ****************************************************
Description : Latch the timestamp of the workload's arrival.
Input       : Value - The timestamp.
Output      : None.
Return      : None.
*****************************************************************************/
#[inline(always)]
fn RME_Bench_End_Set(Value: u64) {
    unsafe { core::ptr::write_volatile(&raw mut RME_Bench_End, Value) }
}
/* End Function:RME_Bench_End_Set ***********************************************/

/* Function:RME_Bench_End_Get ****************************************************
Description : Read the timestamp of the workload's arrival.
Input       : None.
Output      : None.
Return      : u64 - The timestamp.
*****************************************************************************/
#[inline(always)]
fn RME_Bench_End_Get() -> u64 {
    unsafe { core::ptr::read_volatile(&raw const RME_Bench_End) }
}
/* End Function:RME_Bench_End_Get ***********************************************/

/* Function:RME_Bench_Stat_Reset ************************************************
Description : Reset the shared statistics accumulator.
Input       : None.
Output      : None.
Return      : None.
*****************************************************************************/
#[inline(always)]
fn RME_Bench_Stat_Reset() {
    unsafe {
        core::ptr::write_volatile(&raw mut RME_Bench_Stat_Total, 0);
        core::ptr::write_volatile(&raw mut RME_Bench_Stat_Max, 0);
        core::ptr::write_volatile(&raw mut RME_Bench_Stat_Min, u32::MAX);
        core::ptr::write_volatile(&raw mut RME_Bench_Stat_Cnt, 0);
        core::ptr::write_volatile(&raw mut RME_Bench_Stat_Ret, 0);
        core::ptr::write_volatile(&raw mut RME_Bench_Stat_Fail, 0);
    }
}
/* End Function:RME_Bench_Stat_Reset ********************************************/

/* Function:RME_Bench_Stat_Record ***********************************************
Description : Accumulate the result of a single round into the shared
              statistics. The counter difference is calculated with 32 bit
              arithmetic, so that a single overflow of the 64 bit cycle
              counter between the two reads is handled transparently.
Input       : Diff - The number of cycles the round took.
Output      : None.
Return      : None.
*****************************************************************************/
#[inline(always)]
fn RME_Bench_Stat_Record(Diff: u32) {
    unsafe {
        if core::ptr::read_volatile(&raw const RME_Bench_Stat_Fail) != 0 {
            return;
        }

        let Total = core::ptr::read_volatile(&raw const RME_Bench_Stat_Total);
        let Max = core::ptr::read_volatile(&raw const RME_Bench_Stat_Max);
        let Min = core::ptr::read_volatile(&raw const RME_Bench_Stat_Min);
        let Cnt = core::ptr::read_volatile(&raw const RME_Bench_Stat_Cnt);

        core::ptr::write_volatile(&raw mut RME_Bench_Stat_Total, Total + Diff as u64);
        if Diff > Max {
            core::ptr::write_volatile(&raw mut RME_Bench_Stat_Max, Diff);
        }
        if Diff < Min {
            core::ptr::write_volatile(&raw mut RME_Bench_Stat_Min, Diff);
        }
        core::ptr::write_volatile(&raw mut RME_Bench_Stat_Cnt, Cnt + 1);
    }
}
/* End Function:RME_Bench_Stat_Record *******************************************/

/* Function:RME_Bench_Stat_Set_Ret **********************************************
Description : Record the return value of a timed system call on the shared /
              helper path. Direct tests use RME_Bench_Stat::Set_Ret instead.
Input       : Ret - The return value (signed).
Output      : None.
Return      : i64 - The same value, for convenient use in `let _ =`.
*****************************************************************************/
#[inline(always)]
fn RME_Bench_Stat_Set_Ret(Ret: i64) -> i64 {
    unsafe {
        core::ptr::write_volatile(&raw mut RME_Bench_Stat_Ret, Ret);
        if Ret < 0 {
            let Fail = core::ptr::read_volatile(&raw const RME_Bench_Stat_Fail);
            core::ptr::write_volatile(&raw mut RME_Bench_Stat_Fail, Fail + 1);
        }
    }
    Ret
}
/* End Function:RME_Bench_Stat_Set_Ret ******************************************/

/* Function:RME_Bench_Stat_Failed ************************************************
Description : Read the shared helper failure flag.
*****************************************************************************/
#[inline(always)]
fn RME_Bench_Stat_Failed() -> bool {
    unsafe { core::ptr::read_volatile(&raw const RME_Bench_Stat_Fail) != 0 }
}
/* End Function:RME_Bench_Stat_Failed ********************************************/

/* Type ***********************************************************************/
/// The accumulated statistics of a single benchmark test.
struct RME_Bench_Stat {
    Total: u64,
    Max: u32,
    Min: u32,
    Count: u64,
    Ret: i64,
    Fail: u64,
}
/* End Type ******************************************************************/

/* Function:RME_Bench_Stat::New ***************************************************
Description : Create an empty statistics accumulator.
Input       : None.
Output      : None.
Return      : RME_Bench_Stat - The accumulator.
*****************************************************************************/
impl RME_Bench_Stat {
    fn New() -> Self {
        Self {
            Total: 0,
            Max: 0,
            Min: u32::MAX,
            Count: 0,
            Ret: 0,
            Fail: 0,
        }
    }

    /// Reset the accumulator, so that it can be reused for another test.
    fn Reset(&mut self) {
        self.Total = 0;
        self.Max = 0;
        self.Min = u32::MAX;
        self.Count = 0;
        self.Ret = 0;
        self.Fail = 0;
    }

    /// Record the return value of the timed call for this test. A negative
    /// value is counted as a failed round; the last value is retained only for
    /// the printed `ret=` field.
    #[inline(always)]
    fn Set_Ret(&mut self, Ret: i64) -> i64 {
        self.Ret = Ret;
        if Ret < 0 {
            self.Fail += 1;
        }
        Ret
    }

    /// Accumulate the result of a single round into the current statistics.
    /// A failed round is not recorded; the loop condition stops the test.
    #[inline(always)]
    fn Record(&mut self, Diff: u32) {
        if self.Fail != 0 {
            return;
        }
        self.Total += Diff as u64;
        if Diff > self.Max {
            self.Max = Diff;
        }
        if Diff < self.Min {
            self.Min = Diff;
        }
        self.Count += 1;
    }

    /// Pick up the statistics that the receiving side has accumulated.
    fn Load_Shared(&mut self) {
        self.Total = unsafe { core::ptr::read_volatile(&raw const RME_Bench_Stat_Total) };
        self.Max = unsafe { core::ptr::read_volatile(&raw const RME_Bench_Stat_Max) };
        self.Min = unsafe { core::ptr::read_volatile(&raw const RME_Bench_Stat_Min) };
        self.Count = unsafe { core::ptr::read_volatile(&raw const RME_Bench_Stat_Cnt) };
        self.Ret = unsafe { core::ptr::read_volatile(&raw const RME_Bench_Stat_Ret) };
        self.Fail = unsafe { core::ptr::read_volatile(&raw const RME_Bench_Stat_Fail) };
    }

    /// A helper-based test is only valid when the expected number of rounds
    /// completed. A blocked or never-woken helper otherwise leaves a partial
    /// (but still non-negative) result behind.
    fn Require_Count(&mut self, Expected: u64) {
        if self.Count != Expected {
            let _ = self.Set_Ret(-1);
        }
    }
}
/* End Function:RME_Bench_Stat::New ***********************************************/

/* Function:RME_Bench_Report ****************************************************
Description : Print the name of a test followed by its average, maximum and
              minimum results, all of them expressed in CPU cycles. The layout
              matches the RVM_LIST macro of the C rvm_benchmark.h.
Input       : Title - The name of the test, padded with spaces.
              Stat - The accumulated statistics.
Output      : None.
Return      : None.
*****************************************************************************/
fn RME_Bench_Report(Title: &str, Stat: &RME_Bench_Stat) {
    if Stat.Fail != 0 {
        RME_BENCH_PRINT!("{:<44} : FAIL ret={}\r\n", Title, Stat.Ret);
        return;
    }

    let Rounds = if Stat.Count > 0 { Stat.Count } else { 1 };
    let Min = if Stat.Count > 0 { Stat.Min } else { 0 };

    RME_BENCH_PRINT!(
        "{:<44} : {} / {} / {} / ret={}\r\n",
        Title,
        Stat.Total / Rounds,
        Stat.Max,
        Min,
        Stat.Ret
    );
}
/* End Function:RME_Bench_Report *************************************************/

/* Function:RME_Bench_Stack_Top *************************************************
Description : Compute the stack pointer of a helper thread or of an
              invocation stub. The RISC-V stack grows downwards, so the
              initial stack pointer is one past the end of the stack. The
              RISC-V ABI requires the stack pointer to be 16 byte aligned,
              and the kernel uses the value verbatim, so the alignment is
              fixed up here. The fixup rounds the pointer down, which keeps
              the stack inside its own array - rounding up would place the
              stack pointer past the end, into whatever follows in the bss.
Input       : Stack - A pointer to the stack array.
Output      : None.
Return      : u64 - The stack pointer.
*****************************************************************************/
#[inline(always)]
fn RME_Bench_Stack_Top<const N: usize>(Stack: *const [u64; N]) -> u64 {
    (Stack.cast::<u64>() as u64 + (N as u64) * 8) & !0xF
}
/* End Function:RME_Bench_Stack_Top *********************************************/

/* Function:RME_Bench_Pgt_Crt ***************************************************
Description : Create a page table. On RV64V every table has 512 entries
              (Num_Order = 9). The Num_Order is packed into the high bits of
              the system call number (mirroring the kernel's RME_PARAM_PC).
Input       : Cap_Pgt - The capability slot for the new page table.
              Raddr - The relative KOM address for the new table object.
              Base - The virtual base address this table covers.
              Is_Top - Whether this is the top-level (root) table.
              Size_Order - The page size order (RME_PGT_SIZE_4K/2M/1G).
Output      : None.
Return      : u64 - The kernel return value (in a0).
*****************************************************************************/
#[inline(always)]
fn RME_Bench_Pgt_Crt(Cap_Pgt: u64, Raddr: u64, Base: u64, Is_Top: u64, Size_Order: u64) -> u64 {
    RME_Pgt_Crt(
        RME_BOOT_INIT_CPT,
        RME_BOOT_INIT_KOM,
        Cap_Pgt,
        Raddr,
        Base,
        Is_Top,
        Size_Order,
    )
}
/* End Function:RME_Bench_Pgt_Crt *************************************************/

/* Function:RME_Bench_Pgt_Con ***************************************************
Description : Map a child page table into the parent page table, building the
              address space tree.
Input       : Cap_Parent - The parent page table capability slot.
              Pos - The parent table position to map the child to.
              Cap_Child - The child page table capability slot.
Output      : None.
Return      : u64 - The kernel return value (in a0).
*****************************************************************************/
#[inline(always)]
fn RME_Bench_Pgt_Con(Cap_Parent: u64, Pos: u64, Cap_Child: u64) -> u64 {
    RME_Pgt_Con(Cap_Parent, Pos, Cap_Child, RME_PGT_ALL_PERM)
}
/* End Function:RME_Bench_Pgt_Con *************************************************/

/* Function:RME_Bench_Pgt_Add ***************************************************
Description : Delegate a page from one page table to another. Used to copy the
              init image's code/bss pages into the new process's leaf table,
              so the cross-process thread can execute the same code and use
              the same stack/buffers.
Input       : Cap_Dst - The destination page table capability slot.
              Pos_Dst - The destination table position.
              Cap_Src - The source page table capability slot.
              Pos_Src - The source table position.
Output      : None.
Return      : u64 - The kernel return value (in a0).
*****************************************************************************/
#[inline(always)]
fn RME_Bench_Pgt_Add(Cap_Dst: u64, Pos_Dst: u64, Cap_Src: u64, Pos_Src: u64) -> u64 {
    RME_Pgt_Add(Cap_Dst, Pos_Dst, Cap_Src, Pos_Src, RME_PGT_ALL_DYN)
}
/* End Function:RME_Bench_Pgt_Add *************************************************/

/* Function:RME_Bench_Thd_Crt ***************************************************
Description : Create a thread.
Input       : Cap_Cpt - The capability table that holds the capability table
                        the new thread's capability is created in.
              Cap_Thd - The capability slot of the new thread.
              Cap_Prc - The process the thread belongs to.
              Raddr - The relative KOM address of the thread object.
Output      : None.
Return      : u64 - The kernel return value (in a0).
*****************************************************************************/
#[inline(always)]
fn RME_Bench_Thd_Crt(Cap_Cpt: u64, Cap_Thd: u64, Cap_Prc: u64, Raddr: u64) -> u64 {
    RME_Thd_Crt(Cap_Cpt, RME_BOOT_INIT_KOM, Cap_Thd, Cap_Prc, 31, Raddr)
}
/* End Function:RME_Bench_Thd_Crt *************************************************/

/* Function:RME_Bench_Thd_Bind **************************************************
Description : Bind a thread to the current processor, nominate the init thread
              as its scheduler and give it a preemptive priority.
Input       : Cap_Thd - The capability to the thread (two-level).
              Prio - The preemptive priority of the thread.
Output      : None.
Return      : u64 - The kernel return value (in a0).
*****************************************************************************/
#[inline(always)]
fn RME_Bench_Thd_Bind(Cap_Thd: u64, Prio: u64) -> u64 {
    RME_Thd_Sched_Bind(Cap_Thd, RME_BOOT_INIT_THD, RME_CID_NULL, 0, Prio, 0)
}
/* End Function:RME_Bench_Thd_Bind ************************************************/

/* Function:RME_Bench_Sig_Crt ***************************************************
Description : Create a signal endpoint in the init capability table.
Input       : Cap_Sig - The capability slot of the new endpoint.
Output      : None.
Return      : u64 - The kernel return value (in a0).
*****************************************************************************/
#[inline(always)]
fn RME_Bench_Sig_Crt(Cap_Sig: u64) -> u64 {
    RME_Sig_Crt(RME_BOOT_INIT_CPT, Cap_Sig)
}
/* End Function:RME_Bench_Sig_Crt *************************************************/

/* Function:RME_Bench_Inv_Entry_Body *******************************************
Description : Round-trip stub body; a normal return restores s0-s11.
Input       : _param - Unused.
Output      : None.
Return      : u64 - The value passed to the invoker by INV_RET.
*****************************************************************************/
extern "C" fn RME_Bench_Inv_Entry_Body(_param: u64) -> u64 {
    0x1234
}
/* End Function:RME_Bench_Inv_Entry_Body ***************************************/

/* Function:RME_Bench_Inv_Entry *************************************************
Description : Round-trip invocation entry: call the body, then INV_RET.
Input       : param - The parameter the invoker passed (unused).
Output      : None.
Return      : ! - This function never returns.
*****************************************************************************/
#[unsafe(naked)]
extern "C" fn RME_Bench_Inv_Entry(_param: u64) -> ! {
    core::arch::naked_asm!(
        "call {body}",
        "mv a1, a0",
        "li a0, 0",
        "li a2, 0",
        "li a3, 0",
        "ecall",
        "j .",
        body = sym RME_Bench_Inv_Entry_Body,
    );
}
/* End Function:RME_Bench_Inv_Entry **********************************************/

/* Function:RME_Bench_Inv_Act_Entry_Body ***************************************
Description : One-way stub body; latches the arrival time.
Input       : _param - Unused.
Output      : None.
Return      : u64 - The value passed to the invoker by INV_RET.
*****************************************************************************/
extern "C" fn RME_Bench_Inv_Act_Entry_Body(_param: u64) -> u64 {
    RME_Bench_End_Set(RME_Bench_Read_Cycle());
    0x1111
}
/* End Function:RME_Bench_Inv_Act_Entry_Body ***********************************/

/* Function:RME_Bench_Inv_Act_Entry **********************************************
Description : One-way entry: call the body, then INV_RET.
Input       : _param - Unused.
Output      : None.
Return      : ! - This function never returns here.
*****************************************************************************/
#[unsafe(naked)]
extern "C" fn RME_Bench_Inv_Act_Entry(_param: u64) -> ! {
    core::arch::naked_asm!(
        "call {body}",
        "mv a1, a0",
        "li a0, 0",
        "li a2, 0",
        "li a3, 0",
        "ecall",
        "j .",
        body = sym RME_Bench_Inv_Act_Entry_Body,
    );
}
/* End Function:RME_Bench_Inv_Act_Entry ******************************************/

/* Function:RME_Bench_Inv_Ret_Entry_Body ***************************************
Description : Direct-return stub body; latches START.
Input       : _param - Unused.
Output      : None.
Return      : u64 - The value passed to the invoker by INV_RET.
*****************************************************************************/
extern "C" fn RME_Bench_Inv_Ret_Entry_Body(_param: u64) -> u64 {
    RME_Bench_Start_Set(RME_Bench_Read_Cycle());
    0x4444
}
/* End Function:RME_Bench_Inv_Ret_Entry_Body ***********************************/

/* Function:RME_Bench_Inv_Ret_Entry **********************************************
Description : Direct-return entry: call the body, then INV_RET.
Input       : _param - Unused.
Output      : None.
Return      : ! - This function never returns here.
*****************************************************************************/
#[unsafe(naked)]
extern "C" fn RME_Bench_Inv_Ret_Entry(_param: u64) -> ! {
    core::arch::naked_asm!(
        "call {body}",
        "mv a1, a0",
        "li a0, 0",
        "li a2, 0",
        "li a3, 0",
        "ecall",
        "j .",
        body = sym RME_Bench_Inv_Ret_Entry_Body,
    );
}
/* End Function:RME_Bench_Inv_Ret_Entry ******************************************/

/* Function:RME_Bench_Inv_Dbg_Entry_Body ***************************************
Description : DEBUG_INV_MOD stub body; measures the layer-0 SP query.
Input       : _param - Unused.
Output      : None.
Return      : u64 - The value passed to the invoker by INV_RET.
*****************************************************************************/
extern "C" fn RME_Bench_Inv_Dbg_Entry_Body(_param: u64) -> u64 {
    RME_Bench_Start_Set(RME_Bench_Read_Cycle());
    let Ret = RME_Kfn(
        RME_BENCH_CAP2_KFN,
        RME_KFN_DEBUG_INV_MOD,
        RME_BENCH_CAP2_INIT_THD,
        RME_RV64V_KFN_DEBUG_INV_MOD_SP_GET,
        0,
    );
    RME_Bench_End_Set(RME_Bench_Read_Cycle());
    unsafe {
        core::ptr::write_volatile(&raw mut RME_Bench_Kfn_Inv_Ret, Ret as i64);
    }
    0x5555
}
/* End Function:RME_Bench_Inv_Dbg_Entry_Body ***********************************/

/* Function:RME_Bench_Inv_Dbg_Entry **********************************************
Description : DEBUG_INV_MOD entry: call the body, then INV_RET.
Input       : _param - Unused.
Output      : None.
Return      : ! - This function never returns here.
*****************************************************************************/
#[unsafe(naked)]
extern "C" fn RME_Bench_Inv_Dbg_Entry(_param: u64) -> ! {
    core::arch::naked_asm!(
        "call {body}",
        "mv a1, a0",
        "li a0, 0",
        "li a2, 0",
        "li a3, 0",
        "ecall",
        "j .",
        body = sym RME_Bench_Inv_Dbg_Entry_Body,
    );
}
/* End Function:RME_Bench_Inv_Dbg_Entry *******************************************/

/* Function:RME_Bench_P1T2_Entry ***********************************************
Description : The entry point of the helper thread of the init process. It
              plays the receiving role in the intra-process signal test.
Input       : _param - The role passed by the creator (unused).
Output      : None.
Return      : ! - This function never returns.
*****************************************************************************/
extern "C" fn RME_Bench_P1T2_Entry(_param: u64) -> ! {
    loop {
        match RME_Bench_Phase_Get() {
            RME_BENCH_PHASE_SIG_SELF => {
                RME_Bench_Signal_Self();
                RME_Bench_Phase_Set(RME_BENCH_PHASE_IDLE);
            }
            RME_BENCH_PHASE_SIG_RECV => {
                RME_Bench_Signal_Receive();
                RME_Bench_Phase_Set(RME_BENCH_PHASE_IDLE);
            }
            RME_BENCH_PHASE_SIG_RCV_ONLY => {
                RME_Bench_Signal_Rcv_Only();
                RME_Bench_Phase_Set(RME_BENCH_PHASE_IDLE);
            }
            _ => {
                /* Nothing to do - park until the measuring thread wakes us */
                let Cap_Park =
                    unsafe { core::ptr::read_volatile(&raw const RME_Bench_Cap_Sig_Park) };
                let _ = RME_Sig_Rcv(Cap_Park, RME_RCV_BM);
            }
        }
    }
}
/* End Function:RME_Bench_P1T2_Entry ********************************************/

/* Function:RME_Bench_P2T1_Entry ***********************************************
Description : The entry point of the receiver thread of the second process.
              It plays the receiving role in the inter-process signal test.
Input       : _param - The role passed by the creator (unused).
Output      : None.
Return      : ! - This function never returns.
*****************************************************************************/
extern "C" fn RME_Bench_P2T1_Entry(_param: u64) -> ! {
    loop {
        match RME_Bench_Phase_Get() {
            RME_BENCH_PHASE_SIG_RECV2 => {
                RME_Bench_Signal_Receive();
                RME_Bench_Phase_Set(RME_BENCH_PHASE_IDLE);
            }
            _ => {
                /* Nothing to do - park until the measuring thread wakes us.
                 * The parking endpoint is a one level capability ID, because
                 * this thread runs with the second process' capability
                 * table. */
                let _ = RME_Sig_Rcv(RME_BENCH_CAP2_SIG_PARK, RME_RCV_BM);
            }
        }
    }
}
/* End Function:RME_Bench_P2T1_Entry ********************************************/

/* Function:RME_Bench_P2T2_Entry ***********************************************
Description : The entry point of the switch target of the second process. It
              latches the arrival timestamp of the inter-process switch and
              switches straight back.
Input       : _param - The role passed by the creator (unused).
Output      : None.
Return      : ! - This function never returns.
*****************************************************************************/
extern "C" fn RME_Bench_P2T2_Entry(_param: u64) -> ! {
    loop {
        /* Latch the arrival of the switch, then hand the CPU back */
        RME_Bench_End_Set(RME_Bench_Read_Cycle());
        let Cap_P1T1 = unsafe { core::ptr::read_volatile(&raw const RME_Bench_Cap_P1T1_In_P2) };
        let _ = RME_Thd_Swt(Cap_P1T1, 0);
    }
}
/* End Function:RME_Bench_P2T2_Entry ********************************************/

/* Function:RME_Bench_P1T3_Entry ***********************************************
Description : The entry point of the switch target of the init process. It latches
              the arrival timestamp of the intra-process switch and switches
              straight back.
Input       : _param - The role passed by the creator (unused).
Output      : None.
Return      : ! - This function never returns.
*****************************************************************************/
extern "C" fn RME_Bench_P1T3_Entry(_param: u64) -> ! {
    loop {
        /* Latch the arrival of the switch, then hand the CPU back */
        RME_Bench_End_Set(RME_Bench_Read_Cycle());
        let _ = RME_Thd_Swt(RME_BOOT_INIT_THD, 0);
    }
}
/* End Function:RME_Bench_P1T3_Entry ********************************************/

/* Function:RME_Bench_Signal_Self **********************************************
Description : The self signalling test. The helper thread sends a signal to
              its own endpoint and receives it right away, so the receive
              never blocks and the round is a pure system call round trip.
Input       : None.
Output      : None.
Return      : None.
*****************************************************************************/
fn RME_Bench_Signal_Self() {
    let Cap_Self = unsafe { core::ptr::read_volatile(&raw const RME_Bench_Cap_Sig_Self) };
    let mut Count: u64 = 0;

    RME_Bench_Stat_Reset();

    while Count < RME_BENCH_LOOP_NUM && !RME_Bench_Stat_Failed() {
        let Start = RME_Bench_Read_Cycle();
        let Send = RME_Sig_Snd(Cap_Self, 1);
        if (Send as i64) < 0 {
            let _ = RME_Bench_Stat_Set_Ret(Send as i64);
            break;
        }
        let _ = RME_Bench_Stat_Set_Ret(RME_Sig_Rcv(Cap_Self, RME_RCV_BM) as i64);
        let End = RME_Bench_Read_Cycle();

        RME_Bench_Stat_Record((End - Start) as u32);
        Count += 1;
    }
}
/* End Function:RME_Bench_Signal_Self *******************************************/

/* Function:RME_Bench_Signal_Receive *******************************************
Description : The receiving side of the signal tests. The helper thread
              blocks on the data channel, and the sender wakes it up once
              per round. The delivery latency is the distance between the
              timestamp the sender latched and the one latched here.
Input       : None.
Output      : None.
Return      : None.
*****************************************************************************/
fn RME_Bench_Signal_Receive() {
    /* The receiver of the second process uses its own capability table,
     * where the data channel lives in a different slot. */
    let Cap_Data = if RME_Bench_Phase_Get() == RME_BENCH_PHASE_SIG_RECV2 {
        RME_BENCH_CAP2_SIG_DATA
    } else {
        unsafe { core::ptr::read_volatile(&raw const RME_Bench_Cap_Sig_Data) }
    };
    let mut Count: u64 = 0;

    RME_Bench_Stat_Reset();

    while Count < RME_BENCH_LOOP_NUM && !RME_Bench_Stat_Failed() {
        /* Wait for the workload - this blocks until the sender wakes us */
        let _ = RME_Bench_Stat_Set_Ret(RME_Sig_Rcv(Cap_Data, RME_RCV_BM) as i64);
        let End = RME_Bench_Read_Cycle();

        RME_Bench_Stat_Record((End - RME_Bench_Start_Get()) as u32);
        Count += 1;
    }
}
/* End Function:RME_Bench_Signal_Receive ****************************************/

/* Function:RME_Bench_Signal_Rcv_Only *******************************************
Description : Receive-only measurement. The helper sends one signal untimed,
              then times a non-blocking receive, so the window contains only
              SIG_RCV.
Input       : None.
Output      : None.
Return      : None.
*****************************************************************************/
fn RME_Bench_Signal_Rcv_Only() {
    let Cap = RME_BENCH_CAP_SIG_RCV_ONLY;
    let mut Count: u64 = 0;

    RME_Bench_Stat_Reset();

    while Count < RME_BENCH_LOOP_NUM && !RME_Bench_Stat_Failed() {
        let Send = RME_Sig_Snd(Cap, 1);
        if (Send as i64) < 0 {
            let _ = RME_Bench_Stat_Set_Ret(Send as i64);
            break;
        }
        let Start = RME_Bench_Read_Cycle();
        let _ = RME_Bench_Stat_Set_Ret(RME_Sig_Rcv(Cap, RME_RCV_NS) as i64);
        let End = RME_Bench_Read_Cycle();

        RME_Bench_Stat_Record((End - Start) as u32);
        Count += 1;
    }
}
/* End Function:RME_Bench_Signal_Rcv_Only ***************************************/

/* Function:RME_Bench_Prepare_Process2 *****************************************
Description : Create the second process with its own capability table, its
              own three-level Sv39 page table and its own threads. The new
              process shares the init image's physical pages, so the new
              threads can execute the very same code and use the very same
              stacks.
Input       : None.
Output      : None.
Return      : None - A failed setup step halts the benchmark, so this
              function never returns in that case.
*****************************************************************************/
fn RME_Bench_Prepare_Process2() {
    /* ---- Create the new process's capability table (16 entries) ---- */
    /* CPT_CRT: Cid=Cap_Cpt_Crt, Param0=D1(Cap_Kom)|D0(Cap_Crt),
     * Param1=Raddr, Param2=Entry_Num
     * The table holds the delegated caps in slots 0..6 (RME_BENCH_CAP2_*). It is not the
     * init capability table, which the boot code creates with 64 entries. */
    let Raddr = RME_Bench_Kom_Alloc_Cpt(16);
    RME_Bench_Setup_Check(
        "CPT_CRT process2 captbl",
        RME_Cpt_Crt(
            RME_BOOT_INIT_CPT,
            RME_BOOT_INIT_KOM,
            RME_BENCH_CAP_CPT,
            Raddr,
            16,
        ),
    );

    /* ---- Create the new process's page tables (root + L1 + L2) ---- */
    /* Root: 1GB entries, covers the low half. Raddr must be 4KB aligned
     * because Sv39 finds a table through its PPN. */
    let Raddr = RME_Bench_Kom_Alloc_Pgt();
    RME_Bench_Setup_Check(
        "PGT_CRT process2 root",
        RME_Bench_Pgt_Crt(RME_BENCH_CAP_PGT_TOP_NEW, Raddr, 0, 1, RME_PGT_SIZE_1G),
    );
    /* The caller owns ASID assignment: give process 2 its own ASID before any
     * thread can switch to this root table. The KFN flushes both the old and
     * the new ASID, so the table does not inherit stale translations. */
    let Asid = RME_Bench_Asid_Alloc();
    RME_Bench_Asid_Set("PGT_ASID process2 root", RME_BENCH_CAP_PGT_TOP_NEW, Asid);
    unsafe {
        core::ptr::write_volatile(&raw mut RME_Bench_Asid_P2, Asid);
    }
    /* L1: 2MB entries, covers the user window [0,1GB) */
    let Raddr = RME_Bench_Kom_Alloc_Pgt();
    RME_Bench_Setup_Check(
        "PGT_CRT process2 L1",
        RME_Bench_Pgt_Crt(RME_BENCH_CAP_PGT_L1, Raddr, 0, 0, RME_PGT_SIZE_2M),
    );
    /* L2: 4KB entries, covers [0x20000000,0x20200000) */
    let Raddr = RME_Bench_Kom_Alloc_Pgt();
    RME_Bench_Setup_Check(
        "PGT_CRT process2 L2",
        RME_Bench_Pgt_Crt(RME_BENCH_CAP_PGT_L2, Raddr, 0x20000000, 0, RME_PGT_SIZE_4K),
    );

    /* ---- Connect the tables (build the address space tree) ---- */
    /* root[0] -> L1 (0x20000000>>30 = 0) */
    RME_Bench_Setup_Check(
        "PGT_CON process2 root->L1",
        RME_Bench_Pgt_Con(
            RME_BENCH_CAP_PGT_TOP_NEW,
            RME_RV64V_TOP_POS,
            RME_BENCH_CAP_PGT_L1,
        ),
    );
    /* L1[256] -> L2 (0x20000000>>21 = 256) */
    RME_Bench_Setup_Check(
        "PGT_CON process2 L1->L2",
        RME_Bench_Pgt_Con(RME_BENCH_CAP_PGT_L1, RME_RV64V_L1_POS, RME_BENCH_CAP_PGT_L2),
    );

    /* ---- Delegate the init image's code/bss pages into the new L2 ---- */
    /* Copy pages 0..15 (VA 0x20000000..0x20010000) from the init L2 table */
    let mut Count: u64 = 0;
    while Count < RME_RV64V_IMAGE_PAGES {
        RME_Bench_Setup_Check(
            "PGT_ADD process2 image page",
            RME_Bench_Pgt_Add(RME_BENCH_CAP_PGT_L2, Count, RME_BOOT_INIT_PGT_L2, Count),
        );
        Count += 1;
    }

    /* ---- Create the new process ---- */
    /* PRC_CRT: Cid=Cap_Cpt_Crt, Param0=Cap_Prc, Param1=Cap_Cpt, Param2=Cap_Pgt.
     * The new process gets its OWN capability table and its OWN page table. */
    RME_Bench_Setup_Check(
        "PRC_CRT process2",
        RME_Prc_Crt(
            RME_BOOT_INIT_CPT,
            RME_BENCH_CAP_PRC,
            RME_BENCH_CAP_CPT,
            RME_BENCH_CAP_PGT_TOP_NEW,
        ),
    );

    /* ---- Create the receiver thread in the new process' OWN captbl ---- */
    /* THD_CRT: Cid=Cap_Cpt, Param0=D1(Cap_Kom)|D0(Cap_Thd),
     * Param1=D1(Cap_Prc)|D0(Prio_Max), Param2=Raddr.
     * Cid now points at the NEW captbl, so the new thread's cap slot is
     * created in the new process' independent capability namespace. */
    let Raddr = RME_Bench_Kom_Alloc_Thd();
    RME_Bench_Setup_Check(
        "THD_CRT process2 receiver",
        RME_Bench_Thd_Crt(
            RME_BENCH_CAP_CPT,
            RME_BENCH_CAP2_THD,
            RME_BENCH_CAP_PRC,
            Raddr,
        ),
    );

    /* ---- Create the switch target thread in the new process' OWN captbl ---- */
    let Raddr = RME_Bench_Kom_Alloc_Thd();
    RME_Bench_Setup_Check(
        "THD_CRT process2 switch target",
        RME_Bench_Thd_Crt(
            RME_BENCH_CAP_CPT,
            RME_BENCH_CAP2_THD2,
            RME_BENCH_CAP_PRC,
            Raddr,
        ),
    );

    /* ---- Create the helper thread of the init process ---- */
    let Raddr = RME_Bench_Kom_Alloc_Thd();
    RME_Bench_Setup_Check(
        "THD_CRT init process receiver",
        RME_Bench_Thd_Crt(
            RME_BOOT_INIT_CPT,
            RME_BENCH_CAP_THD,
            RME_BOOT_INIT_PRC,
            Raddr,
        ),
    );

    /* ---- Create the switch target of the init process ---- */
    /* It shares the init process' capability table, so no delegation is
     * needed for it. */
    let Raddr = RME_Bench_Kom_Alloc_Thd();
    RME_Bench_Setup_Check(
        "THD_CRT init process switch target",
        RME_Bench_Thd_Crt(
            RME_BOOT_INIT_CPT,
            RME_BENCH_CAP_THD2,
            RME_BOOT_INIT_PRC,
            Raddr,
        ),
    );

    /* ---- Delegate caps between the two independent captbls ---- */
    /* CPT_ADD: Param0=D1(Cap_Cpt_Dst)|D0(Cap_Dst),
     * Param1=D1(Cap_Cpt_Src)|D0(Cap_Src), Param2=Flag.
     *
     * 1) Delegate the switch target cap (new captbl slot RME_BENCH_CAP2_THD2) into the
     *    init captbl slot RME_BENCH_CAP_P2_THD, so the init thread can bind it,
     *    set its execution info, transfer time and switch to it.
     * 2) Delegate the init thread cap (init captbl slot RME_BOOT_INIT_THD)
     *    into the new captbl slot RME_BENCH_CAP2_INIT_THD, so the cross-process
     *    thread can switch back to the init thread from inside its own
     *    namespace. */
    RME_Bench_Setup_Check(
        "CPT_ADD process2 switch target -> init process",
        RME_Cpt_Add(
            RME_BOOT_INIT_CPT,
            RME_BENCH_CAP_P2_THD,
            RME_BENCH_CAP_CPT,
            RME_BENCH_CAP2_THD2,
            RME_THD_FLAG_ALL,
        ),
    );
    RME_Bench_Setup_Check(
        "CPT_ADD init thread -> process2",
        RME_Cpt_Add(
            RME_BENCH_CAP_CPT,
            RME_BENCH_CAP2_INIT_THD,
            /* The source is the per-CPU thread table (a one-level slot of the
             * init captbl), and the init thread is its slot 0. CPT_ADD addresses
             * both the table and the slot with one-level IDs, so the two-level
             * RME_BOOT_INIT_THD cannot be used here. */
            RME_BOOT_TBL_THD,
            RME_BOOT_INIT_THD_SLOT,
            /* Only SWT: the boot init-thread cap lacks EXEC_SET/SCHED_CHILD/
             * SCHED_FREE, so RME_THD_FLAG_ALL is not a valid subset and would be
             * rejected with RME_ERR_CPT_FLAG. The cross-process thread only ever
             * switches back, so SWT alone is both sufficient and correct. */
            RME_THD_FLAG_SWT,
        ),
    );

    /* ---- Resolve the capability IDs the helper threads need ---- */
    /* The two-level IDs point at the init capability table (slot 0), so
     * they are valid for the init thread, whose capability table is the
     * init one.
     * The IDs the threads of the second process use are one-level ones
     * instead: those threads run with the second process' capability
     * table, and a two-level ID would address the slot 13 of the *current*
     * table - which is the second process' own table, not the init one. */
    unsafe {
        core::ptr::write_volatile(
            &raw mut RME_Bench_Cap_P2_THD,
            RME_CID_Build(RME_BOOT_INIT_CPT, RME_BENCH_CAP_P2_THD),
        );
        core::ptr::write_volatile(&raw mut RME_Bench_Cap_P1T1_In_P2, RME_BENCH_CAP2_INIT_THD);
    }
}
/* End Function:RME_Bench_Prepare_Process2 *************************************/

/* Function:RME_Bench_Prepare_Signals ******************************************
Description : Create the signal endpoints the benchmark uses and delegate
              the ones the second process needs into its capability table.
              The parking endpoints wake a parked helper thread up, and the
              data channel carries the workload from the sender to the
              receiver.
Input       : None.
Output      : None.
Return      : None - A failed setup step halts the benchmark, so this
              function never returns in that case.
*****************************************************************************/
fn RME_Bench_Prepare_Signals() {
    /* ---- Create the signal endpoints in the init capability table ---- */
    RME_Bench_Setup_Check(
        "SIG_CRT self endpoint",
        RME_Bench_Sig_Crt(RME_BENCH_CAP_SIG_SELF),
    );
    RME_Bench_Setup_Check(
        "SIG_CRT init process parking",
        RME_Bench_Sig_Crt(RME_BENCH_CAP_SIG_PARK),
    );
    RME_Bench_Setup_Check(
        "SIG_CRT data channel",
        RME_Bench_Sig_Crt(RME_BENCH_CAP_SIG_DATA),
    );
    RME_Bench_Setup_Check(
        "SIG_CRT process2 parking",
        RME_Bench_Sig_Crt(RME_BENCH_CAP_SIG_PARK2),
    );

    /* ---- Delegate the endpoints the second process needs ---- */
    RME_Bench_Setup_Check(
        "CPT_ADD data endpoint -> process2",
        RME_Cpt_Add(
            RME_BENCH_CAP_CPT,
            RME_BENCH_CAP2_SIG_DATA,
            RME_BOOT_INIT_CPT,
            RME_BENCH_CAP_SIG_DATA,
            RME_SIG_FLAG_ALL,
        ),
    );
    RME_Bench_Setup_Check(
        "CPT_ADD park2 endpoint -> process2",
        RME_Cpt_Add(
            RME_BENCH_CAP_CPT,
            RME_BENCH_CAP2_SIG_PARK,
            RME_BOOT_INIT_CPT,
            RME_BENCH_CAP_SIG_PARK2,
            RME_SIG_FLAG_ALL,
        ),
    );

    /* ---- Resolve the capability IDs ---- */
    unsafe {
        core::ptr::write_volatile(
            &raw mut RME_Bench_Cap_Sig_Self,
            RME_CID_Build(RME_BOOT_INIT_CPT, RME_BENCH_CAP_SIG_SELF),
        );
        core::ptr::write_volatile(
            &raw mut RME_Bench_Cap_Sig_Park,
            RME_CID_Build(RME_BOOT_INIT_CPT, RME_BENCH_CAP_SIG_PARK),
        );
        core::ptr::write_volatile(
            &raw mut RME_Bench_Cap_Sig_Park2,
            RME_CID_Build(RME_BOOT_INIT_CPT, RME_BENCH_CAP_SIG_PARK2),
        );
        core::ptr::write_volatile(
            &raw mut RME_Bench_Cap_Sig_Data,
            RME_CID_Build(RME_BOOT_INIT_CPT, RME_BENCH_CAP_SIG_DATA),
        );
    }
}
/* End Function:RME_Bench_Prepare_Signals **************************************/

/* Function:RME_Bench_Prepare_Invocation ***************************************
Description : Create the invocation stub that the invocation test uses. The
              stub enters the second process, so it runs with the second
              process' page table active.
Input       : None.
Output      : None.
Return      : None - A failed setup step halts the benchmark, so this
              function never returns in that case.
*****************************************************************************/
fn RME_Bench_Prepare_Invocation() {
    /* The invocation stub object lives in the kernel memory, right behind the
     * thread objects the preparation has allocated so far. */
    let Raddr = RME_Bench_Kom_Alloc_Inv();

    /* INV_CRT: Cid=Cap_Cpt, Param0=D1(Cap_Kom)|D0(Cap_Inv), Param1=Cap_Prc,
     * Param2=Raddr. The target process is the second process. */
    RME_Bench_Setup_Check(
        "INV_CRT round-trip stub",
        RME_Inv_Crt(
            RME_BOOT_INIT_CPT,
            RME_BOOT_INIT_KOM,
            RME_BENCH_CAP_INV,
            RME_BENCH_CAP_PRC,
            Raddr,
        ),
    );

    /* INV_SET: Param0=D0(Cap_Inv)|D1(Is_Exc_Ret), Param1=Entry, Param2=Stack.
     * The stack lives in the init image's bss, which the second process
     * maps physically one to one. */
    let Entry = RME_Bench_Inv_Entry as extern "C" fn(u64) -> ! as u64;
    let Stack = RME_Bench_Stack_Top(&raw const RME_Bench_Stack_INV);
    RME_Bench_Setup_Check(
        "INV_SET round-trip stub",
        RME_Inv_Set(
            RME_CID_Build(RME_BOOT_INIT_CPT, RME_BENCH_CAP_INV),
            Entry,
            Stack,
            0,
        ),
    );

    /* The second stub is used by the one-way INV_ACT test. */
    let Raddr2 = RME_Bench_Kom_Alloc_Inv();
    RME_Bench_Setup_Check(
        "INV_CRT one-way stub",
        RME_Inv_Crt(
            RME_BOOT_INIT_CPT,
            RME_BOOT_INIT_KOM,
            RME_BENCH_CAP_INV2,
            RME_BENCH_CAP_PRC,
            Raddr2,
        ),
    );
    let Entry2 = RME_Bench_Inv_Act_Entry as extern "C" fn(u64) -> ! as u64;
    let Stack2 = RME_Bench_Stack_Top(&raw const RME_Bench_Stack_INV2);
    RME_Bench_Setup_Check(
        "INV_SET one-way stub",
        RME_Inv_Set(
            RME_CID_Build(RME_BOOT_INIT_CPT, RME_BENCH_CAP_INV2),
            Entry2,
            Stack2,
            0,
        ),
    );

    /* KFN cap for the DEBUG_INV_MOD stub, which runs in process 2's namespace. */
    RME_Bench_Setup_Check(
        "CPT_ADD KFN -> process2",
        RME_Cpt_Add(
            RME_BENCH_CAP_CPT,
            RME_BENCH_CAP2_KFN,
            RME_BOOT_INIT_CPT,
            RME_BOOT_INIT_KFN,
            RME_KFN_FLAG_FULL_RANGE,
        ),
    );
}
/* End Function:RME_Bench_Prepare_Invocation ************************************/

/* Function:RME_Bench_Prepare_Threads ******************************************
Description : Start the helper threads. All of them are bound to the current
              core and get a share of the init thread's timeslice.
              The helper thread of the init process runs one priority level above
              the init thread and the receiver of process 2 runs two levels
              above it, so that a signal wakes them up right away and the
              init thread is not scheduled in their place.
              The switch target of process 2 runs on the init thread's own
              priority level, because a thread switch needs the very same
              priority on both sides.
Input       : None.
Output      : None.
Return      : None - A failed setup step halts the benchmark, so this
              function never returns in that case.
*****************************************************************************/
fn RME_Bench_Prepare_Threads() {
    /* The helper thread of the init process */
    let Cap_P1T2 = RME_CID_Build(RME_BOOT_INIT_CPT, RME_BENCH_CAP_THD);
    RME_Bench_Setup_Check(
        "THD_SCHED_BIND init process receiver",
        RME_Bench_Thd_Bind(Cap_P1T2, RME_BENCH_PRIO_RECV),
    );
    let Entry = RME_Bench_P1T2_Entry as extern "C" fn(u64) -> ! as u64;
    let Stack = RME_Bench_Stack_Top(&raw const RME_Bench_Stack_P1T2);
    RME_Bench_Setup_Check(
        "THD_EXEC_SET init process receiver",
        RME_Thd_Exec_Set(Cap_P1T2, Entry, Stack, 0),
    );

    /* The receiver thread of the second process */
    let Cap_P2T1 = RME_CID_Build(RME_BENCH_CAP_CPT, RME_BENCH_CAP2_THD);
    RME_Bench_Setup_Check(
        "THD_SCHED_BIND process2 receiver",
        RME_Bench_Thd_Bind(Cap_P2T1, RME_BENCH_PRIO_RECV2),
    );
    let Entry = RME_Bench_P2T1_Entry as extern "C" fn(u64) -> ! as u64;
    let Stack = RME_Bench_Stack_Top(&raw const RME_Bench_Stack_P2T1);
    RME_Bench_Setup_Check(
        "THD_EXEC_SET process2 receiver",
        RME_Thd_Exec_Set(Cap_P2T1, Entry, Stack, 0),
    );

    /* The switch target of the second process */
    let Cap_P2T2 = unsafe { core::ptr::read_volatile(&raw const RME_Bench_Cap_P2_THD) };
    RME_Bench_Setup_Check(
        "THD_SCHED_BIND process2 switch target",
        RME_Bench_Thd_Bind(Cap_P2T2, RME_BENCH_PRIO_INIT),
    );
    let Entry = RME_Bench_P2T2_Entry as extern "C" fn(u64) -> ! as u64;
    let Stack = RME_Bench_Stack_Top(&raw const RME_Bench_Stack_P2T2);
    RME_Bench_Setup_Check(
        "THD_EXEC_SET process2 switch target",
        RME_Thd_Exec_Set(Cap_P2T2, Entry, Stack, 0),
    );

    /* The switch target of the init process */
    let Cap_P1T3 = RME_CID_Build(RME_BOOT_INIT_CPT, RME_BENCH_CAP_THD2);
    RME_Bench_Setup_Check(
        "THD_SCHED_BIND init process switch target",
        RME_Bench_Thd_Bind(Cap_P1T3, RME_BENCH_PRIO_INIT),
    );
    let Entry = RME_Bench_P1T3_Entry as extern "C" fn(u64) -> ! as u64;
    let Stack = RME_Bench_Stack_Top(&raw const RME_Bench_Stack_P1T3);
    RME_Bench_Setup_Check(
        "THD_EXEC_SET init process switch target",
        RME_Thd_Exec_Set(Cap_P1T3, Entry, Stack, 0),
    );

    /* Give every helper a finite budget. The init thread intentionally stays
     * on its boot-time INIT_TIME budget; the helpers only need enough budget
     * to run until they park, and the higher-priority ones preempt the init
     * thread whenever a signal wakes them. */
    RME_Bench_Setup_Check(
        "THD_TIME_XFER init process receiver",
        RME_Thd_Time_Xfer(Cap_P1T2, RME_BOOT_INIT_THD, RME_BENCH_TIME_XFER),
    );
    RME_Bench_Setup_Check(
        "THD_TIME_XFER init process switch target",
        RME_Thd_Time_Xfer(Cap_P1T3, RME_BOOT_INIT_THD, RME_BENCH_TIME_XFER),
    );
    RME_Bench_Setup_Check(
        "THD_TIME_XFER process2 receiver",
        RME_Thd_Time_Xfer(Cap_P2T1, RME_BOOT_INIT_THD, RME_BENCH_TIME_XFER),
    );
    RME_Bench_Setup_Check(
        "THD_TIME_XFER process2 switch target",
        RME_Thd_Time_Xfer(Cap_P2T2, RME_BOOT_INIT_THD, RME_BENCH_TIME_XFER),
    );

    unsafe {
        core::ptr::write_volatile(&raw mut RME_Bench_Cap_P1T3, Cap_P1T3);
    }
}
/* End Function:RME_Bench_Prepare_Threads **************************************/

/* Function:RME_Bench_Wake_Helper **********************************************
Description : Publish a phase and wake the helper thread of the init process up.
              The helper thread runs at a higher priority, so the kernel
              switches to it right away and this call only returns once the
              helper thread has parked itself again.
Input       : Phase - The phase the helper thread shall run.
Output      : None.
Return      : rme_ptr_t - The kernel return value of SIG_SND (0 on success).
*****************************************************************************/
fn RME_Bench_Wake_Helper(Phase: u32) -> rme_ptr_t {
    let Cap_Park = unsafe { core::ptr::read_volatile(&raw const RME_Bench_Cap_Sig_Park) };

    RME_Bench_Phase_Set(Phase);
    RME_Sig_Snd(Cap_Park, 1)
}
/* End Function:RME_Bench_Wake_Helper *******************************************/

/* Function:RME_Bench_Wake_Helper2 *********************************************
Description : Publish a phase and wake the receiver thread of the second
              process up. That thread runs two priority levels above the
              init thread, so the kernel switches to it right away.
Input       : Phase - The phase the helper thread shall run.
Output      : None.
Return      : rme_ptr_t - The kernel return value of SIG_SND (0 on success).
*****************************************************************************/
fn RME_Bench_Wake_Helper2(Phase: u32) -> rme_ptr_t {
    let Cap_Park = unsafe { core::ptr::read_volatile(&raw const RME_Bench_Cap_Sig_Park2) };

    RME_Bench_Phase_Set(Phase);
    RME_Sig_Snd(Cap_Park, 1)
}
/* End Function:RME_Bench_Wake_Helper2 ******************************************/

/* Function:RME_Bench_Test_Yield_Self *******************************************
Description : Measure the cost of switching to the calling thread itself.
              The kernel notices that the target is the current thread and
              returns without a context switch, so this is the cheapest
              possible thread switch - the baseline of the whole suite.
Input       : Stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn RME_Bench_Test_Yield_Self(Stat: &mut RME_Bench_Stat) {
    let mut Count: u64 = 0;

    Stat.Reset();

    while Count < RME_BENCH_LOOP_NUM && Stat.Fail == 0 {
        let Start = RME_Bench_Read_Cycle();
        let _ = Stat.Set_Ret(RME_Thd_Swt(RME_BOOT_INIT_THD, 0) as i64);
        let End = RME_Bench_Read_Cycle();

        Stat.Record((End - Start) as u32);
        Count += 1;
    }
}
/* End Function:RME_Bench_Test_Yield_Self ****************************************/

/* Function:RME_Bench_Test_Signal_Self ******************************************
Description : Measure the cost of a signal that is sent to, and received
              from, the very same thread. The helper thread of the init process
              runs the test, because the kernel forbids a boot-time thread
              from receiving a signal at all.
Input       : Stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn RME_Bench_Test_Signal_Self(Stat: &mut RME_Bench_Stat) {
    /* Clear the shared slots before waking the helper: if the wake-up fails,
     * the test must see zero samples instead of the previous test's data. */
    Stat.Reset();
    RME_Bench_Stat_Reset();

    /* Wake the helper thread - it runs the whole test and parks itself
     * again before this call returns */
    if !RME_Bench_Setup_Try(
        Stat,
        "SIG_SND wake init helper",
        RME_Bench_Wake_Helper(RME_BENCH_PHASE_SIG_SELF),
    ) {
        return;
    }

    Stat.Load_Shared();
    Stat.Require_Count(RME_BENCH_LOOP_NUM);
}
/* End Function:RME_Bench_Test_Signal_Self **************************************/

/* Function:RME_Bench_Test_Sig_Snd **********************************************
Description : Send-only SIG_SND measurement. The endpoint has no receiver, so
              the call only increments the signal counter and does not switch.
Input       : Stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn RME_Bench_Test_Sig_Snd(Stat: &mut RME_Bench_Stat) {
    if !RME_Bench_Setup_Try(
        Stat,
        "SIG_CRT send-only",
        RME_Bench_Sig_Crt(RME_BENCH_CAP_SIG_SND_ONLY),
    ) {
        return;
    }

    Stat.Reset();

    let mut Count: u64 = 0;
    while Count < RME_BENCH_LOOP_NUM && Stat.Fail == 0 {
        let Start = RME_Bench_Read_Cycle();
        let _ = Stat.Set_Ret(RME_Sig_Snd(RME_BENCH_CAP_SIG_SND_ONLY, 1) as i64);
        let End = RME_Bench_Read_Cycle();

        Stat.Record((End - Start) as u32);
        Count += 1;
    }

    let _ = RME_Cpt_Frz(RME_BOOT_INIT_CPT, RME_BENCH_CAP_SIG_SND_ONLY);
    let _ = RME_Sig_Del(RME_BOOT_INIT_CPT, RME_BENCH_CAP_SIG_SND_ONLY);
}
/* End Function:RME_Bench_Test_Sig_Snd *******************************************/

/* Function:RME_Bench_Test_Sig_Rcv_Only *****************************************
Description : Receive-only SIG_RCV measurement, executed by the helper thread
              because a boot thread may not receive signals.
Input       : Stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn RME_Bench_Test_Sig_Rcv_Only(Stat: &mut RME_Bench_Stat) {
    if !RME_Bench_Setup_Try(
        Stat,
        "SIG_CRT receive-only",
        RME_Bench_Sig_Crt(RME_BENCH_CAP_SIG_RCV_ONLY),
    ) {
        return;
    }

    /* Clear the shared slots before waking the helper: a failed wake-up
     * must not leave the previous helper test's data visible. */
    Stat.Reset();
    RME_Bench_Stat_Reset();
    if !RME_Bench_Setup_Try(
        Stat,
        "SIG_SND wake init helper",
        RME_Bench_Wake_Helper(RME_BENCH_PHASE_SIG_RCV_ONLY),
    ) {
        return;
    }
    Stat.Load_Shared();
    Stat.Require_Count(RME_BENCH_LOOP_NUM);

    let _ = RME_Cpt_Frz(RME_BOOT_INIT_CPT, RME_BENCH_CAP_SIG_RCV_ONLY);
    let _ = RME_Sig_Del(RME_BOOT_INIT_CPT, RME_BENCH_CAP_SIG_RCV_ONLY);
}
/* End Function:RME_Bench_Test_Sig_Rcv_Only *************************************/

/* Function:RME_Bench_Test_Invocation *******************************************
Description : Measure the cost of a synchronous invocation. The invoker
              activates the stub, which runs in the second process'
              address space and returns immediately, so this is the full
              round trip through the kernel.
              The direct INV_RET test leaves its stub installed here.
Input       : Stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn RME_Bench_Test_Invocation(Stat: &mut RME_Bench_Stat) {
    let Cap_Inv = RME_CID_Build(RME_BOOT_INIT_CPT, RME_BENCH_CAP_INV);
    let Entry = RME_Bench_Inv_Entry as extern "C" fn(u64) -> ! as u64;
    let Stack = RME_Bench_Stack_Top(&raw const RME_Bench_Stack_INV);
    if !RME_Bench_Setup_Try(
        Stat,
        "INV_SET round-trip stub",
        RME_Inv_Set(Cap_Inv, Entry, Stack, 0),
    ) {
        return;
    }

    let mut Count: u64 = 0;

    Stat.Reset();

    while Count < RME_BENCH_LOOP_NUM && Stat.Fail == 0 {
        let Start = RME_Bench_Read_Cycle();
        let Ret = RME_Inv_Act(Cap_Inv, 0x1234);
        let End = RME_Bench_Read_Cycle();

        /* a0 is the activation status; a1 (callee return) is not printed. */
        let _ = Stat.Set_Ret(Ret.Status());
        Stat.Record((End - Start) as u32);
        Count += 1;
    }
}
/* End Function:RME_Bench_Test_Invocation ****************************************/

/* Function:RME_Bench_Test_Inv_Act **********************************************
Description : One-way INV_ACT measurement. The stub latches its entry time and
              returns; the invoker reads that time after the call, so the
              measurement covers entry only.
Input       : Stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn RME_Bench_Test_Inv_Act(Stat: &mut RME_Bench_Stat) {
    let Cap_Inv2 = RME_CID_Build(RME_BOOT_INIT_CPT, RME_BENCH_CAP_INV2);
    let Entry = RME_Bench_Inv_Act_Entry as extern "C" fn(u64) -> ! as u64;
    let Stack = RME_Bench_Stack_Top(&raw const RME_Bench_Stack_INV2);
    if !RME_Bench_Setup_Try(
        Stat,
        "INV_SET one-way stub",
        RME_Inv_Set(Cap_Inv2, Entry, Stack, 0),
    ) {
        return;
    }

    Stat.Reset();

    let mut Count: u64 = 0;
    while Count < RME_BENCH_LOOP_NUM && Stat.Fail == 0 {
        RME_Bench_Start_Set(RME_Bench_Read_Cycle());
        let Ret = RME_Inv_Act(Cap_Inv2, 0x1111);

        let _ = Stat.Set_Ret(Ret.Status());

        Stat.Record((RME_Bench_End_Get() - RME_Bench_Start_Get()) as u32);
        Count += 1;
    }
}
/* End Function:RME_Bench_Test_Inv_Act *******************************************/

/* Function:RME_Bench_Test_Inv_Ret **********************************************
Description : Direct cross-process INV_RET measurement; includes the satp
              switch back to the invoker.
Input       : Stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn RME_Bench_Test_Inv_Ret(Stat: &mut RME_Bench_Stat) {
    let Cap_Inv = RME_CID_Build(RME_BOOT_INIT_CPT, RME_BENCH_CAP_INV);
    let Entry = RME_Bench_Inv_Ret_Entry as extern "C" fn(u64) -> ! as u64;
    let Stack = RME_Bench_Stack_Top(&raw const RME_Bench_Stack_INV);
    if !RME_Bench_Setup_Try(
        Stat,
        "INV_SET direct-return stub",
        RME_Inv_Set(Cap_Inv, Entry, Stack, 0),
    ) {
        return;
    }

    Stat.Reset();

    let mut Count: u64 = 0;
    while Count < RME_BENCH_LOOP_NUM && Stat.Fail == 0 {
        let Ret = RME_Inv_Act(Cap_Inv, 0);
        let End = RME_Bench_Read_Cycle();

        /* a0 is the activation status; a1 is the callee's INV_RET value. */
        let _ = Stat.Set_Ret(Ret.Status());

        Stat.Record((End - RME_Bench_Start_Get()) as u32);
        Count += 1;
    }
}
/* End Function:RME_Bench_Test_Inv_Ret *******************************************/

/* Function:RME_Bench_Test_Signal_Intra *****************************************
Description : Measure the cost of a signal that travels between two threads
              of the same process. The init thread is the sender, the helper
              thread of the init process is the receiver.
Input       : Stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn RME_Bench_Test_Signal_Intra(Stat: &mut RME_Bench_Stat) {
    let Cap_Data = unsafe { core::ptr::read_volatile(&raw const RME_Bench_Cap_Sig_Data) };
    let mut Count: u64 = 0;
    let mut Send_Fail = false;

    /* Clear the shared slots before waking the helper: a failed wake-up
     * must not expose the previous helper test's data. */
    Stat.Reset();
    RME_Bench_Stat_Reset();

    /* Let the helper thread take the receiving role and wait for the
     * workload on the data channel */
    if !RME_Bench_Setup_Try(
        Stat,
        "SIG_SND wake init helper",
        RME_Bench_Wake_Helper(RME_BENCH_PHASE_SIG_RECV),
    ) {
        return;
    }

    while Count < RME_BENCH_LOOP_NUM && !RME_Bench_Stat_Failed() {
        RME_Bench_Start_Set(RME_Bench_Read_Cycle());
        let Ret = RME_Sig_Snd(Cap_Data, 1);
        if (Ret as i64) < 0 {
            Send_Fail = true;
            break;
        }
        Count += 1;
    }

    Stat.Reset();
    Stat.Load_Shared();
    if Send_Fail || Count != RME_BENCH_LOOP_NUM {
        let _ = Stat.Set_Ret(-1);
    }
    Stat.Require_Count(RME_BENCH_LOOP_NUM);
}
/* End Function:RME_Bench_Test_Signal_Intra *************************************/

/* Function:RME_Bench_Test_Signal_Inter *****************************************
Description : Measure the cost of a signal that travels between two threads
              of two different processes. The init thread is the sender, the
              receiver thread of the second process is the receiver.
Input       : Stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn RME_Bench_Test_Signal_Inter(Stat: &mut RME_Bench_Stat) {
    let Cap_Data = unsafe { core::ptr::read_volatile(&raw const RME_Bench_Cap_Sig_Data) };
    let mut Count: u64 = 0;
    let mut Send_Fail = false;

    /* Clear the shared slots before waking the helper: a failed wake-up
     * must not expose the previous helper test's data. */
    Stat.Reset();
    RME_Bench_Stat_Reset();

    /* Let the receiver in the second process take the receiving role and
     * wait for the workload on the data channel */
    if !RME_Bench_Setup_Try(
        Stat,
        "SIG_SND wake process2 helper",
        RME_Bench_Wake_Helper2(RME_BENCH_PHASE_SIG_RECV2),
    ) {
        return;
    }

    while Count < RME_BENCH_LOOP_NUM && !RME_Bench_Stat_Failed() {
        RME_Bench_Start_Set(RME_Bench_Read_Cycle());
        let Ret = RME_Sig_Snd(Cap_Data, 1);
        if (Ret as i64) < 0 {
            Send_Fail = true;
            break;
        }
        Count += 1;
    }

    Stat.Reset();
    Stat.Load_Shared();
    if Send_Fail || Count != RME_BENCH_LOOP_NUM {
        let _ = Stat.Set_Ret(-1);
    }
    Stat.Require_Count(RME_BENCH_LOOP_NUM);
}
/* End Function:RME_Bench_Test_Signal_Inter *************************************/

/* Function:RME_Bench_Test_Yield_Intra *******************************************
Description : Measure the cost of switching between two threads of the same
              process. The switch is one way: the target thread latches the
              arrival timestamp and switches straight back, so the
              measurement does not contain the cost of the way back.
              This is the counterpart of the inter-process yield test, and
              it covers the path the C rme_benchmark.c measured with
              RME_Same_Prc_Thd_Switch_Test.
Input       : Stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn RME_Bench_Test_Yield_Intra(Stat: &mut RME_Bench_Stat) {
    let Cap_P1T3 = unsafe { core::ptr::read_volatile(&raw const RME_Bench_Cap_P1T3) };
    let mut Count: u64 = 0;

    Stat.Reset();

    while Count < RME_BENCH_LOOP_NUM && Stat.Fail == 0 {
        RME_Bench_Start_Set(RME_Bench_Read_Cycle());
        let _ = Stat.Set_Ret(RME_Thd_Swt(Cap_P1T3, 0) as i64);

        Stat.Record((RME_Bench_End_Get() - RME_Bench_Start_Get()) as u32);
        Count += 1;
    }
}
/* End Function:RME_Bench_Test_Yield_Intra **************************************/

/* Function:RME_Bench_Test_Yield_Intra_Roundtrip ********************************
Description : Full intra-process switch round trip: init switches to the
              target, the target switches back, and init reads the cycle
              counter after THD_SWT returns.
*****************************************************************************/
fn RME_Bench_Test_Yield_Intra_Roundtrip(Stat: &mut RME_Bench_Stat) {
    let Cap_P1T3 = unsafe { core::ptr::read_volatile(&raw const RME_Bench_Cap_P1T3) };
    let mut Count: u64 = 0;

    Stat.Reset();

    while Count < RME_BENCH_LOOP_NUM && Stat.Fail == 0 {
        let Start = RME_Bench_Read_Cycle();
        let Ret = RME_Thd_Swt(Cap_P1T3, 0) as i64;
        let End = RME_Bench_Read_Cycle();

        let _ = Stat.Set_Ret(Ret);
        Stat.Record((End - Start) as u32);
        Count += 1;
    }
}
/* End Function:RME_Bench_Test_Yield_Intra_Roundtrip ****************************/

/* Function:RME_Bench_Test_Yield_Inter ******************************************
Description : Measure the cost of switching into another process. The switch
              is one way: the target thread latches the arrival timestamp
              and switches straight back, so the measurement does not contain
              the cost of the way back.
Input       : Stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn RME_Bench_Test_Yield_Inter(Stat: &mut RME_Bench_Stat) {
    let Cap_P2T2 = unsafe { core::ptr::read_volatile(&raw const RME_Bench_Cap_P2_THD) };
    let mut Count: u64 = 0;

    Stat.Reset();

    while Count < RME_BENCH_LOOP_NUM && Stat.Fail == 0 {
        RME_Bench_Start_Set(RME_Bench_Read_Cycle());
        let _ = Stat.Set_Ret(RME_Thd_Swt(Cap_P2T2, 0) as i64);

        Stat.Record((RME_Bench_End_Get() - RME_Bench_Start_Get()) as u32);
        Count += 1;
    }
}
/* End Function:RME_Bench_Test_Yield_Inter *************************************/

/* Function:RME_Bench_Test_Yield_Inter_Roundtrip ********************************
Description : Full inter-process switch round trip: init switches to the
              process-2 target, the target switches back, and init reads the
              cycle counter after THD_SWT returns.
*****************************************************************************/
fn RME_Bench_Test_Yield_Inter_Roundtrip(Stat: &mut RME_Bench_Stat) {
    let Cap_P2T2 = unsafe { core::ptr::read_volatile(&raw const RME_Bench_Cap_P2_THD) };
    let mut Count: u64 = 0;

    Stat.Reset();

    while Count < RME_BENCH_LOOP_NUM && Stat.Fail == 0 {
        let Start = RME_Bench_Read_Cycle();
        let Ret = RME_Thd_Swt(Cap_P2T2, 0) as i64;
        let End = RME_Bench_Read_Cycle();

        let _ = Stat.Set_Ret(Ret);
        Stat.Record((End - Start) as u32);
        Count += 1;
    }
}
/* End Function:RME_Bench_Test_Yield_Inter_Roundtrip ****************************/

/* Function:RME_Bench_Test_Kfn_Pgt_Entry_Mod *************************************
Description : Measure KFN PGT_ENTRY_MOD: walk the init page table at
              0x20000000 and query flags/size/num order. The KFN handler
              clears a0 on success, so ret is 0, not the queried value.
Input       : Stat - The statistics accumulator.
              Operation - FLAG_GET, SZORD_GET or NMORD_GET.
Output      : None.
Return      : None.
*****************************************************************************/
fn RME_Bench_Test_Kfn_Pgt_Entry_Mod(Stat: &mut RME_Bench_Stat, Operation: rme_ptr_t) {
    Stat.Reset();

    let mut Count: u64 = 0;
    while Count < RME_BENCH_LOOP_NUM && Stat.Fail == 0 {
        let Start = RME_Bench_Read_Cycle();
        let _ = Stat.Set_Ret(RME_Kfn(
            RME_BOOT_INIT_KFN,
            RME_KFN_PGT_ENTRY_MOD,
            RME_BOOT_INIT_PGT,
            0x20000000,
            Operation,
        ) as i64);
        let End = RME_Bench_Read_Cycle();

        Stat.Record((End - Start) as u32);
        Count += 1;
    }
}
/* End Function:RME_Bench_Test_Kfn_Pgt_Entry_Mod *********************************/

/* Function:RME_Bench_Test_Kfn_Pgt_Asid_Set *************************************
Description : Measure KFN PGT_ASID_SET on process 2's root page table. The
              ASID alternates between the one assigned to process 2 and a
              caller-allocated scratch value, so every round flushes the old
              and the new ASID before the change. Process 2 is not running
              while this test executes, so borrowing its root table is safe.
Input       : Stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn RME_Bench_Test_Kfn_Pgt_Asid_Set(Stat: &mut RME_Bench_Stat) {
    Stat.Reset();

    let P2_Asid = unsafe { core::ptr::read_volatile(&raw const RME_Bench_Asid_P2) };
    let Scratch_Asid = RME_Bench_Asid_Alloc();
    let mut Asid = Scratch_Asid;
    let mut Count: u64 = 0;

    while Count < RME_BENCH_LOOP_NUM && Stat.Fail == 0 {
        let Start = RME_Bench_Read_Cycle();
        let Ret = RME_Kfn(
            RME_BOOT_INIT_KFN,
            RME_KFN_PGT_ASID_SET,
            RME_BENCH_CAP_PGT_TOP_NEW,
            Asid,
            0,
        );
        let End = RME_Bench_Read_Cycle();

        let _ = Stat.Set_Ret(Ret as i64);
        Stat.Record((End - Start) as u32);

        Asid = if Asid == Scratch_Asid {
            P2_Asid
        } else {
            Scratch_Asid
        };
        Count += 1;
    }

    /* Leave process 2 on its own ASID regardless of the loop parity. */
    let _ = RME_Kfn(
        RME_BOOT_INIT_KFN,
        RME_KFN_PGT_ASID_SET,
        RME_BENCH_CAP_PGT_TOP_NEW,
        P2_Asid,
        0,
    );
}
/* End Function:RME_Bench_Test_Kfn_Pgt_Asid_Set *********************************/

/* Function:RME_Bench_Test_Kfn_Pgt_Cache_Clr *************************************
Description : Measure KFN PGT_CACHE_CLR on the init process's root page table.
              The KFN selects the table through Sub_ID and flushes that table's
              ASID, so the timed window contains the capability lookup and one
              local sfence.vma.
Input       : Stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn RME_Bench_Test_Kfn_Pgt_Cache_Clr(Stat: &mut RME_Bench_Stat) {
    Stat.Reset();

    let mut Count: u64 = 0;
    while Count < RME_BENCH_LOOP_NUM && Stat.Fail == 0 {
        let Start = RME_Bench_Read_Cycle();
        let Ret = RME_Kfn(
            RME_BOOT_INIT_KFN,
            RME_KFN_PGT_CACHE_CLR,
            RME_BOOT_INIT_PGT,
            0,
            0,
        );
        let End = RME_Bench_Read_Cycle();

        let _ = Stat.Set_Ret(Ret as i64);
        Stat.Record((End - Start) as u32);
        Count += 1;
    }
}
/* End Function:RME_Bench_Test_Kfn_Pgt_Cache_Clr *********************************/

/* Function:RME_Bench_Test_Kfn_Pgt_Line_Clr **************************************
Description : Measure KFN PGT_LINE_CLR on the init image page. The KFN selects
              the root page table through Sub_ID and flushes one virtual
              address of that table's ASID.
Input       : Stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn RME_Bench_Test_Kfn_Pgt_Line_Clr(Stat: &mut RME_Bench_Stat) {
    Stat.Reset();

    let mut Count: u64 = 0;
    while Count < RME_BENCH_LOOP_NUM && Stat.Fail == 0 {
        let Start = RME_Bench_Read_Cycle();
        let Ret = RME_Kfn(
            RME_BOOT_INIT_KFN,
            RME_KFN_PGT_LINE_CLR,
            RME_BOOT_INIT_PGT, /* Sub_ID: top-level page table */
            0x20000000,        /* Param1: init image window */
            0,                 /* unused */
        );
        let End = RME_Bench_Read_Cycle();

        let _ = Stat.Set_Ret(Ret as i64);
        Stat.Record((End - Start) as u32);
        Count += 1;
    }
}
/* End Function:RME_Bench_Test_Kfn_Pgt_Line_Clr *********************************/

/* Function:RME_Bench_Test_Kfn_Cache_Maint **************************************
Description : Measure one KFN CACHE_MAINT operation on the first cache block
              of a mapped static buffer. ICACHE uses fence.i; DCACHE uses
              Zicbom clean/inval/flush.
Input       : Stat - The statistics accumulator.
              Cache_ID - RME_RV64V_KFN_CACHE_ICACHE or DCACHE.
              Operation - The cache maintenance operation.
Output      : None.
Return      : None.
*****************************************************************************/
fn RME_Bench_Test_Kfn_Cache_Maint(
    Stat: &mut RME_Bench_Stat,
    Cache_ID: rme_ptr_t,
    Operation: rme_ptr_t,
) {
    let Addr = (unsafe { core::ptr::addr_of!(RME_Bench_Stack_TEST[0]) as u64 })
        & !(RME_RV64V_CACHE_BLOCK_SIZE - 1);

    Stat.Reset();

    let mut Count: u64 = 0;
    while Count < RME_BENCH_LOOP_NUM && Stat.Fail == 0 {
        let Start = RME_Bench_Read_Cycle();
        let _ = Stat.Set_Ret(RME_Kfn(
            RME_BOOT_INIT_KFN,
            RME_KFN_CACHE_MAINT,
            Cache_ID,
            Operation,
            Addr,
        ) as i64);
        let End = RME_Bench_Read_Cycle();

        Stat.Record((End - Start) as u32);
        Count += 1;
    }
}
/* End Function:RME_Bench_Test_Kfn_Cache_Maint **********************************/

/* Function:RME_Bench_Test_Kfn_Debug_Print **************************************
Description : Measure KFN DEBUG_PRINT: one SBI console character per round,
              so the result includes the console cost.
Input       : Stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn RME_Bench_Test_Kfn_Debug_Print(Stat: &mut RME_Bench_Stat) {
    Stat.Reset();

    let mut Count: u64 = 0;
    while Count < RME_BENCH_LOOP_REDUCED_NUM && Stat.Fail == 0 {
        let Start = RME_Bench_Read_Cycle();
        let _ = Stat.Set_Ret(RME_Kfn(
            RME_BOOT_INIT_KFN,
            RME_KFN_DEBUG_PRINT,
            b'.' as rme_ptr_t,
            0,
            0,
        ) as i64);
        let End = RME_Bench_Read_Cycle();

        Stat.Record((End - Start) as u32);
        Count += 1;
    }

    /* End the dots line before the report line. */
    RME_Bench_Print_Str("\r\n");
}
/* End Function:RME_Bench_Test_Kfn_Debug_Print **********************************/

/* Function:RME_Bench_Test_Kfn_Debug_Reg_Mod ************************************
Description : Measure KFN DEBUG_REG_MOD: read the calling thread's A0. The
              value is returned in a1 (not captured); the KFN returns 0.
Input       : Stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn RME_Bench_Test_Kfn_Debug_Reg_Mod(Stat: &mut RME_Bench_Stat) {
    Stat.Reset();

    let mut Count: u64 = 0;
    while Count < RME_BENCH_LOOP_NUM && Stat.Fail == 0 {
        let Start = RME_Bench_Read_Cycle();
        let _ = Stat.Set_Ret(RME_Kfn(
            RME_BOOT_INIT_KFN,
            RME_KFN_DEBUG_REG_MOD,
            RME_BOOT_INIT_THD,
            RME_RV64V_KFN_DEBUG_REG_MOD_X10_A0,
            0,
        ) as i64);
        let End = RME_Bench_Read_Cycle();

        Stat.Record((End - Start) as u32);
        Count += 1;
    }
}
/* End Function:RME_Bench_Test_Kfn_Debug_Reg_Mod ********************************/

/* Function:RME_Bench_Test_Kfn_Debug_Exc_Get ************************************
Description : Measure KFN DEBUG_EXC_GET: read the calling thread's exception
              cause/addr/value. The value is returned in a2 (not captured);
              the KFN returns 0.
Input       : Stat - The statistics accumulator.
              Operation - CAUSE_GET, ADDR_GET or VALUE_GET.
Output      : None.
Return      : None.
*****************************************************************************/
fn RME_Bench_Test_Kfn_Debug_Exc_Get(Stat: &mut RME_Bench_Stat, Operation: rme_ptr_t) {
    Stat.Reset();

    let mut Count: u64 = 0;
    while Count < RME_BENCH_LOOP_NUM && Stat.Fail == 0 {
        let Start = RME_Bench_Read_Cycle();
        let _ = Stat.Set_Ret(RME_Kfn(
            RME_BOOT_INIT_KFN,
            RME_KFN_DEBUG_EXC_GET,
            RME_BOOT_INIT_THD,
            Operation,
            0,
        ) as i64);
        let End = RME_Bench_Read_Cycle();

        Stat.Record((End - Start) as u32);
        Count += 1;
    }
}
/* End Function:RME_Bench_Test_Kfn_Debug_Exc_Get ********************************/

/* Function:RME_Bench_Test_Kfn_Debug_Inv_Mod ************************************
Description : Measure KFN DEBUG_INV_MOD layer-0 SP query from inside an
              invocation. The stub measures the KFN; the invoker reads the
              shared timestamps and return value.
Input       : Stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn RME_Bench_Test_Kfn_Debug_Inv_Mod(Stat: &mut RME_Bench_Stat) {
    let Cap_Inv = RME_CID_Build(RME_BOOT_INIT_CPT, RME_BENCH_CAP_INV);
    let Stack = RME_Bench_Stack_Top(&raw const RME_Bench_Stack_INV);
    let Entry = RME_Bench_Inv_Dbg_Entry as extern "C" fn(u64) -> ! as u64;
    if !RME_Bench_Setup_Try(
        Stat,
        "INV_SET debug stub",
        RME_Inv_Set(Cap_Inv, Entry, Stack, 0),
    ) {
        return;
    }
    Stat.Reset();

    let mut Count: u64 = 0;
    while Count < RME_BENCH_LOOP_NUM && Stat.Fail == 0 {
        unsafe {
            core::ptr::write_volatile(&raw mut RME_Bench_Kfn_Inv_Ret, i64::MIN);
        }

        let Ret = RME_Inv_Act(Cap_Inv, 0);
        let Start = RME_Bench_Start_Get();
        let End = RME_Bench_End_Get();
        let Kfn_Ret = unsafe { core::ptr::read_volatile(&raw const RME_Bench_Kfn_Inv_Ret) };

        if !Ret.Ok() {
            let _ = Stat.Set_Ret(Ret.Status());
        } else if Kfn_Ret == i64::MIN {
            /* The stub never ran and published a result. */
            let _ = Stat.Set_Ret(-1);
        } else {
            let _ = Stat.Set_Ret(Kfn_Ret);
        }

        Stat.Record((End - Start) as u32);
        Count += 1;
    }

    /* Restore the shared stub (this test runs last). */
    let Entry = RME_Bench_Inv_Entry as extern "C" fn(u64) -> ! as u64;
    let _ = RME_Inv_Set(Cap_Inv, Entry, Stack, 0);
}
/* End Function:RME_Bench_Test_Kfn_Debug_Inv_Mod ********************************/

/* Function:RME_Bench_Test_Kfn_Idle_Sleep ***************************************
Description : Measure KFN IDLE_SLEEP. The kernel executes WFI and returns on
              the next timer tick, so the result should include the sleep
              latency. The cycle CSR is also recorded for comparison, but on
              cores that clock-gate the hart in WFI (SpacemiT K3/X100) it does
              not advance during the sleep and therefore only shows the SVC
              round trip; the time CSR is the authoritative number there.
Input       : stat - The cycle statistics accumulator.
              Time_Stat - The time (timebase tick) statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn RME_Bench_Test_Kfn_Idle_Sleep(Stat: &mut RME_Bench_Stat, Time_Stat: &mut RME_Bench_Stat) {
    Stat.Reset();
    Time_Stat.Reset();

    let mut Count: u64 = 0;
    while Count < RME_BENCH_LOOP_REDUCED_NUM && Stat.Fail == 0 {
        let Start = RME_Bench_Read_Cycle();
        let Time_Start = RME_Bench_Read_Time();
        let Ret = RME_Kfn(RME_BOOT_INIT_KFN, RME_KFN_IDLE_SLEEP, 0, 0, 0) as i64;
        let Time_End = RME_Bench_Read_Time();
        let End = RME_Bench_Read_Cycle();

        let _ = Stat.Set_Ret(Ret);
        let _ = Time_Stat.Set_Ret(Ret);
        Stat.Record((End - Start) as u32);
        Time_Stat.Record((Time_End - Time_Start) as u32);
        Count += 1;
    }
}
/* End Function:RME_Bench_Test_Kfn_Idle_Sleep ***********************************/

/* Function:RME_Benchmark ***************************************************
Description : The benchmark entry, also the init thread. It runs every test
              of the suite and prints the results in the order of the
              reference benchmark.
Input       : None.
Output      : None.
Return      : ! - This function never returns.
*****************************************************************************/
pub fn RME_Benchmark() -> ! {
    let mut Stat = RME_Bench_Stat::New();
    let mut Time_Stat = RME_Bench_Stat::New();

    /* Print table header - the banner is generated with
     * "figlet -f smslant -k RME" */
    RME_Bench_Print_Str("\r\n");
    RME_Bench_Print_Str("   ___   __  ___ ____\r\n");
    RME_Bench_Print_Str("  / _ \\ /  |/  // __/\r\n");
    RME_Bench_Print_Str(" / , _// /|_/ // _/\r\n");
    RME_Bench_Print_Str("/_/|_|/_/  /_//___/\r\n");
    RME_Bench_Print_Str("RME Concord Microkernel - Standard benchmark test\r\n");
    RME_Bench_Print_Str("====================================================\r\n");
    RME_BENCH_PRINT!(
        "{:<44} : AVG / MAX / MIN / ret\r\n",
        "Test (number in CPU cycles)"
    );

    /* The caller owns the ASID assignment of the address spaces it creates.
     * The init address space is tagged by the kernel, so only the second
     * process needs one here. */

    /* Prepare the shared fixtures. A failed preparation step means that the
     * measurement environment is broken, so the benchmark is halted right
     * there: the remaining tests would only produce misleading numbers. */
    RME_Bench_Prepare_Process2();
    RME_Bench_Prepare_Signals();
    RME_Bench_Prepare_Invocation();
    RME_Bench_Prepare_Threads();

    /* ---- THD_SWT: switch variants ---- */
    RME_Bench_Test_Yield_Self(&mut Stat);
    RME_Bench_Report("THD_SWT(self)", &Stat);

    RME_Bench_Test_Yield_Intra(&mut Stat);
    RME_Bench_Report("THD_SWT(intra, one-way)", &Stat);
    RME_Bench_Test_Yield_Intra_Roundtrip(&mut Stat);
    RME_Bench_Report("THD_SWT(intra, round-trip)", &Stat);

    RME_Bench_Test_Yield_Inter(&mut Stat);
    RME_Bench_Report("THD_SWT(inter, one-way)", &Stat);
    RME_Bench_Test_Yield_Inter_Roundtrip(&mut Stat);
    RME_Bench_Report("THD_SWT(inter, round-trip)", &Stat);

    /* ---- Signal ---- */
    RME_Bench_Test_Sig_Snd(&mut Stat);
    RME_Bench_Report("SIG_SND", &Stat);

    RME_Bench_Test_Sig_Rcv_Only(&mut Stat);
    RME_Bench_Report("SIG_RCV", &Stat);
    RME_Bench_Test_Signal_Self(&mut Stat);
    RME_Bench_Report("SIG_SND+SIG_RCV(self)", &Stat);

    RME_Bench_Test_Signal_Intra(&mut Stat);
    RME_Bench_Report("SIG_SND+SIG_RCV(intra)", &Stat);

    RME_Bench_Test_Signal_Inter(&mut Stat);
    RME_Bench_Report("SIG_SND+SIG_RCV(inter)", &Stat);

    /* ---- Invocation ---- */
    RME_Bench_Test_Inv_Act(&mut Stat);
    RME_Bench_Report("INV_ACT(inter, one-way)", &Stat);
    RME_Bench_Test_Inv_Ret(&mut Stat);
    RME_Bench_Report("INV_RET(inter, one-way)", &Stat);
    RME_Bench_Test_Invocation(&mut Stat);
    RME_Bench_Report("INV_ACT+INV_RET(inter, round-trip)", &Stat);

    /* ---- KFN: testable success paths ---- */
    RME_Bench_Test_Kfn_Pgt_Entry_Mod(&mut Stat, RME_RV64V_KFN_PGT_ENTRY_MOD_FLAG_GET);
    RME_Bench_Report("KFN(PGT_ENTRY_MOD flag)", &Stat);
    RME_Bench_Test_Kfn_Pgt_Entry_Mod(&mut Stat, RME_RV64V_KFN_PGT_ENTRY_MOD_SZORD_GET);
    RME_Bench_Report("KFN(PGT_ENTRY_MOD size)", &Stat);
    RME_Bench_Test_Kfn_Pgt_Entry_Mod(&mut Stat, RME_RV64V_KFN_PGT_ENTRY_MOD_NMORD_GET);
    RME_Bench_Report("KFN(PGT_ENTRY_MOD num)", &Stat);

    RME_Bench_Test_Kfn_Pgt_Asid_Set(&mut Stat);
    RME_Bench_Report("KFN(PGT_ASID_SET)", &Stat);

    RME_Bench_Test_Kfn_Pgt_Cache_Clr(&mut Stat);
    RME_Bench_Report("KFN(PGT_CACHE_CLR)", &Stat);
    RME_Bench_Test_Kfn_Pgt_Line_Clr(&mut Stat);
    RME_Bench_Report("KFN(PGT_LINE_CLR)", &Stat);

    RME_Bench_Test_Kfn_Cache_Maint(
        &mut Stat,
        RME_RV64V_KFN_CACHE_ICACHE,
        RME_RV64V_KFN_CACHE_INV_ALL,
    );
    RME_Bench_Report("KFN(CACHE_MAINT icache)", &Stat);
    RME_Bench_Test_Kfn_Cache_Maint(
        &mut Stat,
        RME_RV64V_KFN_CACHE_DCACHE,
        RME_RV64V_KFN_CACHE_CLEAN_ADDR,
    );
    RME_Bench_Report("KFN(CACHE_MAINT dclean)", &Stat);
    RME_Bench_Test_Kfn_Cache_Maint(
        &mut Stat,
        RME_RV64V_KFN_CACHE_DCACHE,
        RME_RV64V_KFN_CACHE_INV_ADDR,
    );
    RME_Bench_Report("KFN(CACHE_MAINT dinval)", &Stat);
    RME_Bench_Test_Kfn_Cache_Maint(
        &mut Stat,
        RME_RV64V_KFN_CACHE_DCACHE,
        RME_RV64V_KFN_CACHE_CLEAN_INV_ADDR,
    );
    RME_Bench_Report("KFN(CACHE_MAINT dflush)", &Stat);

    RME_Bench_Test_Kfn_Debug_Print(&mut Stat);
    RME_Bench_Report("KFN(DEBUG_PRINT)", &Stat);

    RME_Bench_Test_Kfn_Debug_Reg_Mod(&mut Stat);
    RME_Bench_Report("KFN(DEBUG_REG_MOD get)", &Stat);

    RME_Bench_Test_Kfn_Debug_Exc_Get(&mut Stat, RME_RV64V_KFN_DEBUG_EXC_CAUSE_GET);
    RME_Bench_Report("KFN(DEBUG_EXC_GET cause)", &Stat);
    RME_Bench_Test_Kfn_Debug_Exc_Get(&mut Stat, RME_RV64V_KFN_DEBUG_EXC_ADDR_GET);
    RME_Bench_Report("KFN(DEBUG_EXC_GET addr)", &Stat);
    RME_Bench_Test_Kfn_Debug_Exc_Get(&mut Stat, RME_RV64V_KFN_DEBUG_EXC_VALUE_GET);
    RME_Bench_Report("KFN(DEBUG_EXC_GET value)", &Stat);

    RME_Bench_Test_Kfn_Debug_Inv_Mod(&mut Stat);
    RME_Bench_Report("KFN(DEBUG_INV_MOD sp)", &Stat);

    RME_Bench_Test_Kfn_Idle_Sleep(&mut Stat, &mut Time_Stat);
    RME_Bench_Report("KFN(IDLE_SLEEP tick, cycle CSR)", &Stat);
    RME_Bench_Report("KFN(IDLE_SLEEP tick, time CSR)", &Time_Stat);

    RME_Bench_Print_Str("\r\nRME BENCH REPORT OK\r\n");

    /* Idle forever - busy loop */
    loop {
        core::hint::spin_loop();
    }
}
/* End Function:RME_Benchmark ***********************************************/

/* End Of File ***************************************************************/

/* Copyright (C) Evo-Devo Instrum. All rights reserved ***********************/
