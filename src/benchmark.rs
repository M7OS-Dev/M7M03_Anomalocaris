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
/* This module mirrors the whole C bench_main.c. Not every constant is
 * used by every platform, so dead-code warnings are suppressed on purpose. */
#![allow(dead_code)]
/* End Attribute *************************************************************/

/* Define ********************************************************************/
/* Rounds for the normal tests; AVG/MAX/MIN are computed over these rounds. */
const BENCH_LOOP_NUM: u64 = 10000;

/* Reduced rounds for the wall-clock/console-bound tests: IDLE_SLEEP (one 10ms
 * tick per round) and DEBUG_PRINT (one console character per round). Every
 * other test uses BENCH_LOOP_NUM. */
const BENCH_LOOP_REDUCED_NUM: u64 = 100;

/* The number of words reserved for each helper thread's stack. */
const BENCH_STACK_WORD_NUM: usize = 512;

/* Capability slots inside the init process' capability table. The table has
 * 64 entries and the boot code consumes slots 0..9, so the benchmark starts
 * at slot 10. */
const CAP_BENCH_THD: RmePtr = 10; /* The helper thread of the init process */
const CAP_BENCH_CPT: RmePtr = 13; /* The capability table of process 2 */
const CAP_BENCH_PGT_TOP_NEW: RmePtr = 14; /* The root page table of process 2 */
const CAP_BENCH_PGT_L1: RmePtr = 15; /* The second level page table of process 2 */
const CAP_BENCH_PGT_L2: RmePtr = 16; /* The third level page table of process 2 */
const CAP_BENCH_PRC: RmePtr = 17; /* Process 2 itself */
const CAP_BENCH_P2_THD: RmePtr = 18; /* The switch target of process 2 */
const CAP_BENCH_SIG_SELF: RmePtr = 19; /* The self-to-self signal endpoint */
const CAP_BENCH_SIG_PARK: RmePtr = 20; /* The parking endpoint of the init process */
const CAP_BENCH_SIG_DATA: RmePtr = 21; /* The data channel signal endpoint */
const CAP_BENCH_SIG_PARK2: RmePtr = 22; /* The parking endpoint of process 2 */
const CAP_BENCH_INV: RmePtr = 23; /* The invocation stub */
const CAP_BENCH_THD2: RmePtr = 24; /* The switch target of the init process */
const CAP_BENCH_INV2: RmePtr = 59; /* The second invocation stub */
const CAP_BENCH_SIG_SND_ONLY: RmePtr = 60; /* Send-only signal endpoint */
const CAP_BENCH_SIG_RCV_ONLY: RmePtr = 61; /* Receive-only signal endpoint */

/* Capability slots inside process 2's own capability table. Every process
 * owns an independent capability ID namespace, so the capabilities that
 * process 2 needs have to be delegated into its table one by one. */
const CAP2_THD: RmePtr = 0; /* The receiver thread of process 2 */
const CAP2_INIT_THD: RmePtr = 1; /* The init thread, delegated */
const CAP2_SIG_DATA: RmePtr = 2; /* The data channel endpoint, delegated */
const CAP2_SIG_PARK: RmePtr = 3; /* The parking endpoint, delegated */
const CAP2_THD2: RmePtr = 4; /* The switch target thread of process 2 */
const CAP2_INV2: RmePtr = 5; /* The second invocation stub, delegated */
const CAP2_KFN: RmePtr = 6; /* The KFN capability, used by DEBUG_INV_MOD */

/* The preemptive priorities the benchmark uses. The init thread runs at
 * priority 0, and a thread can only be switched to with THD_SWT when it
 * has the very same priority. */
const BENCH_PRIO_INIT: RmePtr = 0;
const BENCH_PRIO_RECV: RmePtr = 1; /* One level above the init thread */
const BENCH_PRIO_RECV2: RmePtr = 2; /* Two levels above the init thread */

/* The phases the helper threads dispatch on. The phase is published by the
 * measuring thread through a shared variable before it wakes the helper
 * thread up with a signal on its parking endpoint. */
const PHASE_IDLE: u32 = 0;
const PHASE_SIG_SELF: u32 = 1; /* Process 1's helper signals itself */
const PHASE_SIG_RECV: u32 = 2; /* Process 1's helper receives and measures */
const PHASE_SIG_RECV2: u32 = 3; /* Process 2's helper receives and measures */
const PHASE_SIG_RCV_ONLY: u32 = 4; /* Process 1's helper measures receive only */

/* The kernel memory frontier for the objects the benchmark creates. The
 * init image is physically at 0x81030000 and the 32-page mapping ends at
 * 0x81050000, i.e. KOM-relative 0x50000. Start above that to avoid
 * overwriting the image (and its .bss) with kernel objects.
 * Every kernel object the benchmark creates is carved out of this region by
 * the bump allocator: the second process, its page tables, the helper
 * threads and the invocation stubs. One frontier is enough, because the bump
 * pointer hands out a distinct range to every object. */
const BENCH_KOM_FRONTIER: RmePtr = 0x60000;

/* The alignment every kernel object needs inside the KOM. The KOM hands out
 * its objects in 64 byte slots (RME_KOM_SLOT_ORDER = 6), and _RME_Kot_Mark
 * rejects an address that is not slot aligned. */
const BENCH_KOM_ALIGN: RmePtr = 64;

/* A page table needs 4KB alignment on top of that: Sv39 finds the table
 * through its PPN, so the kernel rejects a misaligned one (__RME_Pgt_Check
 * tests Vaddr & 0xFFF). */
const BENCH_PGT_ALIGN: RmePtr = 0x1000;

/* The sizes of the kernel objects the benchmark creates. The kernel marks
 * exactly its own size in the KOT bitmap (_RME_Kot_Mark), so a request below
 * the real size of the object is reported as RME_ERR_KOT_BMP at the next
 * allocation. The margins below are headroom against a kernel-side growth:
 *   PGT : RME_PGT_SIZE_NOM(9) = 512 entries x 8 B = 4096
 *   THD : RME_THD_SIZE        = 728
 *   INV : RME_INV_SIZE        = 72
 * The size of a capability table is derived from its entry count instead,
 * see bench_kom_alloc_cpt.
 * The thread object holds the register context at offset 176 and the FPU
 * context at offset 464, so it must stay 8 byte aligned for the FSD of the
 * coprocessor save; the 64 byte slot alignment covers that. */
const BENCH_PGT_SIZE: RmePtr = 0x1000;
const BENCH_THD_SIZE: RmePtr = 0x2000;
const BENCH_INV_SIZE: RmePtr = 0x1000;

/* The number of timeslices the measuring thread donates to a helper thread.
 * It is deliberately large enough that a helper never runs out of time
 * during a test, and small enough that the measuring thread keeps a healthy
 * share of the CPU. */
const BENCH_TIME_XFER: RmePtr = 10000000;

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

use crate::syscall::{
    RME_BOOT_INIT_CPT, RME_BOOT_INIT_KFN, RME_BOOT_INIT_KOM, RME_BOOT_INIT_PGT,
    RME_BOOT_INIT_PGT_L2, RME_BOOT_INIT_PRC, RME_BOOT_INIT_THD, RME_BOOT_INIT_THD_SLOT,
    RME_BOOT_TBL_THD, RME_CID_NULL, RME_INV_FLAG_ALL, RME_KFN_CACHE_MAINT, RME_KFN_DEBUG_EXC_GET,
    RME_KFN_DEBUG_INV_MOD, RME_KFN_DEBUG_PRINT, RME_KFN_DEBUG_REG_MOD, RME_KFN_FLAG_FULL_RANGE,
    RME_KFN_IDLE_SLEEP, RME_KFN_PGT_ASID_SET, RME_KFN_PGT_CACHE_CLR, RME_KFN_PGT_ENTRY_MOD,
    RME_KFN_PGT_LINE_CLR, RME_PGT_ALL_DYN, RME_PGT_ALL_PERM, RME_PGT_SIZE_1G, RME_PGT_SIZE_2M,
    RME_PGT_SIZE_4K, RME_RCV_BM, RME_RCV_NS, RME_RV64V_CACHE_BLOCK_SIZE,
    RME_RV64V_KFN_CACHE_CLEAN_ADDR, RME_RV64V_KFN_CACHE_CLEAN_INV_ADDR, RME_RV64V_KFN_CACHE_DCACHE,
    RME_RV64V_KFN_CACHE_ICACHE, RME_RV64V_KFN_CACHE_INV_ADDR, RME_RV64V_KFN_CACHE_INV_ALL,
    RME_RV64V_KFN_DEBUG_EXC_ADDR_GET, RME_RV64V_KFN_DEBUG_EXC_CAUSE_GET,
    RME_RV64V_KFN_DEBUG_EXC_VALUE_GET, RME_RV64V_KFN_DEBUG_INV_MOD_SP_GET,
    RME_RV64V_KFN_DEBUG_REG_MOD_X10_A0, RME_RV64V_KFN_PGT_ENTRY_MOD_FLAG_GET,
    RME_RV64V_KFN_PGT_ENTRY_MOD_NMORD_GET, RME_RV64V_KFN_PGT_ENTRY_MOD_SZORD_GET, RME_SIG_FLAG_ALL,
    RME_THD_FLAG_ALL, RME_THD_FLAG_SWT, RmePtr, rme_cid_2l, rme_cpt_add, rme_cpt_crt, rme_cpt_frz,
    rme_inv_act, rme_inv_crt, rme_inv_ret, rme_inv_set, rme_kfn, rme_pgt_add, rme_pgt_con,
    rme_pgt_crt, rme_prc_crt, rme_sig_crt, rme_sig_del, rme_sig_rcv, rme_sig_snd, rme_thd_crt,
    rme_thd_exec_set, rme_thd_sched_bind, rme_thd_swt, rme_thd_time_xfer,
};
/* End Include ***************************************************************/

/* Private Variables *********************************************************/
/* The stacks of the helper threads and of the invocation. They live in the
 * init image's bss, which the second process maps physically one to one, so
 * the very same addresses are valid in both processes. */
static mut BENCH_STACK_P1T2: [u64; BENCH_STACK_WORD_NUM] = [0; BENCH_STACK_WORD_NUM];
static mut BENCH_STACK_P1T3: [u64; BENCH_STACK_WORD_NUM] = [0; BENCH_STACK_WORD_NUM];
static mut BENCH_STACK_P2T1: [u64; BENCH_STACK_WORD_NUM] = [0; BENCH_STACK_WORD_NUM];
static mut BENCH_STACK_P2T2: [u64; BENCH_STACK_WORD_NUM] = [0; BENCH_STACK_WORD_NUM];
static mut BENCH_STACK_INV: [u64; BENCH_STACK_WORD_NUM] = [0; BENCH_STACK_WORD_NUM];
static mut BENCH_STACK_INV2: [u64; BENCH_STACK_WORD_NUM] = [0; BENCH_STACK_WORD_NUM];
static mut BENCH_STACK_TEST: [u64; BENCH_STACK_WORD_NUM] = [0; BENCH_STACK_WORD_NUM];

/* The shared phase flag, published by the measuring thread and observed by
 * the helper threads. */
static mut BENCH_PHASE: u32 = PHASE_IDLE;

/* The timestamp the sender latches before it hands the workload over, and
 * the timestamp the receiver latches when the workload arrives. */
static mut BENCH_START: u64 = 0;
static mut BENCH_END: u64 = 0;

/* The statistics the receiving side accumulates. They live in the shared
 * image, so the measuring thread can pick them up once the test is over. */
static mut BENCH_STAT_TOTAL: u64 = 0;
static mut BENCH_STAT_MAX: u32 = 0;
static mut BENCH_STAT_MIN: u32 = 0;
static mut BENCH_STAT_CNT: u64 = 0;

/* The return value of the timed system call on the shared/helper path.
 * Direct tests keep it in BenchStat; helper threads write this shared slot and
 * BenchStat::load_shared copies it back. */
static mut BENCH_STAT_RET: i64 = 0;

/* Number of rounds whose timed call returned a negative value. Nonzero stops
 * the current test and prevents the failed sample from entering the stats. */
static mut BENCH_STAT_FAIL: u64 = 0;

/* Set by the nested INV_RET stub when its inner INV_ACT fails. The outer
 * invoker checks this before reporting the nested round as successful. */
static mut BENCH_INV_NEST_ERR: i64 = 0;

/* The return value of the DEBUG_INV_MOD call made inside the invocation stub.
 * The stub runs in process 2's capability namespace, the invoker reads this
 * shared slot after the invocation returns. */
static mut BENCH_KFN_INV_RET: i64 = 0;

/* The capability IDs the helper threads need. They are resolved once during
 * the preparation, because a capability ID cannot be derived from a slot
 * number that lives in another capability table. */
static mut CAP_P1T3: RmePtr = 0;
static mut CAP_P2_THD: RmePtr = 0;
static mut CAP_P1T1_IN_P2: RmePtr = 0;
static mut CAP_SIG_SELF: RmePtr = 0;
static mut CAP_SIG_PARK: RmePtr = 0;
static mut CAP_SIG_PARK2: RmePtr = 0;
static mut CAP_SIG_DATA: RmePtr = 0;

/* The monotonic KOM allocator. Every kernel object the benchmark creates is
 * carved out of this bump pointer, and each fixture consumes one distinct
 * range for the lifetime of the run. */
static mut BENCH_KOM_NEXT: RmePtr = BENCH_KOM_FRONTIER;
/* End Private Variables *****************************************************/

/* Function:bench_read_cycle ***************************************************
Description : Read the RISC-V cycle counter. "TSC" is x86 terminology, so the
              function is named after the CSR it actually reads. The cycle CSR
              is a free-running counter at CPU clock frequency.
Input       : None.
Output      : None.
Return      : u64 - The current cycle count.
*****************************************************************************/
#[inline(always)]
fn bench_read_cycle() -> u64 {
    let cycle: u64;

    unsafe {
        core::arch::asm!(
            "csrr {0}, cycle",
            out(reg) cycle,
            options(nostack),
        );
    }

    cycle
}
/* End Function:bench_read_cycle ***********************************************/

/* Function:bench_read_time ****************************************************
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
fn bench_read_time() -> u64 {
    let time: u64;

    unsafe {
        core::arch::asm!(
            "csrr {0}, time",
            out(reg) time,
            options(nostack),
        );
    }

    time
}
/* End Function:bench_read_time ***********************************************/

/* Function:bench_kom_alloc **************************************************
Description : Allocate a distinct KOM range for a kernel object. The address is
              relative to the KOM capability's Begin, as the kernel expects.
              KOM is never reclaimed here; the bump pointer is enough.
              The size must cover what the kernel writes into the object: the
              kernel marks exactly its own size in the KOT bitmap, so a size
              below that is not reported here but at the next allocation, as
              RME_ERR_CPT_KOT. The alignment must be a power of two.
Input       : size - The number of bytes needed.
              align - The required alignment.
Output      : None.
Return      : RmePtr - The aligned relative KOM address.
*****************************************************************************/
#[inline(always)]
fn bench_kom_alloc(size: RmePtr, align: RmePtr) -> RmePtr {
    unsafe {
        /* The address is rounded up to the requested alignment, and the bump
         * pointer moves on by the size rounded up to the 64 byte slot of the
         * KOM, so that the next object never overlaps this one. */
        let addr = (BENCH_KOM_NEXT + align - 1) & !(align - 1);
        BENCH_KOM_NEXT = addr + ((size + BENCH_KOM_ALIGN - 1) & !(BENCH_KOM_ALIGN - 1));
        addr
    }
}
/* End Function:bench_kom_alloc **********************************************/

/* Function:bench_kom_alloc_cpt *********************************************
Description : Allocate the kernel object of a capability table with the given
              number of entries. The size is derived from the count, so the
              object is never smaller than what the kernel writes into it.
              NOTE: the same count has to be passed to CPT_CRT - the kernel
              writes exactly Entry_Num slots into the object, so the two
              numbers must agree.
Input       : entry_num - The number of entries of the table.
Output      : None.
Return      : RmePtr - The relative KOM address of the new table.
*****************************************************************************/
#[inline(always)]
fn bench_kom_alloc_cpt(entry_num: RmePtr) -> RmePtr {
    /* One entry is 64 bytes: Cap_Head (5 words) + Info[3]. */
    bench_kom_alloc(entry_num * 64, BENCH_KOM_ALIGN)
}
/* End Function:bench_kom_alloc_cpt *****************************************/

/* Function:bench_kom_alloc_pgt *********************************************
Description : Allocate the kernel object of a page table.
Input       : None.
Output      : None.
Return      : RmePtr - The relative KOM address of the new table.
*****************************************************************************/
#[inline(always)]
fn bench_kom_alloc_pgt() -> RmePtr {
    bench_kom_alloc(BENCH_PGT_SIZE, BENCH_PGT_ALIGN)
}
/* End Function:bench_kom_alloc_pgt *****************************************/

/* Function:bench_kom_alloc_thd *********************************************
Description : Allocate the kernel object of a thread.
Input       : None.
Output      : None.
Return      : RmePtr - The relative KOM address of the new thread.
*****************************************************************************/
#[inline(always)]
fn bench_kom_alloc_thd() -> RmePtr {
    bench_kom_alloc(BENCH_THD_SIZE, BENCH_KOM_ALIGN)
}
/* End Function:bench_kom_alloc_thd *****************************************/

/* Function:bench_kom_alloc_inv *********************************************
Description : Allocate the kernel object of an invocation stub.
Input       : None.
Output      : None.
Return      : RmePtr - The relative KOM address of the new stub.
*****************************************************************************/
#[inline(always)]
fn bench_kom_alloc_inv() -> RmePtr {
    bench_kom_alloc(BENCH_INV_SIZE, BENCH_KOM_ALIGN)
}
/* End Function:bench_kom_alloc_inv *****************************************/

/* Function:bench_print_char ***************************************************
Description : Print one character by invoking the DEBUG_PRINT kernel function.
              Param0 packs D0 = Func_ID (DEBUG_PRINT) and D1 = Sub_ID (the
              character), the same convention as the C init's Print_Char.
Input       : ch - The character to print.
Output      : None.
Return      : None.
*****************************************************************************/
#[inline(always)]
pub fn bench_print_char(ch: u8) {
    let _ = rme_kfn(RME_BOOT_INIT_KFN, RME_KFN_DEBUG_PRINT, ch as RmePtr, 0, 0);
}
/* End Function:bench_print_char ***********************************************/

/* Function:bench_print_str ****************************************************
Description : Print a string, one syscall per character.
Input       : s - The string to print.
Output      : None.
Return      : None.
*****************************************************************************/
#[inline(always)]
fn bench_print_str(s: &str) {
    for &ch in s.as_bytes() {
        bench_print_char(ch);
    }
}
/* End Function:bench_print_str ************************************************/

/* Type:BenchWriter **********************************************************
Description : A core::fmt::Write implementation that forwards formatted output
              to the KFN debug-print path. This makes write!/format_args!
              usable without an allocator; format! itself would need String.
*****************************************************************************/
struct BenchWriter;

impl Write for BenchWriter {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        bench_print_str(s);
        Ok(())
    }
}
/* End Type:BenchWriter ******************************************************/

/* Macro:bench_print *********************************************************
Description : Formatted printing without allocation. Uses format_args! and the
              BenchWriter, not format! (which requires an allocator).
*****************************************************************************/
macro_rules! bench_print {
    ($($arg:tt)*) => {{
        let _ = BenchWriter.write_fmt(format_args!($($arg)*));
    }};
}
/* End Macro:bench_print *****************************************************/

/* Function:bench_setup_check ************************************************
Description : Check the result of one setup system call of the global
              preparation stage. A failed setup is fatal: the error is printed
              and the benchmark stops right there, because the measurement
              environment is broken and every test that follows would only
              produce misleading numbers. The program spins forever instead of
              returning, because a user-mode thread cannot be aborted from
              here.
Input       : label - The setup step name.
              ret - The value the setup system call returned.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_setup_check(label: &str, ret: RmePtr) {
    let signed = ret as i64;

    if signed < 0 {
        bench_print!("bench setup failed: {} ret={}\r\n", label, signed);
        bench_print!("RME BENCH REPORT ABORTED\r\n");

        loop {
            core::hint::spin_loop();
        }
    }
}
/* End Function:bench_setup_check *******************************************/

/* Function:bench_setup_try **************************************************
Description : Check the result of one setup system call of a single test. A
              failed setup only fails that test: the error is recorded in the
              statistics, so the caller can leave the test and the suite
              continues with the next one.
Input       : stat - The statistics accumulator of the current test.
              label - The setup step name.
              ret - The value the setup system call returned.
Output      : None.
Return      : bool - true when the call succeeded.
*****************************************************************************/
fn bench_setup_try(stat: &mut BenchStat, label: &str, ret: RmePtr) -> bool {
    let signed = ret as i64;

    if signed < 0 {
        bench_print!("bench setup failed: {} ret={}\r\n", label, signed);
        let _ = stat.set_ret(signed);
        false
    } else {
        true
    }
}
/* End Function:bench_setup_try *********************************************/

/* Function:bench_phase_get **************************************************
Description : Read the shared phase flag.
Input       : None.
Output      : None.
Return      : u32 - The current phase.
*****************************************************************************/
#[inline(always)]
fn bench_phase_get() -> u32 {
    unsafe { core::ptr::read_volatile(&raw const BENCH_PHASE) }
}
/* End Function:bench_phase_get **********************************************/

/* Function:bench_phase_set **************************************************
Description : Publish the shared phase flag.
Input       : phase - The phase to publish.
Output      : None.
Return      : None.
*****************************************************************************/
#[inline(always)]
fn bench_phase_set(phase: u32) {
    unsafe { core::ptr::write_volatile(&raw mut BENCH_PHASE, phase) }
}
/* End Function:bench_phase_set **********************************************/

/* Function:bench_start_set **************************************************
Description : Latch the timestamp of the workload's departure.
Input       : value - The timestamp.
Output      : None.
Return      : None.
*****************************************************************************/
#[inline(always)]
fn bench_start_set(value: u64) {
    unsafe { core::ptr::write_volatile(&raw mut BENCH_START, value) }
}
/* End Function:bench_start_set **********************************************/

/* Function:bench_start_get **************************************************
Description : Read the timestamp of the workload's departure.
Input       : None.
Output      : None.
Return      : u64 - The timestamp.
*****************************************************************************/
#[inline(always)]
fn bench_start_get() -> u64 {
    unsafe { core::ptr::read_volatile(&raw const BENCH_START) }
}
/* End Function:bench_start_get **********************************************/

/* Function:bench_end_set ****************************************************
Description : Latch the timestamp of the workload's arrival.
Input       : value - The timestamp.
Output      : None.
Return      : None.
*****************************************************************************/
#[inline(always)]
fn bench_end_set(value: u64) {
    unsafe { core::ptr::write_volatile(&raw mut BENCH_END, value) }
}
/* End Function:bench_end_set ***********************************************/

/* Function:bench_end_get ****************************************************
Description : Read the timestamp of the workload's arrival.
Input       : None.
Output      : None.
Return      : u64 - The timestamp.
*****************************************************************************/
#[inline(always)]
fn bench_end_get() -> u64 {
    unsafe { core::ptr::read_volatile(&raw const BENCH_END) }
}
/* End Function:bench_end_get ***********************************************/

/* Function:bench_stat_reset ************************************************
Description : Reset the shared statistics accumulator.
Input       : None.
Output      : None.
Return      : None.
*****************************************************************************/
#[inline(always)]
fn bench_stat_reset() {
    unsafe {
        core::ptr::write_volatile(&raw mut BENCH_STAT_TOTAL, 0);
        core::ptr::write_volatile(&raw mut BENCH_STAT_MAX, 0);
        core::ptr::write_volatile(&raw mut BENCH_STAT_MIN, u32::MAX);
        core::ptr::write_volatile(&raw mut BENCH_STAT_CNT, 0);
        core::ptr::write_volatile(&raw mut BENCH_STAT_RET, 0);
        core::ptr::write_volatile(&raw mut BENCH_STAT_FAIL, 0);
    }
}
/* End Function:bench_stat_reset ********************************************/

/* Function:bench_stat_record ***********************************************
Description : Accumulate the result of a single round into the shared
              statistics. The counter difference is calculated with 32 bit
              arithmetic, so that a single overflow of the 64 bit cycle
              counter between the two reads is handled transparently.
Input       : diff - The number of cycles the round took.
Output      : None.
Return      : None.
*****************************************************************************/
#[inline(always)]
fn bench_stat_record(diff: u32) {
    unsafe {
        if core::ptr::read_volatile(&raw const BENCH_STAT_FAIL) != 0 {
            return;
        }

        let total = core::ptr::read_volatile(&raw const BENCH_STAT_TOTAL);
        let max = core::ptr::read_volatile(&raw const BENCH_STAT_MAX);
        let min = core::ptr::read_volatile(&raw const BENCH_STAT_MIN);
        let cnt = core::ptr::read_volatile(&raw const BENCH_STAT_CNT);

        core::ptr::write_volatile(&raw mut BENCH_STAT_TOTAL, total + diff as u64);
        if diff > max {
            core::ptr::write_volatile(&raw mut BENCH_STAT_MAX, diff);
        }
        if diff < min {
            core::ptr::write_volatile(&raw mut BENCH_STAT_MIN, diff);
        }
        core::ptr::write_volatile(&raw mut BENCH_STAT_CNT, cnt + 1);
    }
}
/* End Function:bench_stat_record *******************************************/

/* Function:bench_stat_set_ret **********************************************
Description : Record the return value of a timed system call on the shared /
              helper path. Direct tests use BenchStat::set_ret instead.
Input       : ret - The return value (signed).
Output      : None.
Return      : i64 - The same value, for convenient use in `let _ =`.
*****************************************************************************/
#[inline(always)]
fn bench_stat_set_ret(ret: i64) -> i64 {
    unsafe {
        core::ptr::write_volatile(&raw mut BENCH_STAT_RET, ret);
        if ret < 0 {
            let fail = core::ptr::read_volatile(&raw const BENCH_STAT_FAIL);
            core::ptr::write_volatile(&raw mut BENCH_STAT_FAIL, fail + 1);
        }
    }
    ret
}
/* End Function:bench_stat_set_ret ******************************************/

/* Function:bench_stat_failed ************************************************
Description : Read the shared helper failure flag.
*****************************************************************************/
#[inline(always)]
fn bench_stat_failed() -> bool {
    unsafe { core::ptr::read_volatile(&raw const BENCH_STAT_FAIL) != 0 }
}
/* End Function:bench_stat_failed ********************************************/

/* Type ***********************************************************************/
/// The accumulated statistics of a single benchmark test.
struct BenchStat {
    total: u64,
    max: u32,
    min: u32,
    count: u64,
    ret: i64,
    fail: u64,
}
/* End Type ******************************************************************/

/* Function:BenchStat::new ***************************************************
Description : Create an empty statistics accumulator.
Input       : None.
Output      : None.
Return      : BenchStat - The accumulator.
*****************************************************************************/
impl BenchStat {
    fn new() -> Self {
        Self {
            total: 0,
            max: 0,
            min: u32::MAX,
            count: 0,
            ret: 0,
            fail: 0,
        }
    }

    /// Reset the accumulator, so that it can be reused for another test.
    fn reset(&mut self) {
        self.total = 0;
        self.max = 0;
        self.min = u32::MAX;
        self.count = 0;
        self.ret = 0;
        self.fail = 0;
    }

    /// Record the return value of the timed call for this test. A negative
    /// value is counted as a failed round; the last value is retained only for
    /// the printed `ret=` field.
    #[inline(always)]
    fn set_ret(&mut self, ret: i64) -> i64 {
        self.ret = ret;
        if ret < 0 {
            self.fail += 1;
        }
        ret
    }

    /// Accumulate the result of a single round into the current statistics.
    /// A failed round is not recorded; the loop condition stops the test.
    #[inline(always)]
    fn record(&mut self, diff: u32) {
        if self.fail != 0 {
            return;
        }
        self.total += diff as u64;
        if diff > self.max {
            self.max = diff;
        }
        if diff < self.min {
            self.min = diff;
        }
        self.count += 1;
    }

    /// Pick up the statistics that the receiving side has accumulated.
    fn load_shared(&mut self) {
        self.total = unsafe { core::ptr::read_volatile(&raw const BENCH_STAT_TOTAL) };
        self.max = unsafe { core::ptr::read_volatile(&raw const BENCH_STAT_MAX) };
        self.min = unsafe { core::ptr::read_volatile(&raw const BENCH_STAT_MIN) };
        self.count = unsafe { core::ptr::read_volatile(&raw const BENCH_STAT_CNT) };
        self.ret = unsafe { core::ptr::read_volatile(&raw const BENCH_STAT_RET) };
        self.fail = unsafe { core::ptr::read_volatile(&raw const BENCH_STAT_FAIL) };
    }

    /// A helper-based test is only valid when the expected number of rounds
    /// completed. A blocked or never-woken helper otherwise leaves a partial
    /// (but still non-negative) result behind.
    fn require_count(&mut self, expected: u64) {
        if self.count != expected {
            let _ = self.set_ret(-1);
        }
    }
}
/* End Function:BenchStat::new ***********************************************/

/* Function:bench_report ****************************************************
Description : Print the name of a test followed by its average, maximum and
              minimum results, all of them expressed in CPU cycles. The layout
              matches the RVM_LIST macro of the C rvm_benchmark.h.
Input       : title - The name of the test, padded with spaces.
              stat - The accumulated statistics.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_report(title: &str, stat: &BenchStat) {
    if stat.fail != 0 {
        bench_print!("{:<44} : FAIL ret={}\r\n", title, stat.ret);
        return;
    }

    let rounds = if stat.count > 0 { stat.count } else { 1 };
    let min = if stat.count > 0 { stat.min } else { 0 };

    bench_print!(
        "{:<44} : {} / {} / {} / ret={}\r\n",
        title,
        stat.total / rounds,
        stat.max,
        min,
        stat.ret
    );
}
/* End Function:bench_report *************************************************/

/* Function:bench_stack_top *************************************************
Description : Compute the stack pointer of a helper thread or of an
              invocation stub. The RISC-V stack grows downwards, so the
              initial stack pointer is one past the end of the stack. The
              RISC-V ABI requires the stack pointer to be 16 byte aligned,
              and the kernel uses the value verbatim, so the alignment is
              fixed up here. The fixup rounds the pointer down, which keeps
              the stack inside its own array - rounding up would place the
              stack pointer past the end, into whatever follows in the bss.
Input       : stack - A pointer to the stack array.
Output      : None.
Return      : u64 - The stack pointer.
*****************************************************************************/
#[inline(always)]
fn bench_stack_top<const N: usize>(stack: *const [u64; N]) -> u64 {
    (stack.cast::<u64>() as u64 + (N as u64) * 8) & !0xF
}
/* End Function:bench_stack_top *********************************************/

/* Function:bench_pgt_crt ***************************************************
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
*****************************************************************************/
#[inline(always)]
fn bench_pgt_crt(cap_pgt: u64, raddr: u64, base: u64, is_top: u64, size_order: u64) -> u64 {
    rme_pgt_crt(
        RME_BOOT_INIT_CPT,
        RME_BOOT_INIT_KOM,
        cap_pgt,
        raddr,
        base,
        is_top,
        size_order,
    )
}
/* End Function:bench_pgt_crt *************************************************/

/* Function:bench_pgt_con ***************************************************
Description : Map a child page table into the parent page table, building the
              address space tree.
Input       : cap_parent - The parent page table capability slot.
              pos - The parent table position to map the child to.
              cap_child - The child page table capability slot.
Output      : None.
Return      : u64 - The kernel return value (in a0).
*****************************************************************************/
#[inline(always)]
fn bench_pgt_con(cap_parent: u64, pos: u64, cap_child: u64) -> u64 {
    rme_pgt_con(cap_parent, pos, cap_child, RME_PGT_ALL_PERM)
}
/* End Function:bench_pgt_con *************************************************/

/* Function:bench_pgt_add ***************************************************
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
*****************************************************************************/
#[inline(always)]
fn bench_pgt_add(cap_dst: u64, pos_dst: u64, cap_src: u64, pos_src: u64) -> u64 {
    rme_pgt_add(cap_dst, pos_dst, cap_src, pos_src, RME_PGT_ALL_DYN)
}
/* End Function:bench_pgt_add *************************************************/

/* Function:bench_thd_crt ***************************************************
Description : Create a thread.
Input       : cap_cpt - The capability table that holds the capability table
                        the new thread's capability is created in.
              cap_thd - The capability slot of the new thread.
              cap_prc - The process the thread belongs to.
              raddr - The relative KOM address of the thread object.
Output      : None.
Return      : u64 - The kernel return value (in a0).
*****************************************************************************/
#[inline(always)]
fn bench_thd_crt(cap_cpt: u64, cap_thd: u64, cap_prc: u64, raddr: u64) -> u64 {
    rme_thd_crt(cap_cpt, RME_BOOT_INIT_KOM, cap_thd, cap_prc, 31, raddr)
}
/* End Function:bench_thd_crt *************************************************/

/* Function:bench_thd_bind **************************************************
Description : Bind a thread to the current processor, nominate the init thread
              as its scheduler and give it a preemptive priority.
Input       : cap_thd - The capability to the thread (two-level).
              prio - The preemptive priority of the thread.
Output      : None.
Return      : u64 - The kernel return value (in a0).
*****************************************************************************/
#[inline(always)]
fn bench_thd_bind(cap_thd: u64, prio: u64) -> u64 {
    rme_thd_sched_bind(cap_thd, RME_BOOT_INIT_THD, RME_CID_NULL, 0, prio, 0)
}
/* End Function:bench_thd_bind ************************************************/

/* Function:bench_sig_crt ***************************************************
Description : Create a signal endpoint in the init capability table.
Input       : cap_sig - The capability slot of the new endpoint.
Output      : None.
Return      : u64 - The kernel return value (in a0).
*****************************************************************************/
#[inline(always)]
fn bench_sig_crt(cap_sig: u64) -> u64 {
    rme_sig_crt(RME_BOOT_INIT_CPT, cap_sig)
}
/* End Function:bench_sig_crt *************************************************/

/* Function:bench_inv_entry *************************************************
Description : The entry point of the invocation. It runs in the address space
              of the second process, with the second process' page table
              active, and returns to the invoker immediately.
Input       : param - The parameter the invoker passed (unused).
Output      : None.
Return      : ! - This function never returns.
*****************************************************************************/
extern "C" fn bench_inv_entry(_param: u64) -> ! {
    let _ = rme_inv_ret(0x1234);

    /* The return above resumes the invoker - this thread never runs again. */
    loop {}
}
/* End Function:bench_inv_entry **********************************************/

/* Function:bench_inv_act_entry **********************************************
Description : One-way INV_ACT entry. It latches the arrival time and returns
              immediately; the invoker reads that timestamp after INV_ACT
              returns, so the measurement covers entry only, not the return.
Input       : _param - Unused.
Output      : None.
Return      : ! - This function never returns here.
*****************************************************************************/
extern "C" fn bench_inv_act_entry(_param: u64) -> ! {
    bench_end_set(bench_read_cycle());
    let _ = rme_inv_ret(0x1111);
    loop {}
}
/* End Function:bench_inv_act_entry ******************************************/

/* Function:bench_inv_nest_entry *********************************************
Description : Outer stub of the nested INV_RET test. It invokes the inner stub
              and latches END when that invocation returns, then returns to the
              original invoker.
Input       : _param - Unused.
Output      : None.
Return      : ! - This function never returns here.
*****************************************************************************/
extern "C" fn bench_inv_nest_entry(_param: u64) -> ! {
    let ret = rme_inv_act(CAP2_INV2, 0x2222);

    /* Publish the inner activation failure. The outer invoker checks this
     * before it reports the nested INV_RET round as successful. */
    if !ret.ok() {
        unsafe {
            core::ptr::write_volatile(&raw mut BENCH_INV_NEST_ERR, ret.status());
        }
    }

    bench_end_set(bench_read_cycle());
    let _ = rme_inv_ret(0x3333);
    loop {}
}
/* End Function:bench_inv_nest_entry *****************************************/

/* Function:bench_inv_ret_entry **********************************************
Description : Inner stub of the nested INV_RET test. It latches START and
              immediately executes INV_RET.
Input       : _param - Unused.
Output      : None.
Return      : ! - This function never returns here.
*****************************************************************************/
extern "C" fn bench_inv_ret_entry(_param: u64) -> ! {
    bench_start_set(bench_read_cycle());
    let _ = rme_inv_ret(0x4444);
    loop {}
}
/* End Function:bench_inv_ret_entry ******************************************/

/* Function:bench_inv_dbg_entry **********************************************
Description : DEBUG_INV_MOD invocation stub. It runs with process 2's
              capabilities and a non-empty invocation stack; it measures the
              layer-0 SP query and publishes the return value.
Input       : _param - Unused.
Output      : None.
Return      : ! - This function never returns here.
*****************************************************************************/
extern "C" fn bench_inv_dbg_entry(_param: u64) -> ! {
    bench_start_set(bench_read_cycle());
    let ret = rme_kfn(
        CAP2_KFN,
        RME_KFN_DEBUG_INV_MOD,
        CAP2_INIT_THD,
        RME_RV64V_KFN_DEBUG_INV_MOD_SP_GET,
        0,
    );
    bench_end_set(bench_read_cycle());
    unsafe {
        core::ptr::write_volatile(&raw mut BENCH_KFN_INV_RET, ret as i64);
    }
    let _ = rme_inv_ret(0x5555);
    loop {}
}
/* End Function:bench_inv_dbg_entry *******************************************/

/* Function:bench_p1t2_entry ***********************************************
Description : The entry point of the helper thread of the init process. It
              plays the receiving role in the intra-process signal test.
Input       : _param - The role passed by the creator (unused).
Output      : None.
Return      : ! - This function never returns.
*****************************************************************************/
extern "C" fn bench_p1t2_entry(_param: u64) -> ! {
    loop {
        match bench_phase_get() {
            PHASE_SIG_SELF => {
                bench_signal_self();
                bench_phase_set(PHASE_IDLE);
            }
            PHASE_SIG_RECV => {
                bench_signal_receive();
                bench_phase_set(PHASE_IDLE);
            }
            PHASE_SIG_RCV_ONLY => {
                bench_signal_rcv_only();
                bench_phase_set(PHASE_IDLE);
            }
            _ => {
                /* Nothing to do - park until the measuring thread wakes us */
                let cap_park = unsafe { core::ptr::read_volatile(&raw const CAP_SIG_PARK) };
                let _ = rme_sig_rcv(cap_park, RME_RCV_BM);
            }
        }
    }
}
/* End Function:bench_p1t2_entry ********************************************/

/* Function:bench_p2t1_entry ***********************************************
Description : The entry point of the receiver thread of the second process.
              It plays the receiving role in the inter-process signal test.
Input       : _param - The role passed by the creator (unused).
Output      : None.
Return      : ! - This function never returns.
*****************************************************************************/
extern "C" fn bench_p2t1_entry(_param: u64) -> ! {
    loop {
        match bench_phase_get() {
            PHASE_SIG_RECV2 => {
                bench_signal_receive();
                bench_phase_set(PHASE_IDLE);
            }
            _ => {
                /* Nothing to do - park until the measuring thread wakes us.
                 * The parking endpoint is a one level capability ID, because
                 * this thread runs with the second process' capability
                 * table. */
                let _ = rme_sig_rcv(CAP2_SIG_PARK, RME_RCV_BM);
            }
        }
    }
}
/* End Function:bench_p2t1_entry ********************************************/

/* Function:bench_p2t2_entry ***********************************************
Description : The entry point of the switch target of the second process. It
              latches the arrival timestamp of the inter-process switch and
              switches straight back.
Input       : _param - The role passed by the creator (unused).
Output      : None.
Return      : ! - This function never returns.
*****************************************************************************/
extern "C" fn bench_p2t2_entry(_param: u64) -> ! {
    loop {
        /* Latch the arrival of the switch, then hand the CPU back */
        bench_end_set(bench_read_cycle());
        let cap_p1t1 = unsafe { core::ptr::read_volatile(&raw const CAP_P1T1_IN_P2) };
        let _ = rme_thd_swt(cap_p1t1, 0);
    }
}
/* End Function:bench_p2t2_entry ********************************************/

/* Function:bench_p1t3_entry ***********************************************
Description : The entry point of the switch target of the init process. It latches
              the arrival timestamp of the intra-process switch and switches
              straight back.
Input       : _param - The role passed by the creator (unused).
Output      : None.
Return      : ! - This function never returns.
*****************************************************************************/
extern "C" fn bench_p1t3_entry(_param: u64) -> ! {
    loop {
        /* Latch the arrival of the switch, then hand the CPU back */
        bench_end_set(bench_read_cycle());
        let _ = rme_thd_swt(RME_BOOT_INIT_THD, 0);
    }
}
/* End Function:bench_p1t3_entry ********************************************/

/* Function:bench_signal_self **********************************************
Description : The self signalling test. The helper thread sends a signal to
              its own endpoint and receives it right away, so the receive
              never blocks and the round is a pure system call round trip.
Input       : None.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_signal_self() {
    let cap_self = unsafe { core::ptr::read_volatile(&raw const CAP_SIG_SELF) };
    let mut count: u64 = 0;

    bench_stat_reset();

    while count < BENCH_LOOP_NUM && !bench_stat_failed() {
        let start = bench_read_cycle();
        let send = rme_sig_snd(cap_self, 1);
        if (send as i64) < 0 {
            let _ = bench_stat_set_ret(send as i64);
            break;
        }
        let _ = bench_stat_set_ret(rme_sig_rcv(cap_self, RME_RCV_BM) as i64);
        let end = bench_read_cycle();

        bench_stat_record((end - start) as u32);
        count += 1;
    }
}
/* End Function:bench_signal_self *******************************************/

/* Function:bench_signal_receive *******************************************
Description : The receiving side of the signal tests. The helper thread
              blocks on the data channel, and the sender wakes it up once
              per round. The delivery latency is the distance between the
              timestamp the sender latched and the one latched here.
Input       : None.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_signal_receive() {
    /* The receiver of the second process uses its own capability table,
     * where the data channel lives in a different slot. */
    let cap_data = if bench_phase_get() == PHASE_SIG_RECV2 {
        CAP2_SIG_DATA
    } else {
        unsafe { core::ptr::read_volatile(&raw const CAP_SIG_DATA) }
    };
    let mut count: u64 = 0;

    bench_stat_reset();

    while count < BENCH_LOOP_NUM && !bench_stat_failed() {
        /* Wait for the workload - this blocks until the sender wakes us */
        let _ = bench_stat_set_ret(rme_sig_rcv(cap_data, RME_RCV_BM) as i64);
        let end = bench_read_cycle();

        bench_stat_record((end - bench_start_get()) as u32);
        count += 1;
    }
}
/* End Function:bench_signal_receive ****************************************/

/* Function:bench_signal_rcv_only *******************************************
Description : Receive-only measurement. The helper sends one signal untimed,
              then times a non-blocking receive, so the window contains only
              SIG_RCV.
Input       : None.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_signal_rcv_only() {
    let cap = CAP_BENCH_SIG_RCV_ONLY;
    let mut count: u64 = 0;

    bench_stat_reset();

    while count < BENCH_LOOP_NUM && !bench_stat_failed() {
        let send = rme_sig_snd(cap, 1);
        if (send as i64) < 0 {
            let _ = bench_stat_set_ret(send as i64);
            break;
        }
        let start = bench_read_cycle();
        let _ = bench_stat_set_ret(rme_sig_rcv(cap, RME_RCV_NS) as i64);
        let end = bench_read_cycle();

        bench_stat_record((end - start) as u32);
        count += 1;
    }
}
/* End Function:bench_signal_rcv_only ***************************************/

/* Function:bench_prepare_process2 *****************************************
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
fn bench_prepare_process2() {
    /* ---- Create the new process's capability table (16 entries) ---- */
    /* CPT_CRT: Cid=Cap_Cpt_Crt, Param0=D1(Cap_Kom)|D0(Cap_Crt),
     * Param1=Raddr, Param2=Entry_Num
     * The table holds the delegated caps in slots 0..6 (CAP2_*). It is not the
     * init capability table, which the boot code creates with 64 entries. */
    let raddr = bench_kom_alloc_cpt(16);
    bench_setup_check(
        "CPT_CRT process2 captbl",
        rme_cpt_crt(
            RME_BOOT_INIT_CPT,
            RME_BOOT_INIT_KOM,
            CAP_BENCH_CPT,
            raddr,
            16,
        ),
    );

    /* ---- Create the new process's page tables (root + L1 + L2) ---- */
    /* Root: 1GB entries, covers the low half. Raddr must be 4KB aligned
     * because Sv39 finds a table through its PPN. */
    let raddr = bench_kom_alloc_pgt();
    bench_setup_check(
        "PGT_CRT process2 root",
        bench_pgt_crt(CAP_BENCH_PGT_TOP_NEW, raddr, 0, 1, RME_PGT_SIZE_1G),
    );
    /* L1: 2MB entries, covers the user window [0,1GB) */
    let raddr = bench_kom_alloc_pgt();
    bench_setup_check(
        "PGT_CRT process2 L1",
        bench_pgt_crt(CAP_BENCH_PGT_L1, raddr, 0, 0, RME_PGT_SIZE_2M),
    );
    /* L2: 4KB entries, covers [0x20000000,0x20200000) */
    let raddr = bench_kom_alloc_pgt();
    bench_setup_check(
        "PGT_CRT process2 L2",
        bench_pgt_crt(CAP_BENCH_PGT_L2, raddr, 0x20000000, 0, RME_PGT_SIZE_4K),
    );

    /* ---- Connect the tables (build the address space tree) ---- */
    /* root[0] -> L1 (0x20000000>>30 = 0) */
    bench_setup_check(
        "PGT_CON process2 root->L1",
        bench_pgt_con(CAP_BENCH_PGT_TOP_NEW, RME_RV64V_TOP_POS, CAP_BENCH_PGT_L1),
    );
    /* L1[256] -> L2 (0x20000000>>21 = 256) */
    bench_setup_check(
        "PGT_CON process2 L1->L2",
        bench_pgt_con(CAP_BENCH_PGT_L1, RME_RV64V_L1_POS, CAP_BENCH_PGT_L2),
    );

    /* ---- Delegate the init image's code/bss pages into the new L2 ---- */
    /* Copy pages 0..15 (VA 0x20000000..0x20010000) from the init L2 table */
    let mut count: u64 = 0;
    while count < RME_RV64V_IMAGE_PAGES {
        bench_setup_check(
            "PGT_ADD process2 image page",
            bench_pgt_add(CAP_BENCH_PGT_L2, count, RME_BOOT_INIT_PGT_L2, count),
        );
        count += 1;
    }

    /* ---- Create the new process ---- */
    /* PRC_CRT: Cid=Cap_Cpt_Crt, Param0=Cap_Prc, Param1=Cap_Cpt, Param2=Cap_Pgt.
     * The new process gets its OWN capability table and its OWN page table. */
    bench_setup_check(
        "PRC_CRT process2",
        rme_prc_crt(
            RME_BOOT_INIT_CPT,
            CAP_BENCH_PRC,
            CAP_BENCH_CPT,
            CAP_BENCH_PGT_TOP_NEW,
        ),
    );

    /* ---- Create the receiver thread in the new process' OWN captbl ---- */
    /* THD_CRT: Cid=Cap_Cpt, Param0=D1(Cap_Kom)|D0(Cap_Thd),
     * Param1=D1(Cap_Prc)|D0(Prio_Max), Param2=Raddr.
     * Cid now points at the NEW captbl, so the new thread's cap slot is
     * created in the new process' independent capability namespace. */
    let raddr = bench_kom_alloc_thd();
    bench_setup_check(
        "THD_CRT process2 receiver",
        bench_thd_crt(CAP_BENCH_CPT, CAP2_THD, CAP_BENCH_PRC, raddr),
    );

    /* ---- Create the switch target thread in the new process' OWN captbl ---- */
    let raddr = bench_kom_alloc_thd();
    bench_setup_check(
        "THD_CRT process2 switch target",
        bench_thd_crt(CAP_BENCH_CPT, CAP2_THD2, CAP_BENCH_PRC, raddr),
    );

    /* ---- Create the helper thread of the init process ---- */
    let raddr = bench_kom_alloc_thd();
    bench_setup_check(
        "THD_CRT init process receiver",
        bench_thd_crt(RME_BOOT_INIT_CPT, CAP_BENCH_THD, RME_BOOT_INIT_PRC, raddr),
    );

    /* ---- Create the switch target of the init process ---- */
    /* It shares the init process' capability table, so no delegation is
     * needed for it. */
    let raddr = bench_kom_alloc_thd();
    bench_setup_check(
        "THD_CRT init process switch target",
        bench_thd_crt(RME_BOOT_INIT_CPT, CAP_BENCH_THD2, RME_BOOT_INIT_PRC, raddr),
    );

    /* ---- Delegate caps between the two independent captbls ---- */
    /* CPT_ADD: Param0=D1(Cap_Cpt_Dst)|D0(Cap_Dst),
     * Param1=D1(Cap_Cpt_Src)|D0(Cap_Src), Param2=Flag.
     *
     * 1) Delegate the switch target cap (new captbl slot CAP2_THD2) into the
     *    init captbl slot CAP_BENCH_P2_THD, so the init thread can bind it,
     *    set its execution info, transfer time and switch to it.
     * 2) Delegate the init thread cap (init captbl slot RME_BOOT_INIT_THD)
     *    into the new captbl slot CAP2_INIT_THD, so the cross-process
     *    thread can switch back to the init thread from inside its own
     *    namespace. */
    bench_setup_check(
        "CPT_ADD process2 switch target -> init process",
        rme_cpt_add(
            RME_BOOT_INIT_CPT,
            CAP_BENCH_P2_THD,
            CAP_BENCH_CPT,
            CAP2_THD2,
            RME_THD_FLAG_ALL,
        ),
    );
    bench_setup_check(
        "CPT_ADD init thread -> process2",
        rme_cpt_add(
            CAP_BENCH_CPT,
            CAP2_INIT_THD,
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
            &raw mut CAP_P2_THD,
            rme_cid_2l(RME_BOOT_INIT_CPT, CAP_BENCH_P2_THD),
        );
        core::ptr::write_volatile(&raw mut CAP_P1T1_IN_P2, CAP2_INIT_THD);
    }
}
/* End Function:bench_prepare_process2 *************************************/

/* Function:bench_prepare_signals ******************************************
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
fn bench_prepare_signals() {
    /* ---- Create the signal endpoints in the init capability table ---- */
    bench_setup_check("SIG_CRT self endpoint", bench_sig_crt(CAP_BENCH_SIG_SELF));
    bench_setup_check(
        "SIG_CRT init process parking",
        bench_sig_crt(CAP_BENCH_SIG_PARK),
    );
    bench_setup_check("SIG_CRT data channel", bench_sig_crt(CAP_BENCH_SIG_DATA));
    bench_setup_check(
        "SIG_CRT process2 parking",
        bench_sig_crt(CAP_BENCH_SIG_PARK2),
    );

    /* ---- Delegate the endpoints the second process needs ---- */
    bench_setup_check(
        "CPT_ADD data endpoint -> process2",
        rme_cpt_add(
            CAP_BENCH_CPT,
            CAP2_SIG_DATA,
            RME_BOOT_INIT_CPT,
            CAP_BENCH_SIG_DATA,
            RME_SIG_FLAG_ALL,
        ),
    );
    bench_setup_check(
        "CPT_ADD park2 endpoint -> process2",
        rme_cpt_add(
            CAP_BENCH_CPT,
            CAP2_SIG_PARK,
            RME_BOOT_INIT_CPT,
            CAP_BENCH_SIG_PARK2,
            RME_SIG_FLAG_ALL,
        ),
    );

    /* ---- Resolve the capability IDs ---- */
    unsafe {
        core::ptr::write_volatile(
            &raw mut CAP_SIG_SELF,
            rme_cid_2l(RME_BOOT_INIT_CPT, CAP_BENCH_SIG_SELF),
        );
        core::ptr::write_volatile(
            &raw mut CAP_SIG_PARK,
            rme_cid_2l(RME_BOOT_INIT_CPT, CAP_BENCH_SIG_PARK),
        );
        core::ptr::write_volatile(
            &raw mut CAP_SIG_PARK2,
            rme_cid_2l(RME_BOOT_INIT_CPT, CAP_BENCH_SIG_PARK2),
        );
        core::ptr::write_volatile(
            &raw mut CAP_SIG_DATA,
            rme_cid_2l(RME_BOOT_INIT_CPT, CAP_BENCH_SIG_DATA),
        );
    }
}
/* End Function:bench_prepare_signals **************************************/

/* Function:bench_prepare_invocation ***************************************
Description : Create the invocation stub that the invocation test uses. The
              stub enters the second process, so it runs with the second
              process' page table active.
Input       : None.
Output      : None.
Return      : None - A failed setup step halts the benchmark, so this
              function never returns in that case.
*****************************************************************************/
fn bench_prepare_invocation() {
    /* The invocation stub object lives in the kernel memory, right behind the
     * thread objects the preparation has allocated so far. */
    let raddr = bench_kom_alloc_inv();

    /* INV_CRT: Cid=Cap_Cpt, Param0=D1(Cap_Kom)|D0(Cap_Inv), Param1=Cap_Prc,
     * Param2=Raddr. The target process is the second process. */
    bench_setup_check(
        "INV_CRT round-trip stub",
        rme_inv_crt(
            RME_BOOT_INIT_CPT,
            RME_BOOT_INIT_KOM,
            CAP_BENCH_INV,
            CAP_BENCH_PRC,
            raddr,
        ),
    );

    /* INV_SET: Param0=D0(Cap_Inv)|D1(Is_Exc_Ret), Param1=Entry, Param2=Stack.
     * The stack lives in the init image's bss, which the second process
     * maps physically one to one. */
    let entry = bench_inv_entry as extern "C" fn(u64) -> ! as u64;
    let stack = bench_stack_top(&raw const BENCH_STACK_INV);
    bench_setup_check(
        "INV_SET round-trip stub",
        rme_inv_set(
            rme_cid_2l(RME_BOOT_INIT_CPT, CAP_BENCH_INV),
            entry,
            stack,
            0,
        ),
    );

    /* The second stub is used by the one-way INV_ACT test and as the inner
     * stub of the nested INV_RET test. */
    let raddr2 = bench_kom_alloc_inv();
    bench_setup_check(
        "INV_CRT one-way stub",
        rme_inv_crt(
            RME_BOOT_INIT_CPT,
            RME_BOOT_INIT_KOM,
            CAP_BENCH_INV2,
            CAP_BENCH_PRC,
            raddr2,
        ),
    );
    let entry2 = bench_inv_act_entry as extern "C" fn(u64) -> ! as u64;
    let stack2 = bench_stack_top(&raw const BENCH_STACK_INV2);
    bench_setup_check(
        "INV_SET one-way stub",
        rme_inv_set(
            rme_cid_2l(RME_BOOT_INIT_CPT, CAP_BENCH_INV2),
            entry2,
            stack2,
            0,
        ),
    );

    /* The nested stub runs inside process 2, so it needs the inner stub in
     * process 2's own capability table. */
    bench_setup_check(
        "CPT_ADD nested stub -> process2",
        rme_cpt_add(
            CAP_BENCH_CPT,
            CAP2_INV2,
            RME_BOOT_INIT_CPT,
            CAP_BENCH_INV2,
            RME_INV_FLAG_ALL,
        ),
    );

    /* KFN cap for the DEBUG_INV_MOD stub, which runs in process 2's namespace. */
    bench_setup_check(
        "CPT_ADD KFN -> process2",
        rme_cpt_add(
            CAP_BENCH_CPT,
            CAP2_KFN,
            RME_BOOT_INIT_CPT,
            RME_BOOT_INIT_KFN,
            RME_KFN_FLAG_FULL_RANGE,
        ),
    );
}
/* End Function:bench_prepare_invocation ************************************/

/* Function:bench_prepare_threads ******************************************
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
fn bench_prepare_threads() {
    /* The helper thread of the init process */
    let cap_p1t2 = rme_cid_2l(RME_BOOT_INIT_CPT, CAP_BENCH_THD);
    bench_setup_check(
        "THD_SCHED_BIND init process receiver",
        bench_thd_bind(cap_p1t2, BENCH_PRIO_RECV),
    );
    let entry = bench_p1t2_entry as extern "C" fn(u64) -> ! as u64;
    let stack = bench_stack_top(&raw const BENCH_STACK_P1T2);
    bench_setup_check(
        "THD_EXEC_SET init process receiver",
        rme_thd_exec_set(cap_p1t2, entry, stack, 0),
    );

    /* The receiver thread of the second process */
    let cap_p2t1 = rme_cid_2l(CAP_BENCH_CPT, CAP2_THD);
    bench_setup_check(
        "THD_SCHED_BIND process2 receiver",
        bench_thd_bind(cap_p2t1, BENCH_PRIO_RECV2),
    );
    let entry = bench_p2t1_entry as extern "C" fn(u64) -> ! as u64;
    let stack = bench_stack_top(&raw const BENCH_STACK_P2T1);
    bench_setup_check(
        "THD_EXEC_SET process2 receiver",
        rme_thd_exec_set(cap_p2t1, entry, stack, 0),
    );

    /* The switch target of the second process */
    let cap_p2t2 = unsafe { core::ptr::read_volatile(&raw const CAP_P2_THD) };
    bench_setup_check(
        "THD_SCHED_BIND process2 switch target",
        bench_thd_bind(cap_p2t2, BENCH_PRIO_INIT),
    );
    let entry = bench_p2t2_entry as extern "C" fn(u64) -> ! as u64;
    let stack = bench_stack_top(&raw const BENCH_STACK_P2T2);
    bench_setup_check(
        "THD_EXEC_SET process2 switch target",
        rme_thd_exec_set(cap_p2t2, entry, stack, 0),
    );

    /* The switch target of the init process */
    let cap_p1t3 = rme_cid_2l(RME_BOOT_INIT_CPT, CAP_BENCH_THD2);
    bench_setup_check(
        "THD_SCHED_BIND init process switch target",
        bench_thd_bind(cap_p1t3, BENCH_PRIO_INIT),
    );
    let entry = bench_p1t3_entry as extern "C" fn(u64) -> ! as u64;
    let stack = bench_stack_top(&raw const BENCH_STACK_P1T3);
    bench_setup_check(
        "THD_EXEC_SET init process switch target",
        rme_thd_exec_set(cap_p1t3, entry, stack, 0),
    );

    /* Give every helper a finite budget. The init thread intentionally stays
     * on its boot-time INIT_TIME budget; the helpers only need enough budget
     * to run until they park, and the higher-priority ones preempt the init
     * thread whenever a signal wakes them. */
    bench_setup_check(
        "THD_TIME_XFER init process receiver",
        rme_thd_time_xfer(cap_p1t2, RME_BOOT_INIT_THD, BENCH_TIME_XFER),
    );
    bench_setup_check(
        "THD_TIME_XFER init process switch target",
        rme_thd_time_xfer(cap_p1t3, RME_BOOT_INIT_THD, BENCH_TIME_XFER),
    );
    bench_setup_check(
        "THD_TIME_XFER process2 receiver",
        rme_thd_time_xfer(cap_p2t1, RME_BOOT_INIT_THD, BENCH_TIME_XFER),
    );
    bench_setup_check(
        "THD_TIME_XFER process2 switch target",
        rme_thd_time_xfer(cap_p2t2, RME_BOOT_INIT_THD, BENCH_TIME_XFER),
    );

    unsafe {
        core::ptr::write_volatile(&raw mut CAP_P1T3, cap_p1t3);
    }
}
/* End Function:bench_prepare_threads **************************************/

/* Function:bench_wake_helper **********************************************
Description : Publish a phase and wake the helper thread of the init process up.
              The helper thread runs at a higher priority, so the kernel
              switches to it right away and this call only returns once the
              helper thread has parked itself again.
Input       : phase - The phase the helper thread shall run.
Output      : None.
Return      : RmePtr - The kernel return value of SIG_SND (0 on success).
*****************************************************************************/
fn bench_wake_helper(phase: u32) -> RmePtr {
    let cap_park = unsafe { core::ptr::read_volatile(&raw const CAP_SIG_PARK) };

    bench_phase_set(phase);
    rme_sig_snd(cap_park, 1)
}
/* End Function:bench_wake_helper *******************************************/

/* Function:bench_wake_helper2 *********************************************
Description : Publish a phase and wake the receiver thread of the second
              process up. That thread runs two priority levels above the
              init thread, so the kernel switches to it right away.
Input       : phase - The phase the helper thread shall run.
Output      : None.
Return      : RmePtr - The kernel return value of SIG_SND (0 on success).
*****************************************************************************/
fn bench_wake_helper2(phase: u32) -> RmePtr {
    let cap_park = unsafe { core::ptr::read_volatile(&raw const CAP_SIG_PARK2) };

    bench_phase_set(phase);
    rme_sig_snd(cap_park, 1)
}
/* End Function:bench_wake_helper2 ******************************************/

/* Function:bench_test_yield_self *******************************************
Description : Measure the cost of switching to the calling thread itself.
              The kernel notices that the target is the current thread and
              returns without a context switch, so this is the cheapest
              possible thread switch - the baseline of the whole suite.
Input       : stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_test_yield_self(stat: &mut BenchStat) {
    let mut count: u64 = 0;

    stat.reset();

    while count < BENCH_LOOP_NUM && stat.fail == 0 {
        let start = bench_read_cycle();
        let _ = stat.set_ret(rme_thd_swt(RME_BOOT_INIT_THD, 0) as i64);
        let end = bench_read_cycle();

        stat.record((end - start) as u32);
        count += 1;
    }
}
/* End Function:bench_test_yield_self ****************************************/

/* Function:bench_test_signal_self ******************************************
Description : Measure the cost of a signal that is sent to, and received
              from, the very same thread. The helper thread of the init process
              runs the test, because the kernel forbids a boot-time thread
              from receiving a signal at all.
Input       : stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_test_signal_self(stat: &mut BenchStat) {
    /* Clear the shared slots before waking the helper: if the wake-up fails,
     * the test must see zero samples instead of the previous test's data. */
    stat.reset();
    bench_stat_reset();

    /* Wake the helper thread - it runs the whole test and parks itself
     * again before this call returns */
    if !bench_setup_try(
        stat,
        "SIG_SND wake init helper",
        bench_wake_helper(PHASE_SIG_SELF),
    ) {
        return;
    }

    stat.load_shared();
    stat.require_count(BENCH_LOOP_NUM);
}
/* End Function:bench_test_signal_self **************************************/

/* Function:bench_test_sig_snd **********************************************
Description : Send-only SIG_SND measurement. The endpoint has no receiver, so
              the call only increments the signal counter and does not switch.
Input       : stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_test_sig_snd(stat: &mut BenchStat) {
    if !bench_setup_try(
        stat,
        "SIG_CRT send-only",
        bench_sig_crt(CAP_BENCH_SIG_SND_ONLY),
    ) {
        return;
    }

    stat.reset();

    let mut count: u64 = 0;
    while count < BENCH_LOOP_NUM && stat.fail == 0 {
        let start = bench_read_cycle();
        let _ = stat.set_ret(rme_sig_snd(CAP_BENCH_SIG_SND_ONLY, 1) as i64);
        let end = bench_read_cycle();

        stat.record((end - start) as u32);
        count += 1;
    }

    let _ = rme_cpt_frz(RME_BOOT_INIT_CPT, CAP_BENCH_SIG_SND_ONLY);
    let _ = rme_sig_del(RME_BOOT_INIT_CPT, CAP_BENCH_SIG_SND_ONLY);
}
/* End Function:bench_test_sig_snd *******************************************/

/* Function:bench_test_sig_rcv_only *****************************************
Description : Receive-only SIG_RCV measurement, executed by the helper thread
              because a boot thread may not receive signals.
Input       : stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_test_sig_rcv_only(stat: &mut BenchStat) {
    if !bench_setup_try(
        stat,
        "SIG_CRT receive-only",
        bench_sig_crt(CAP_BENCH_SIG_RCV_ONLY),
    ) {
        return;
    }

    /* Clear the shared slots before waking the helper: a failed wake-up
     * must not leave the previous helper test's data visible. */
    stat.reset();
    bench_stat_reset();
    if !bench_setup_try(
        stat,
        "SIG_SND wake init helper",
        bench_wake_helper(PHASE_SIG_RCV_ONLY),
    ) {
        return;
    }
    stat.load_shared();
    stat.require_count(BENCH_LOOP_NUM);

    let _ = rme_cpt_frz(RME_BOOT_INIT_CPT, CAP_BENCH_SIG_RCV_ONLY);
    let _ = rme_sig_del(RME_BOOT_INIT_CPT, CAP_BENCH_SIG_RCV_ONLY);
}
/* End Function:bench_test_sig_rcv_only *************************************/

/* Function:bench_test_invocation *******************************************
Description : Measure the cost of a synchronous invocation. The invoker
              activates the stub, which runs in the second process'
              address space and returns immediately, so this is the full
              round trip through the kernel.
Input       : stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_test_invocation(stat: &mut BenchStat) {
    let cap_inv = rme_cid_2l(RME_BOOT_INIT_CPT, CAP_BENCH_INV);
    let mut count: u64 = 0;

    stat.reset();

    while count < BENCH_LOOP_NUM && stat.fail == 0 {
        let start = bench_read_cycle();
        let ret = rme_inv_act(cap_inv, 0x1234);
        let end = bench_read_cycle();

        /* a0 is the activation status; a1 (callee return) is not printed. */
        let _ = stat.set_ret(ret.status());
        stat.record((end - start) as u32);
        count += 1;
    }
}
/* End Function:bench_test_invocation ****************************************/

/* Function:bench_test_inv_act **********************************************
Description : One-way INV_ACT measurement. The stub latches its entry time and
              returns; the invoker reads that time after the call, so the
              measurement covers entry only.
Input       : stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_test_inv_act(stat: &mut BenchStat) {
    let cap_inv2 = rme_cid_2l(RME_BOOT_INIT_CPT, CAP_BENCH_INV2);
    let entry = bench_inv_act_entry as extern "C" fn(u64) -> ! as u64;
    let stack = bench_stack_top(&raw const BENCH_STACK_INV2);
    if !bench_setup_try(
        stat,
        "INV_SET one-way stub",
        rme_inv_set(cap_inv2, entry, stack, 0),
    ) {
        return;
    }

    stat.reset();

    let mut count: u64 = 0;
    while count < BENCH_LOOP_NUM && stat.fail == 0 {
        bench_start_set(bench_read_cycle());
        let ret = rme_inv_act(cap_inv2, 0x1111);

        let _ = stat.set_ret(ret.status());

        stat.record((bench_end_get() - bench_start_get()) as u32);
        count += 1;
    }
}
/* End Function:bench_test_inv_act *******************************************/

/* Function:bench_test_inv_ret **********************************************
Description : Nested INV_RET measurement. The outer stub invokes the inner
              stub; the inner stub latches START and executes INV_RET, and the
              outer stub latches END when its inner INV_ACT returns.
Input       : stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_test_inv_ret(stat: &mut BenchStat) {
    let cap_inv1 = rme_cid_2l(RME_BOOT_INIT_CPT, CAP_BENCH_INV);
    let cap_inv2 = rme_cid_2l(RME_BOOT_INIT_CPT, CAP_BENCH_INV2);
    let entry1 = bench_inv_nest_entry as extern "C" fn(u64) -> ! as u64;
    let entry2 = bench_inv_ret_entry as extern "C" fn(u64) -> ! as u64;
    let stack1 = bench_stack_top(&raw const BENCH_STACK_INV);
    let stack2 = bench_stack_top(&raw const BENCH_STACK_INV2);
    if !bench_setup_try(
        stat,
        "INV_SET nested outer stub",
        rme_inv_set(cap_inv1, entry1, stack1, 0),
    ) {
        return;
    }
    if !bench_setup_try(
        stat,
        "INV_SET nested inner stub",
        rme_inv_set(cap_inv2, entry2, stack2, 0),
    ) {
        return;
    }

    stat.reset();

    let mut count: u64 = 0;
    while count < BENCH_LOOP_NUM && stat.fail == 0 {
        unsafe {
            core::ptr::write_volatile(&raw mut BENCH_INV_NEST_ERR, 0);
        }

        let ret = rme_inv_act(cap_inv1, 0);
        let nest_err = unsafe { core::ptr::read_volatile(&raw const BENCH_INV_NEST_ERR) };

        let verdict = if !ret.ok() {
            ret.status()
        } else if nest_err < 0 {
            nest_err
        } else {
            ret.status()
        };
        let _ = stat.set_ret(verdict);

        stat.record((bench_end_get() - bench_start_get()) as u32);
        count += 1;
    }
}
/* End Function:bench_test_inv_ret *******************************************/

/* Function:bench_test_signal_intra *****************************************
Description : Measure the cost of a signal that travels between two threads
              of the same process. The init thread is the sender, the helper
              thread of the init process is the receiver.
Input       : stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_test_signal_intra(stat: &mut BenchStat) {
    let cap_data = unsafe { core::ptr::read_volatile(&raw const CAP_SIG_DATA) };
    let mut count: u64 = 0;
    let mut send_fail = false;

    /* Clear the shared slots before waking the helper: a failed wake-up
     * must not expose the previous helper test's data. */
    stat.reset();
    bench_stat_reset();

    /* Let the helper thread take the receiving role and wait for the
     * workload on the data channel */
    if !bench_setup_try(
        stat,
        "SIG_SND wake init helper",
        bench_wake_helper(PHASE_SIG_RECV),
    ) {
        return;
    }

    while count < BENCH_LOOP_NUM && !bench_stat_failed() {
        bench_start_set(bench_read_cycle());
        let ret = rme_sig_snd(cap_data, 1);
        if (ret as i64) < 0 {
            send_fail = true;
            break;
        }
        count += 1;
    }

    stat.reset();
    stat.load_shared();
    if send_fail || count != BENCH_LOOP_NUM {
        let _ = stat.set_ret(-1);
    }
    stat.require_count(BENCH_LOOP_NUM);
}
/* End Function:bench_test_signal_intra *************************************/

/* Function:bench_test_signal_inter *****************************************
Description : Measure the cost of a signal that travels between two threads
              of two different processes. The init thread is the sender, the
              receiver thread of the second process is the receiver.
Input       : stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_test_signal_inter(stat: &mut BenchStat) {
    let cap_data = unsafe { core::ptr::read_volatile(&raw const CAP_SIG_DATA) };
    let mut count: u64 = 0;
    let mut send_fail = false;

    /* Clear the shared slots before waking the helper: a failed wake-up
     * must not expose the previous helper test's data. */
    stat.reset();
    bench_stat_reset();

    /* Let the receiver in the second process take the receiving role and
     * wait for the workload on the data channel */
    if !bench_setup_try(
        stat,
        "SIG_SND wake process2 helper",
        bench_wake_helper2(PHASE_SIG_RECV2),
    ) {
        return;
    }

    while count < BENCH_LOOP_NUM && !bench_stat_failed() {
        bench_start_set(bench_read_cycle());
        let ret = rme_sig_snd(cap_data, 1);
        if (ret as i64) < 0 {
            send_fail = true;
            break;
        }
        count += 1;
    }

    stat.reset();
    stat.load_shared();
    if send_fail || count != BENCH_LOOP_NUM {
        let _ = stat.set_ret(-1);
    }
    stat.require_count(BENCH_LOOP_NUM);
}
/* End Function:bench_test_signal_inter *************************************/

/* Function:bench_test_yield_intra *******************************************
Description : Measure the cost of switching between two threads of the same
              process. The switch is one way: the target thread latches the
              arrival timestamp and switches straight back, so the
              measurement does not contain the cost of the way back.
              This is the counterpart of the inter-process yield test, and
              it covers the path the C bench_main.c measured with
              RME_Same_Prc_Thd_Switch_Test.
Input       : stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_test_yield_intra(stat: &mut BenchStat) {
    let cap_p1t3 = unsafe { core::ptr::read_volatile(&raw const CAP_P1T3) };
    let mut count: u64 = 0;

    stat.reset();

    while count < BENCH_LOOP_NUM && stat.fail == 0 {
        bench_start_set(bench_read_cycle());
        let _ = stat.set_ret(rme_thd_swt(cap_p1t3, 0) as i64);

        stat.record((bench_end_get() - bench_start_get()) as u32);
        count += 1;
    }
}
/* End Function:bench_test_yield_intra **************************************/

/* Function:bench_test_yield_intra_roundtrip ********************************
Description : Full intra-process switch round trip: init switches to the
              target, the target switches back, and init reads the cycle
              counter after THD_SWT returns.
*****************************************************************************/
fn bench_test_yield_intra_roundtrip(stat: &mut BenchStat) {
    let cap_p1t3 = unsafe { core::ptr::read_volatile(&raw const CAP_P1T3) };
    let mut count: u64 = 0;

    stat.reset();

    while count < BENCH_LOOP_NUM && stat.fail == 0 {
        let start = bench_read_cycle();
        let ret = rme_thd_swt(cap_p1t3, 0) as i64;
        let end = bench_read_cycle();

        let _ = stat.set_ret(ret);
        stat.record((end - start) as u32);
        count += 1;
    }
}
/* End Function:bench_test_yield_intra_roundtrip ****************************/

/* Function:bench_test_yield_inter ******************************************
Description : Measure the cost of switching into another process. The switch
              is one way: the target thread latches the arrival timestamp
              and switches straight back, so the measurement does not contain
              the cost of the way back.
Input       : stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_test_yield_inter(stat: &mut BenchStat) {
    let cap_p2t2 = unsafe { core::ptr::read_volatile(&raw const CAP_P2_THD) };
    let mut count: u64 = 0;

    stat.reset();

    while count < BENCH_LOOP_NUM && stat.fail == 0 {
        bench_start_set(bench_read_cycle());
        let _ = stat.set_ret(rme_thd_swt(cap_p2t2, 0) as i64);

        stat.record((bench_end_get() - bench_start_get()) as u32);
        count += 1;
    }
}
/* End Function:bench_test_yield_inter *************************************/

/* Function:bench_test_yield_inter_roundtrip ********************************
Description : Full inter-process switch round trip: init switches to the
              process-2 target, the target switches back, and init reads the
              cycle counter after THD_SWT returns.
*****************************************************************************/
fn bench_test_yield_inter_roundtrip(stat: &mut BenchStat) {
    let cap_p2t2 = unsafe { core::ptr::read_volatile(&raw const CAP_P2_THD) };
    let mut count: u64 = 0;

    stat.reset();

    while count < BENCH_LOOP_NUM && stat.fail == 0 {
        let start = bench_read_cycle();
        let ret = rme_thd_swt(cap_p2t2, 0) as i64;
        let end = bench_read_cycle();

        let _ = stat.set_ret(ret);
        stat.record((end - start) as u32);
        count += 1;
    }
}
/* End Function:bench_test_yield_inter_roundtrip ****************************/

/* Function:bench_test_kfn_pgt_entry_mod *************************************
Description : Measure KFN PGT_ENTRY_MOD: walk the init page table at
              0x20000000 and query flags/size/num order. The KFN handler
              clears a0 on success, so ret is 0, not the queried value.
Input       : stat - The statistics accumulator.
              operation - FLAG_GET, SZORD_GET or NMORD_GET.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_test_kfn_pgt_entry_mod(stat: &mut BenchStat, operation: RmePtr) {
    stat.reset();

    let mut count: u64 = 0;
    while count < BENCH_LOOP_NUM && stat.fail == 0 {
        let start = bench_read_cycle();
        let _ = stat.set_ret(rme_kfn(
            RME_BOOT_INIT_KFN,
            RME_KFN_PGT_ENTRY_MOD,
            RME_BOOT_INIT_PGT,
            0x20000000,
            operation,
        ) as i64);
        let end = bench_read_cycle();

        stat.record((end - start) as u32);
        count += 1;
    }
}
/* End Function:bench_test_kfn_pgt_entry_mod *********************************/

/* Function:bench_test_kfn_pgt_asid_set *************************************
Description : Measure KFN PGT_ASID_SET on process 2's root page table. The
              ASID alternates between 2 and 3, so every round flushes the old
              and the new ASID before the change.
Input       : stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_test_kfn_pgt_asid_set(stat: &mut BenchStat) {
    stat.reset();

    let mut asid: RmePtr = 3;
    let mut count: u64 = 0;
    while count < BENCH_LOOP_NUM && stat.fail == 0 {
        let start = bench_read_cycle();
        let ret = rme_kfn(
            RME_BOOT_INIT_KFN,
            RME_KFN_PGT_ASID_SET,
            CAP_BENCH_PGT_TOP_NEW,
            asid,
            0,
        );
        let end = bench_read_cycle();

        let _ = stat.set_ret(ret as i64);
        stat.record((end - start) as u32);

        asid = if asid == 3 { 2 } else { 3 };
        count += 1;
    }
}
/* End Function:bench_test_kfn_pgt_asid_set *********************************/

/* Function:bench_test_kfn_pgt_cache_clr *************************************
Description : Measure KFN PGT_CACHE_CLR: a full local TLB flush.
Input       : stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_test_kfn_pgt_cache_clr(stat: &mut BenchStat) {
    stat.reset();

    let mut count: u64 = 0;
    while count < BENCH_LOOP_NUM && stat.fail == 0 {
        let start = bench_read_cycle();
        let ret = rme_kfn(RME_BOOT_INIT_KFN, RME_KFN_PGT_CACHE_CLR, 0, 0, 0);
        let end = bench_read_cycle();

        let _ = stat.set_ret(ret as i64);
        stat.record((end - start) as u32);
        count += 1;
    }
}
/* End Function:bench_test_kfn_pgt_cache_clr *********************************/

/* Function:bench_test_kfn_pgt_line_clr **************************************
Description : Measure KFN PGT_LINE_CLR on the init image page, all ASIDs.
Input       : stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_test_kfn_pgt_line_clr(stat: &mut BenchStat) {
    stat.reset();

    let mut count: u64 = 0;
    while count < BENCH_LOOP_NUM && stat.fail == 0 {
        let start = bench_read_cycle();
        let ret = rme_kfn(
            RME_BOOT_INIT_KFN,
            RME_KFN_PGT_LINE_CLR,
            0,          /* Sub_ID is unused by this KFN */
            0x20000000, /* Vaddr: init image window */
            0,          /* ASID 0: all ASIDs */
        );
        let end = bench_read_cycle();

        let _ = stat.set_ret(ret as i64);
        stat.record((end - start) as u32);
        count += 1;
    }
}
/* End Function:bench_test_kfn_pgt_line_clr *********************************/

/* Function:bench_test_kfn_cache_maint **************************************
Description : Measure one KFN CACHE_MAINT operation on the first cache block
              of a mapped static buffer. ICACHE uses fence.i; DCACHE uses
              Zicbom clean/inval/flush.
Input       : stat - The statistics accumulator.
              cache_id - RME_RV64V_KFN_CACHE_ICACHE or DCACHE.
              operation - The cache maintenance operation.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_test_kfn_cache_maint(stat: &mut BenchStat, cache_id: RmePtr, operation: RmePtr) {
    let addr = (unsafe { core::ptr::addr_of!(BENCH_STACK_TEST[0]) as u64 })
        & !(RME_RV64V_CACHE_BLOCK_SIZE - 1);

    stat.reset();

    let mut count: u64 = 0;
    while count < BENCH_LOOP_NUM && stat.fail == 0 {
        let start = bench_read_cycle();
        let _ = stat.set_ret(rme_kfn(
            RME_BOOT_INIT_KFN,
            RME_KFN_CACHE_MAINT,
            cache_id,
            operation,
            addr,
        ) as i64);
        let end = bench_read_cycle();

        stat.record((end - start) as u32);
        count += 1;
    }
}
/* End Function:bench_test_kfn_cache_maint **********************************/

/* Function:bench_test_kfn_debug_print **************************************
Description : Measure KFN DEBUG_PRINT: one SBI console character per round,
              so the result includes the console cost.
Input       : stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_test_kfn_debug_print(stat: &mut BenchStat) {
    stat.reset();

    let mut count: u64 = 0;
    while count < BENCH_LOOP_REDUCED_NUM && stat.fail == 0 {
        let start = bench_read_cycle();
        let _ = stat
            .set_ret(rme_kfn(RME_BOOT_INIT_KFN, RME_KFN_DEBUG_PRINT, b'.' as RmePtr, 0, 0) as i64);
        let end = bench_read_cycle();

        stat.record((end - start) as u32);
        count += 1;
    }

    /* End the dots line before the report line. */
    bench_print_str("\r\n");
}
/* End Function:bench_test_kfn_debug_print **********************************/

/* Function:bench_test_kfn_debug_reg_mod ************************************
Description : Measure KFN DEBUG_REG_MOD: read the calling thread's A0. The
              value is returned in a1 (not captured); the KFN returns 0.
Input       : stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_test_kfn_debug_reg_mod(stat: &mut BenchStat) {
    stat.reset();

    let mut count: u64 = 0;
    while count < BENCH_LOOP_NUM && stat.fail == 0 {
        let start = bench_read_cycle();
        let _ = stat.set_ret(rme_kfn(
            RME_BOOT_INIT_KFN,
            RME_KFN_DEBUG_REG_MOD,
            RME_BOOT_INIT_THD,
            RME_RV64V_KFN_DEBUG_REG_MOD_X10_A0,
            0,
        ) as i64);
        let end = bench_read_cycle();

        stat.record((end - start) as u32);
        count += 1;
    }
}
/* End Function:bench_test_kfn_debug_reg_mod ********************************/

/* Function:bench_test_kfn_debug_exc_get ************************************
Description : Measure KFN DEBUG_EXC_GET: read the calling thread's exception
              cause/addr/value. The value is returned in a2 (not captured);
              the KFN returns 0.
Input       : stat - The statistics accumulator.
              operation - CAUSE_GET, ADDR_GET or VALUE_GET.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_test_kfn_debug_exc_get(stat: &mut BenchStat, operation: RmePtr) {
    stat.reset();

    let mut count: u64 = 0;
    while count < BENCH_LOOP_NUM && stat.fail == 0 {
        let start = bench_read_cycle();
        let _ = stat.set_ret(rme_kfn(
            RME_BOOT_INIT_KFN,
            RME_KFN_DEBUG_EXC_GET,
            RME_BOOT_INIT_THD,
            operation,
            0,
        ) as i64);
        let end = bench_read_cycle();

        stat.record((end - start) as u32);
        count += 1;
    }
}
/* End Function:bench_test_kfn_debug_exc_get ********************************/

/* Function:bench_test_kfn_debug_inv_mod ************************************
Description : Measure KFN DEBUG_INV_MOD layer-0 SP query from inside an
              invocation. The stub measures the KFN; the invoker reads the
              shared timestamps and return value.
Input       : stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_test_kfn_debug_inv_mod(stat: &mut BenchStat) {
    let cap_inv = rme_cid_2l(RME_BOOT_INIT_CPT, CAP_BENCH_INV);
    let stack = bench_stack_top(&raw const BENCH_STACK_INV);
    let entry = bench_inv_dbg_entry as extern "C" fn(u64) -> ! as u64;
    if !bench_setup_try(
        stat,
        "INV_SET debug stub",
        rme_inv_set(cap_inv, entry, stack, 0),
    ) {
        return;
    }
    stat.reset();

    let mut count: u64 = 0;
    while count < BENCH_LOOP_NUM && stat.fail == 0 {
        unsafe {
            core::ptr::write_volatile(&raw mut BENCH_KFN_INV_RET, i64::MIN);
        }

        let ret = rme_inv_act(cap_inv, 0);
        let start = bench_start_get();
        let end = bench_end_get();
        let kfn_ret = unsafe { core::ptr::read_volatile(&raw const BENCH_KFN_INV_RET) };

        if !ret.ok() {
            let _ = stat.set_ret(ret.status());
        } else if kfn_ret == i64::MIN {
            /* The stub never ran and published a result. */
            let _ = stat.set_ret(-1);
        } else {
            let _ = stat.set_ret(kfn_ret);
        }

        stat.record((end - start) as u32);
        count += 1;
    }

    /* Restore the shared stub (this test runs last). */
    let entry = bench_inv_entry as extern "C" fn(u64) -> ! as u64;
    let _ = rme_inv_set(cap_inv, entry, stack, 0);
}
/* End Function:bench_test_kfn_debug_inv_mod ********************************/

/* Function:bench_test_kfn_idle_sleep ***************************************
Description : Measure KFN IDLE_SLEEP. The kernel executes WFI and returns on
              the next timer tick, so the result should include the sleep
              latency. The cycle CSR is also recorded for comparison, but on
              cores that clock-gate the hart in WFI (SpacemiT K3/X100) it does
              not advance during the sleep and therefore only shows the SVC
              round trip; the time CSR is the authoritative number there.
Input       : stat - The cycle statistics accumulator.
              time_stat - The time (timebase tick) statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_test_kfn_idle_sleep(stat: &mut BenchStat, time_stat: &mut BenchStat) {
    stat.reset();
    time_stat.reset();

    let mut count: u64 = 0;
    while count < BENCH_LOOP_REDUCED_NUM && stat.fail == 0 {
        let start = bench_read_cycle();
        let time_start = bench_read_time();
        let ret = rme_kfn(RME_BOOT_INIT_KFN, RME_KFN_IDLE_SLEEP, 0, 0, 0) as i64;
        let time_end = bench_read_time();
        let end = bench_read_cycle();

        let _ = stat.set_ret(ret);
        let _ = time_stat.set_ret(ret);
        stat.record((end - start) as u32);
        time_stat.record((time_end - time_start) as u32);
        count += 1;
    }
}
/* End Function:bench_test_kfn_idle_sleep ***********************************/

/* Function:bench_main ***************************************************
Description : The benchmark entry, also the init thread. It runs every test
              of the suite and prints the results in the order of the
              reference benchmark.
Input       : None.
Output      : None.
Return      : ! - This function never returns.
*****************************************************************************/
pub fn bench_main() -> ! {
    let mut stat = BenchStat::new();
    let mut time_stat = BenchStat::new();

    /* Print table header - the banner is generated with
     * "figlet -f smslant -k RME" */
    bench_print_str("\r\n");
    bench_print_str("   ___   __  ___ ____\r\n");
    bench_print_str("  / _ \\ /  |/  // __/\r\n");
    bench_print_str(" / , _// /|_/ // _/\r\n");
    bench_print_str("/_/|_|/_/  /_//___/\r\n");
    bench_print_str("RME Concord Microkernel - Standard benchmark test\r\n");
    bench_print_str("====================================================\r\n");
    bench_print!(
        "{:<44} : AVG / MAX / MIN / ret\r\n",
        "Test (number in CPU cycles)"
    );

    /* Prepare the shared fixtures. A failed preparation step means that the
     * measurement environment is broken, so the benchmark is halted right
     * there: the remaining tests would only produce misleading numbers. */
    bench_prepare_process2();
    bench_prepare_signals();
    bench_prepare_invocation();
    bench_prepare_threads();

    /* ---- THD_SWT: switch variants ---- */
    bench_test_yield_self(&mut stat);
    bench_report("THD_SWT(self)", &stat);

    bench_test_yield_intra(&mut stat);
    bench_report("THD_SWT(intra, one-way)", &stat);
    bench_test_yield_intra_roundtrip(&mut stat);
    bench_report("THD_SWT(intra, round-trip)", &stat);

    bench_test_yield_inter(&mut stat);
    bench_report("THD_SWT(inter, one-way)", &stat);
    bench_test_yield_inter_roundtrip(&mut stat);
    bench_report("THD_SWT(inter, round-trip)", &stat);

    /* ---- Signal ---- */
    bench_test_sig_snd(&mut stat);
    bench_report("SIG_SND", &stat);

    bench_test_sig_rcv_only(&mut stat);
    bench_report("SIG_RCV", &stat);
    bench_test_signal_self(&mut stat);
    bench_report("SIG_SND+SIG_RCV(self)", &stat);

    bench_test_signal_intra(&mut stat);
    bench_report("SIG_SND+SIG_RCV(intra)", &stat);

    bench_test_signal_inter(&mut stat);
    bench_report("SIG_SND+SIG_RCV(inter)", &stat);

    /* ---- Invocation ---- */
    bench_test_inv_act(&mut stat);
    bench_report("INV_ACT(one-way entry)", &stat);
    bench_test_inv_ret(&mut stat);
    bench_report("INV_RET(nested return)", &stat);
    bench_test_invocation(&mut stat);
    bench_report("INV_ACT+INV_RET(round-trip)", &stat);

    /* ---- KFN: testable success paths ---- */
    bench_test_kfn_pgt_entry_mod(&mut stat, RME_RV64V_KFN_PGT_ENTRY_MOD_FLAG_GET);
    bench_report("KFN(PGT_ENTRY_MOD flag)", &stat);
    bench_test_kfn_pgt_entry_mod(&mut stat, RME_RV64V_KFN_PGT_ENTRY_MOD_SZORD_GET);
    bench_report("KFN(PGT_ENTRY_MOD size)", &stat);
    bench_test_kfn_pgt_entry_mod(&mut stat, RME_RV64V_KFN_PGT_ENTRY_MOD_NMORD_GET);
    bench_report("KFN(PGT_ENTRY_MOD num)", &stat);

    bench_test_kfn_pgt_asid_set(&mut stat);
    bench_report("KFN(PGT_ASID_SET)", &stat);

    bench_test_kfn_pgt_cache_clr(&mut stat);
    bench_report("KFN(PGT_CACHE_CLR)", &stat);
    bench_test_kfn_pgt_line_clr(&mut stat);
    bench_report("KFN(PGT_LINE_CLR)", &stat);

    bench_test_kfn_cache_maint(
        &mut stat,
        RME_RV64V_KFN_CACHE_ICACHE,
        RME_RV64V_KFN_CACHE_INV_ALL,
    );
    bench_report("KFN(CACHE_MAINT icache)", &stat);
    bench_test_kfn_cache_maint(
        &mut stat,
        RME_RV64V_KFN_CACHE_DCACHE,
        RME_RV64V_KFN_CACHE_CLEAN_ADDR,
    );
    bench_report("KFN(CACHE_MAINT dclean)", &stat);
    bench_test_kfn_cache_maint(
        &mut stat,
        RME_RV64V_KFN_CACHE_DCACHE,
        RME_RV64V_KFN_CACHE_INV_ADDR,
    );
    bench_report("KFN(CACHE_MAINT dinval)", &stat);
    bench_test_kfn_cache_maint(
        &mut stat,
        RME_RV64V_KFN_CACHE_DCACHE,
        RME_RV64V_KFN_CACHE_CLEAN_INV_ADDR,
    );
    bench_report("KFN(CACHE_MAINT dflush)", &stat);

    bench_test_kfn_debug_print(&mut stat);
    bench_report("KFN(DEBUG_PRINT)", &stat);

    bench_test_kfn_debug_reg_mod(&mut stat);
    bench_report("KFN(DEBUG_REG_MOD get)", &stat);

    bench_test_kfn_debug_exc_get(&mut stat, RME_RV64V_KFN_DEBUG_EXC_CAUSE_GET);
    bench_report("KFN(DEBUG_EXC_GET cause)", &stat);
    bench_test_kfn_debug_exc_get(&mut stat, RME_RV64V_KFN_DEBUG_EXC_ADDR_GET);
    bench_report("KFN(DEBUG_EXC_GET addr)", &stat);
    bench_test_kfn_debug_exc_get(&mut stat, RME_RV64V_KFN_DEBUG_EXC_VALUE_GET);
    bench_report("KFN(DEBUG_EXC_GET value)", &stat);

    bench_test_kfn_debug_inv_mod(&mut stat);
    bench_report("KFN(DEBUG_INV_MOD sp)", &stat);

    bench_test_kfn_idle_sleep(&mut stat, &mut time_stat);
    bench_report("KFN(IDLE_SLEEP tick, cycle CSR)", &stat);
    bench_report("KFN(IDLE_SLEEP tick, time CSR)", &time_stat);

    bench_print_str("\r\nRME BENCH REPORT OK\r\n");

    /* Idle forever - busy loop */
    loop {
        core::hint::spin_loop();
    }
}
/* End Function:bench_main ***********************************************/

/* End Of File ***************************************************************/

/* Copyright (C) Evo-Devo Instrum. All rights reserved ***********************/
