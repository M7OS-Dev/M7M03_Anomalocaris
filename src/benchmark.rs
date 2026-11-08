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
                - Tick preemption         timer timeout notification
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
                  the signals of the intra-process test and burns a timeslice
                  in the tick preemption test.
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

/* Reduced rounds for the heavy tests: tick preemption/IDLE_SLEEP (one tick per round)
 * and DEBUG_PRINT (console output). */
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
const PHASE_TICK_PREEMPT: u32 = 3; /* Process 1's helper burns a timeslice */
const PHASE_SIG_RECV2: u32 = 4; /* Process 2's helper receives and measures */
const PHASE_SIG_RCV_ONLY: u32 = 5; /* Process 1's helper measures receive only */

/* The kernel memory frontier for the objects the benchmark creates. The
 * kernel's boot-time objects occupy the first part of KOM, so the benchmark
 * starts allocating at 0x10000 (64KB) to stay clear of them. */
/* The kernel memory frontier for the objects the benchmark creates. The
 * init image is physically at 0x81030000 and the 32-page mapping ends at
 * 0x81050000, i.e. KOM-relative 0x50000. Start above that to avoid
 * overwriting the image (and its .bss) with kernel objects. */
const BENCH_KOM_FRONTIER: RmePtr = 0x60000;

/* The lifecycle/state tests allocate from a higher bump pointer so they never
 * overlap the process2/invocation objects above. */
const BENCH_TEST_KOM_FRONTIER: RmePtr = 0x100000;

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

/* Scratch capability slots for the lifecycle/state SVC tests. These are all
 * inside the init capability table and are used sequentially; each test
 * creates its objects, measures, and tears them down before the next test. */
const CAP_TEST_THD: RmePtr = 25;
const CAP_TEST_CPT: RmePtr = 26;
const CAP_TEST_PGT: RmePtr = 27;
const CAP_TEST_PGT2: RmePtr = 28;
const CAP_TEST_SIG: RmePtr = 29;
const CAP_TEST_INV: RmePtr = 30;
const CAP_TEST_PRC: RmePtr = 31;
const CAP_TEST_CPT2: RmePtr = 32;
const CAP_TEST_PGT3: RmePtr = 33;
const CAP_TEST_PRC2: RmePtr = 34;
const CAP_TEST_SIG2: RmePtr = 35;
const CAP_TEST_INV2: RmePtr = 36;
const CAP_TEST_THD2: RmePtr = 37;
const CAP_TEST_PGT_PARENT: RmePtr = 38;
const CAP_TEST_PGT_CHILD: RmePtr = 39;
const CAP_TEST_PGT_DST: RmePtr = 40;
const CAP_TEST_CPT3: RmePtr = 41;
/* PGT_CON needs one fresh child table per position because PGT_DES is broken
 * in the current kernel (see the report note). 16 children fit in 42..57. */
const CAP_TEST_PGT_CON_PARENT: RmePtr = 58;
const CAP_TEST_PGT_CHILD_BASE: RmePtr = 42;
const BENCH_PGT_CON_ROUNDS: u64 = 16;

/* Rounds for the object lifecycle tests. Creating/deleting kernel objects is
 * much heavier than a pure syscall, so fewer rounds are enough. */
const BENCH_OBJ_LOOP_NUM: u64 = 1000;

/* Upper bound on how long the measuring thread waits for a helper event before
 * declaring the test failed. This prevents a broken helper from hanging the
 * whole suite. It is measured in cycle CSR units; the tick preemption test expects one
 * tick, which is far below this bound on QEMU virt and K3. */
const BENCH_WAIT_TIMEOUT_CYCLES: u64 = 200_000_000;
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
    RME_THD_FLAG_ALL, RME_THD_FLAG_SWT, RmePtr, rme_cid_2l, rme_cpt_add, rme_cpt_crt, rme_cpt_del,
    rme_cpt_frz, rme_cpt_rem, rme_inv_act, rme_inv_crt, rme_inv_del, rme_inv_ret, rme_inv_set,
    rme_kfn, rme_pgt_add, rme_pgt_con, rme_pgt_crt, rme_pgt_del, rme_pgt_des, rme_pgt_rem,
    rme_prc_cpt, rme_prc_crt, rme_prc_del, rme_prc_pgt, rme_sig_crt, rme_sig_del, rme_sig_rcv,
    rme_sig_snd, rme_thd_crt, rme_thd_del, rme_thd_exec_set, rme_thd_sched_bind,
    rme_thd_sched_free, rme_thd_sched_prio, rme_thd_sched_rcv, rme_thd_swt, rme_thd_time_xfer,
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

/* The shared phase and stop flags, published by the measuring thread and
 * observed by the helper threads. */
static mut BENCH_PHASE: u32 = PHASE_IDLE;
static mut BENCH_STOP: u32 = 0;

/* The timestamp the sender latches before it hands the workload over, and
 * the timestamp the receiver latches when the workload arrives. */
static mut BENCH_START: u64 = 0;
static mut BENCH_END: u64 = 0;

/* The tickets of the thread switches. The measuring thread raises a ticket
 * before it switches to a switch target, and the target latches the arrival
 * timestamp once per raised ticket. A counter is used instead of a flag, so
 * that the target performs exactly one action per switch, and never one
 * action too many.
 * The two switch targets each have their own ticket: both of them spin while
 * they are idle, and a shared ticket would let one of them swallow the
 * other's switch. */
static mut BENCH_TICKET_INTRA: u64 = 0;
static mut BENCH_TICKET_INTER: u64 = 0;

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
static mut CAP_P1T2: RmePtr = 0;
static mut CAP_P1T3: RmePtr = 0;
static mut CAP_P2_THD: RmePtr = 0;
static mut CAP_P1T1_IN_P2: RmePtr = 0;
static mut CAP_SIG_SELF: RmePtr = 0;
static mut CAP_SIG_PARK: RmePtr = 0;
static mut CAP_SIG_PARK2: RmePtr = 0;
static mut CAP_SIG_DATA: RmePtr = 0;

/* The monotonic KOM allocator. Kernel objects must occupy distinct KOM
 * addresses; we never free them, the benchmark's total footprint is tiny
 * compared with the 496MB KOM. */
static mut BENCH_KOM_NEXT: RmePtr = BENCH_TEST_KOM_FRONTIER;
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

/* Function:bench_kom_alloc **************************************************
Description : Allocate a distinct KOM range for a kernel object. The address is
              relative to the KOM capability's Begin, as the kernel expects.
              KOM is never reclaimed here; the bump pointer is enough.
Input       : size - The number of bytes needed.
              align - The required alignment.
Output      : None.
Return      : RmePtr - The aligned relative KOM address.
*****************************************************************************/
#[inline(always)]
fn bench_kom_alloc(size: RmePtr, align: RmePtr) -> RmePtr {
    unsafe {
        let mut addr = BENCH_KOM_NEXT;
        addr = (addr + align - 1) & !(align - 1);
        /* KOM allocation granularity is 64 bytes. */
        BENCH_KOM_NEXT = addr + ((size + 63) & !63);
        addr
    }
}
/* End Function:bench_kom_alloc **********************************************/

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

/* Function:bench_ticket_intra_get *******************************************
Description : Read the intra-process thread switch ticket.
Input       : None.
Output      : None.
Return      : u64 - The current ticket.
*****************************************************************************/
#[inline(always)]
fn bench_ticket_intra_get() -> u64 {
    unsafe { core::ptr::read_volatile(&raw const BENCH_TICKET_INTRA) }
}
/* End Function:bench_ticket_intra_get **************************************/

/* Function:bench_ticket_intra_raise ****************************************
Description : Raise the intra-process thread switch ticket, so that the
              switch target performs exactly one arrival latch for it.
Input       : None.
Output      : None.
Return      : None.
*****************************************************************************/
#[inline(always)]
fn bench_ticket_intra_raise() {
    let ticket = bench_ticket_intra_get();

    unsafe { core::ptr::write_volatile(&raw mut BENCH_TICKET_INTRA, ticket + 1) }
}
/* End Function:bench_ticket_intra_raise ************************************/

/* Function:bench_ticket_inter_get *******************************************
Description : Read the inter-process thread switch ticket.
Input       : None.
Output      : None.
Return      : u64 - The current ticket.
*****************************************************************************/
#[inline(always)]
fn bench_ticket_inter_get() -> u64 {
    unsafe { core::ptr::read_volatile(&raw const BENCH_TICKET_INTER) }
}
/* End Function:bench_ticket_inter_get **************************************/

/* Function:bench_ticket_inter_raise ****************************************
Description : Raise the inter-process thread switch ticket, so that the
              switch target performs exactly one arrival latch for it.
Input       : None.
Output      : None.
Return      : None.
*****************************************************************************/
#[inline(always)]
fn bench_ticket_inter_raise() {
    let ticket = bench_ticket_inter_get();

    unsafe { core::ptr::write_volatile(&raw mut BENCH_TICKET_INTER, ticket + 1) }
}
/* End Function:bench_ticket_inter_raise ************************************/

/* Function:bench_stop_set **************************************************
Description : Publish the shared stop flag.
Input       : value - The new value of the flag.
Output      : None.
Return      : None.
*****************************************************************************/
#[inline(always)]
fn bench_stop_set(value: u32) {
    unsafe { core::ptr::write_volatile(&raw mut BENCH_STOP, value) }
}
/* End Function:bench_stop_set **********************************************/

/* Function:bench_stop_get ***************************************************
Description : Read the shared stop flag.
Input       : None.
Output      : None.
Return      : u32 - The stop flag.
*****************************************************************************/
#[inline(always)]
fn bench_stop_get() -> u32 {
    unsafe { core::ptr::read_volatile(&raw const BENCH_STOP) }
}
/* End Function:bench_stop_get ***********************************************/

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
Description : Compute the stack pointer of a helper thread. The RISC-V ABI
              requires the stack pointer to be 16 byte aligned.
Input       : base - The address of the last word of the stack array.
Output      : None.
Return      : u64 - The stack pointer.
*****************************************************************************/
#[inline(always)]
fn bench_stack_top(base: u64) -> u64 {
    (base + 8) & !0xF
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

/* Function:bench_noop_entry *************************************************
Description : A do-nothing entry used by THD_EXEC_SET. The thread is never
              given a time slice during that test, so it must never run.
Input       : _param - Unused.
Output      : None.
Return      : ! - This function never returns.
*****************************************************************************/
extern "C" fn bench_noop_entry(_param: u64) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
/* End Function:bench_noop_entry *********************************************/

/* Function:bench_p1t2_entry ***********************************************
Description : The entry point of the helper thread of the init process. It plays the
              receiving role in the intra-process signal test and the load
              role in the tick preemption test.
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
            PHASE_TICK_PREEMPT => {
                bench_tick_load();
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
    /* The ticket of the previous inter-process switch this thread served. */
    let mut served_ticket: u64 = 0;

    loop {
        /* The measuring thread raises the ticket before it switches to us,
         * so the ticket tells us that a switch is waiting for us. One
         * ticket is served exactly once. */
        let ticket = bench_ticket_inter_get();

        if ticket != served_ticket {
            served_ticket = ticket;

            /* Latch the arrival of the switch, then hand the CPU back */
            bench_end_set(bench_read_cycle());
            let cap_p1t1 = unsafe { core::ptr::read_volatile(&raw const CAP_P1T1_IN_P2) };
            let _ = rme_thd_swt(cap_p1t1, 0);
        } else {
            /* Nothing to do - wait for the measuring thread to switch to
             * us. A thread switch needs the very same priority, so this
             * thread stays on the init thread's priority level. */
            core::hint::spin_loop();
        }
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
    /* The ticket of the previous intra-process switch this thread served. */
    let mut served_ticket: u64 = 0;

    loop {
        /* The measuring thread raises the ticket before it switches to us,
         * so the ticket tells us that a switch is waiting for us. One
         * ticket is served exactly once. */
        let ticket = bench_ticket_intra_get();

        if ticket != served_ticket {
            served_ticket = ticket;

            /* Latch the arrival of the switch, then hand the CPU back */
            bench_end_set(bench_read_cycle());
            let _ = rme_thd_swt(RME_BOOT_INIT_THD, 0);
        } else {
            /* Nothing to do - wait for the measuring thread to switch to
             * us. A thread switch needs the very same priority, so this
             * thread stays on the init thread's priority level. */
            core::hint::spin_loop();
        }
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

/* Function:bench_tick_load ***********************************************
Description : The load side of the tick preemption test. The helper thread burns its
              only timeslice in a tight loop, so that the next kernel tick
              preempts it and notifies its scheduler - the measuring thread.
              The timestamp it latches before the loop is the departure of
              the workload.
Input       : None.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_tick_load() {
    /* Latch the departure of the workload */
    bench_start_set(bench_read_cycle());

    /* Burn the timeslice until the measuring thread lets us go */
    while bench_stop_get() == 0 {
        core::hint::spin_loop();
    }
}
/* End Function:bench_tick_load *******************************************/

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
    let mut raddr: u64;

    /* ---- Create the new process's capability table (16 entries) ---- */
    /* CPT_CRT: Cid=Cap_Cpt_Crt, Param0=D1(Cap_Kom)|D0(Cap_Crt),
     * Param1=Raddr, Param2=Entry_Num */
    raddr = BENCH_KOM_FRONTIER;
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
    raddr += 0x400;

    /* ---- Create the new process's page tables (root + L1 + L2) ---- */
    /* Root: 1GB entries, covers the low half. Raddr must be 4KB aligned
     * because Sv39 finds a table through its PPN. */
    raddr = (raddr + 0xFFF) & !0xFFF;
    bench_setup_check(
        "PGT_CRT process2 root",
        bench_pgt_crt(CAP_BENCH_PGT_TOP_NEW, raddr, 0, 1, RME_PGT_SIZE_1G),
    );
    raddr += 0x1000;
    /* L1: 2MB entries, covers the user window [0,1GB) */
    bench_setup_check(
        "PGT_CRT process2 L1",
        bench_pgt_crt(CAP_BENCH_PGT_L1, raddr, 0, 0, RME_PGT_SIZE_2M),
    );
    raddr += 0x1000;
    /* L2: 4KB entries, covers [0x20000000,0x20200000) */
    bench_setup_check(
        "PGT_CRT process2 L2",
        bench_pgt_crt(CAP_BENCH_PGT_L2, raddr, 0x20000000, 0, RME_PGT_SIZE_4K),
    );
    raddr += 0x1000;

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
    raddr = (raddr + 0xFFF) & !0xFFF;
    bench_setup_check(
        "THD_CRT process2 receiver",
        bench_thd_crt(CAP_BENCH_CPT, CAP2_THD, CAP_BENCH_PRC, raddr),
    );
    raddr += 0x1000;

    /* ---- Create the switch target thread in the new process' OWN captbl ---- */
    bench_setup_check(
        "THD_CRT process2 switch target",
        bench_thd_crt(CAP_BENCH_CPT, CAP2_THD2, CAP_BENCH_PRC, raddr),
    );
    raddr += 0x1000;

    /* ---- Create the helper thread of the init process ---- */
    bench_setup_check(
        "THD_CRT init process receiver",
        bench_thd_crt(RME_BOOT_INIT_CPT, CAP_BENCH_THD, RME_BOOT_INIT_PRC, raddr),
    );
    raddr += 0x1000;

    /* ---- Create the switch target of the init process ---- */
    /* It shares the init process' capability table, so no delegation is
     * needed for it. */
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
    let raddr: u64 = BENCH_KOM_FRONTIER + 0x18000;

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
    let stack = bench_stack_top(unsafe {
        core::ptr::addr_of!(BENCH_STACK_INV[BENCH_STACK_WORD_NUM - 1]) as u64
    });
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
    let raddr2: u64 = BENCH_KOM_FRONTIER + 0x19000;
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
    let stack2 = bench_stack_top(unsafe {
        core::ptr::addr_of!(BENCH_STACK_INV2[BENCH_STACK_WORD_NUM - 1]) as u64
    });
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
    let stack = bench_stack_top(unsafe {
        core::ptr::addr_of!(BENCH_STACK_P1T2[BENCH_STACK_WORD_NUM - 1]) as u64
    });
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
    let stack = bench_stack_top(unsafe {
        core::ptr::addr_of!(BENCH_STACK_P2T1[BENCH_STACK_WORD_NUM - 1]) as u64
    });
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
    let stack = bench_stack_top(unsafe {
        core::ptr::addr_of!(BENCH_STACK_P2T2[BENCH_STACK_WORD_NUM - 1]) as u64
    });
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
    let stack = bench_stack_top(unsafe {
        core::ptr::addr_of!(BENCH_STACK_P1T3[BENCH_STACK_WORD_NUM - 1]) as u64
    });
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
        core::ptr::write_volatile(&raw mut CAP_P1T2, cap_p1t2);
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
Return      : None.
*****************************************************************************/
fn bench_wake_helper(phase: u32) {
    let cap_park = unsafe { core::ptr::read_volatile(&raw const CAP_SIG_PARK) };

    bench_phase_set(phase);
    let _ = rme_sig_snd(cap_park, 1);
}
/* End Function:bench_wake_helper *******************************************/

/* Function:bench_wake_helper2 *********************************************
Description : Publish a phase and wake the receiver thread of the second
              process up. That thread runs two priority levels above the
              init thread, so the kernel switches to it right away.
Input       : phase - The phase the helper thread shall run.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_wake_helper2(phase: u32) {
    let cap_park = unsafe { core::ptr::read_volatile(&raw const CAP_SIG_PARK2) };

    bench_phase_set(phase);
    let _ = rme_sig_snd(cap_park, 1);
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
    bench_stat_reset();

    /* Wake the helper thread - it runs the whole test and parks itself
     * again before this call returns */
    bench_wake_helper(PHASE_SIG_SELF);

    stat.reset();
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
    bench_stat_reset();
    bench_wake_helper(PHASE_SIG_RCV_ONLY);
    stat.reset();
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
    let stack = bench_stack_top(unsafe {
        core::ptr::addr_of!(BENCH_STACK_INV2[BENCH_STACK_WORD_NUM - 1]) as u64
    });
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
    let stack1 = bench_stack_top(unsafe {
        core::ptr::addr_of!(BENCH_STACK_INV[BENCH_STACK_WORD_NUM - 1]) as u64
    });
    let stack2 = bench_stack_top(unsafe {
        core::ptr::addr_of!(BENCH_STACK_INV2[BENCH_STACK_WORD_NUM - 1]) as u64
    });
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
    bench_stat_reset();

    /* Let the helper thread take the receiving role and wait for the
     * workload on the data channel */
    bench_wake_helper(PHASE_SIG_RECV);

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
    bench_stat_reset();

    /* Let the receiver in the second process take the receiving role and
     * wait for the workload on the data channel */
    bench_wake_helper2(PHASE_SIG_RECV2);

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
        /* Raise the ticket, so that the target thread latches the arrival
         * exactly once for this switch */
        bench_ticket_intra_raise();
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
        bench_ticket_intra_raise();
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
        /* Raise the ticket, so that the target thread latches the arrival
         * exactly once for this switch */
        bench_ticket_inter_raise();
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
        bench_ticket_inter_raise();
        let start = bench_read_cycle();
        let ret = rme_thd_swt(cap_p2t2, 0) as i64;
        let end = bench_read_cycle();

        let _ = stat.set_ret(ret);
        stat.record((end - start) as u32);
        count += 1;
    }
}
/* End Function:bench_test_yield_inter_roundtrip ****************************/

/* Function:bench_test_tick_preempt ***********************************************
Description : Measure the cost of the kernel's tick preemption reaching the
              user space. The helper thread of the init process is given a single
              timeslice and burns it in a tight loop, so the next tick
              preempts it and notifies its scheduler - the init thread. The
              measurement is the distance between the helper's departure and
              the moment the notification becomes visible to the user.
              This is the RME counterpart of the tick preemption test of
              rvm_benchmark.h: RME has no user level interrupt handler, so
              the tick preemption notification is the only user visible
              trace of a physical event that reaches the user space.
Input       : stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_test_tick_preempt(stat: &mut BenchStat) {
    let cap_p1t2 = unsafe { core::ptr::read_volatile(&raw const CAP_P1T2) };
    let cap_park = unsafe { core::ptr::read_volatile(&raw const CAP_SIG_PARK) };
    let mut count: u64 = 0;

    stat.reset();

    while count < BENCH_LOOP_REDUCED_NUM && stat.fail == 0 {
        /* Arm the load thread again */
        bench_stop_set(0);
        bench_phase_set(PHASE_TICK_PREEMPT);

        /* Drain the notifications that are still pending */
        while rme_thd_sched_rcv(RME_BOOT_INIT_THD) as i64 >= 0 {
            core::hint::spin_loop();
        }

        /* Revoke the helper thread's timeslice and give it a single one, so
         * that the very next kernel tick preempts it. The helper thread is
         * parked at this point, and a parked thread only becomes runnable
         * again when it is woken up with a signal. */
        if !bench_setup_try(
            stat,
            "THD_TIME_XFER revoke helper time",
            rme_thd_time_xfer(RME_BOOT_INIT_THD, cap_p1t2, u64::MAX),
        ) {
            break;
        }
        if !bench_setup_try(
            stat,
            "THD_TIME_XFER arm helper time",
            rme_thd_time_xfer(cap_p1t2, RME_BOOT_INIT_THD, 1),
        ) {
            /* The helper's budget was revoked, so give it back before the
             * test is left. */
            bench_stop_set(1);
            let _ = rme_thd_time_xfer(cap_p1t2, RME_BOOT_INIT_THD, BENCH_TIME_XFER);
            break;
        }
        /* Wake the helper thread up - it latches the departure timestamp and
         * burns its timeslice until the next tick preempts it */
        if !bench_setup_try(stat, "SIG_SND wake helper", rme_sig_snd(cap_park, 1)) {
            bench_stop_set(1);
            let _ = rme_thd_time_xfer(cap_p1t2, RME_BOOT_INIT_THD, BENCH_TIME_XFER);
            break;
        }
        /* The tick preemption notifies this thread, as the helper's
         * scheduler. Bound the wait so a broken helper cannot hang the
         * whole suite. */
        let wait_start = bench_read_cycle();
        let mut got_notification = false;
        loop {
            let ret = rme_thd_sched_rcv(RME_BOOT_INIT_THD) as i64;
            if ret >= 0 {
                let _ = stat.set_ret(ret);
                got_notification = true;
                break;
            }
            if bench_read_cycle().wrapping_sub(wait_start) > BENCH_WAIT_TIMEOUT_CYCLES {
                break;
            }
            core::hint::spin_loop();
        }
        bench_end_set(bench_read_cycle());

        if !got_notification {
            let _ = stat.set_ret(-1);
        }
        stat.record((bench_end_get() - bench_start_get()) as u32);

        /* Release the helper thread, so that it can leave its loop */
        bench_stop_set(1);
        let _ = rme_thd_time_xfer(cap_p1t2, RME_BOOT_INIT_THD, BENCH_TIME_XFER);

        if !got_notification {
            break;
        }
        count += 1;
    }
}
/* End Function:bench_test_tick_preempt *******************************************/

/* Function:bench_test_cpt_crt **********************************************
Description : Measure CPT_CRT on a fresh 16-entry capability table. The table
              is created inside the timing window and deleted untimed.
Input       : stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_test_cpt_crt(stat: &mut BenchStat) {
    stat.reset();

    let mut count: u64 = 0;
    while count < BENCH_OBJ_LOOP_NUM && stat.fail == 0 {
        let raddr = bench_kom_alloc(16 * 64, 64);

        let start = bench_read_cycle();
        let _ = stat.set_ret(rme_cpt_crt(
            RME_BOOT_INIT_CPT,
            RME_BOOT_INIT_KOM,
            CAP_TEST_CPT,
            raddr,
            16,
        ) as i64);
        let end = bench_read_cycle();

        stat.record((end - start) as u32);

        let _ = rme_cpt_frz(RME_BOOT_INIT_CPT, CAP_TEST_CPT);
        let _ = rme_cpt_del(RME_BOOT_INIT_CPT, CAP_TEST_CPT);
        count += 1;
    }
}
/* End Function:bench_test_cpt_crt *******************************************/

/* Function:bench_test_cpt_del **********************************************
Description : Measure CPT_DEL. The empty table is created and frozen outside
              the timing window.
Input       : stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_test_cpt_del(stat: &mut BenchStat) {
    stat.reset();

    let mut count: u64 = 0;
    while count < BENCH_OBJ_LOOP_NUM && stat.fail == 0 {
        let raddr = bench_kom_alloc(16 * 64, 64);
        if !bench_setup_try(
            stat,
            "CPT_CRT delete target",
            rme_cpt_crt(
                RME_BOOT_INIT_CPT,
                RME_BOOT_INIT_KOM,
                CAP_TEST_CPT,
                raddr,
                16,
            ),
        ) {
            break;
        }
        let _ = rme_cpt_frz(RME_BOOT_INIT_CPT, CAP_TEST_CPT);

        let start = bench_read_cycle();
        let _ = stat.set_ret(rme_cpt_del(RME_BOOT_INIT_CPT, CAP_TEST_CPT) as i64);
        let end = bench_read_cycle();

        stat.record((end - start) as u32);
        count += 1;
    }
}
/* End Function:bench_test_cpt_del *******************************************/

/* Function:bench_test_cpt_frz **********************************************
Description : Measure CPT_FRZ on a fresh signal endpoint. The endpoint is
              created and deleted outside the timing window.
Input       : stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_test_cpt_frz(stat: &mut BenchStat) {
    stat.reset();

    let mut count: u64 = 0;
    while count < BENCH_OBJ_LOOP_NUM && stat.fail == 0 {
        if !bench_setup_try(stat, "SIG_CRT freeze target", bench_sig_crt(CAP_TEST_SIG)) {
            break;
        }

        let start = bench_read_cycle();
        let _ = stat.set_ret(rme_cpt_frz(RME_BOOT_INIT_CPT, CAP_TEST_SIG) as i64);
        let end = bench_read_cycle();

        stat.record((end - start) as u32);

        let _ = rme_sig_del(RME_BOOT_INIT_CPT, CAP_TEST_SIG);
        count += 1;
    }
}
/* End Function:bench_test_cpt_frz *******************************************/

/* Function:bench_test_cpt_add **********************************************
Description : Measure CPT_ADD by delegating a signal capability into an empty
              slot of the init table. The leaf is removed untimed.
Input       : stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_test_cpt_add(stat: &mut BenchStat) {
    if !bench_setup_try(
        stat,
        "SIG_CRT delegate source",
        bench_sig_crt(CAP_TEST_SIG2),
    ) {
        return;
    }

    stat.reset();

    let mut count: u64 = 0;
    while count < BENCH_OBJ_LOOP_NUM && stat.fail == 0 {
        let start = bench_read_cycle();
        let _ = stat.set_ret(rme_cpt_add(
            RME_BOOT_INIT_CPT,
            CAP_TEST_SIG,
            RME_BOOT_INIT_CPT,
            CAP_TEST_SIG2,
            RME_SIG_FLAG_ALL,
        ) as i64);
        let end = bench_read_cycle();

        stat.record((end - start) as u32);

        let _ = rme_cpt_frz(RME_BOOT_INIT_CPT, CAP_TEST_SIG);
        let _ = rme_cpt_rem(RME_BOOT_INIT_CPT, CAP_TEST_SIG);
        count += 1;
    }

    let _ = rme_cpt_frz(RME_BOOT_INIT_CPT, CAP_TEST_SIG2);
    let _ = rme_sig_del(RME_BOOT_INIT_CPT, CAP_TEST_SIG2);
}
/* End Function:bench_test_cpt_add *******************************************/

/* Function:bench_test_cpt_rem **********************************************
Description : Measure CPT_REM. The delegated leaf is created and frozen outside
              the timing window.
Input       : stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_test_cpt_rem(stat: &mut BenchStat) {
    if !bench_setup_try(
        stat,
        "SIG_CRT delegate source",
        bench_sig_crt(CAP_TEST_SIG2),
    ) {
        return;
    }

    stat.reset();

    let mut count: u64 = 0;
    while count < BENCH_OBJ_LOOP_NUM && stat.fail == 0 {
        if !bench_setup_try(
            stat,
            "CPT_ADD delegate leaf",
            rme_cpt_add(
                RME_BOOT_INIT_CPT,
                CAP_TEST_SIG,
                RME_BOOT_INIT_CPT,
                CAP_TEST_SIG2,
                RME_SIG_FLAG_ALL,
            ),
        ) {
            break;
        }
        let _ = rme_cpt_frz(RME_BOOT_INIT_CPT, CAP_TEST_SIG);

        let start = bench_read_cycle();
        let _ = stat.set_ret(rme_cpt_rem(RME_BOOT_INIT_CPT, CAP_TEST_SIG) as i64);
        let end = bench_read_cycle();

        stat.record((end - start) as u32);
        count += 1;
    }

    let _ = rme_cpt_frz(RME_BOOT_INIT_CPT, CAP_TEST_SIG2);
    let _ = rme_sig_del(RME_BOOT_INIT_CPT, CAP_TEST_SIG2);
}
/* End Function:bench_test_cpt_rem *******************************************/

/* Function:bench_test_pgt_crt **********************************************
Description : Measure PGT_CRT on a fresh 4KB page table.
Input       : stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_test_pgt_crt(stat: &mut BenchStat) {
    stat.reset();

    let mut count: u64 = 0;
    while count < BENCH_OBJ_LOOP_NUM && stat.fail == 0 {
        let raddr = bench_kom_alloc(0x1000, 0x1000);

        let start = bench_read_cycle();
        let _ = stat.set_ret(bench_pgt_crt(CAP_TEST_PGT, raddr, 0, 0, RME_PGT_SIZE_4K) as i64);
        let end = bench_read_cycle();

        stat.record((end - start) as u32);

        let _ = rme_cpt_frz(RME_BOOT_INIT_CPT, CAP_TEST_PGT);
        let _ = rme_pgt_del(RME_BOOT_INIT_CPT, CAP_TEST_PGT);
        count += 1;
    }
}
/* End Function:bench_test_pgt_crt *******************************************/

/* Function:bench_test_pgt_del **********************************************
Description : Measure PGT_DEL. The table is created and frozen outside the
              timing window.
Input       : stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_test_pgt_del(stat: &mut BenchStat) {
    stat.reset();

    let mut count: u64 = 0;
    while count < BENCH_OBJ_LOOP_NUM && stat.fail == 0 {
        let raddr = bench_kom_alloc(0x1000, 0x1000);
        if !bench_setup_try(
            stat,
            "PGT_CRT delete target",
            bench_pgt_crt(CAP_TEST_PGT, raddr, 0, 0, RME_PGT_SIZE_4K),
        ) {
            break;
        }
        let _ = rme_cpt_frz(RME_BOOT_INIT_CPT, CAP_TEST_PGT);

        let start = bench_read_cycle();
        let _ = stat.set_ret(rme_pgt_del(RME_BOOT_INIT_CPT, CAP_TEST_PGT) as i64);
        let end = bench_read_cycle();

        stat.record((end - start) as u32);
        count += 1;
    }
}
/* End Function:bench_test_pgt_del *******************************************/

/* Function:bench_test_pgt_add **********************************************
Description : Measure PGT_ADD by mapping init image page 0 into a scratch 4KB
              table. The mapping is removed untimed.
Input       : stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_test_pgt_add(stat: &mut BenchStat) {
    stat.reset();

    let mut count: u64 = 0;
    while count < BENCH_OBJ_LOOP_NUM && stat.fail == 0 {
        /* PGT_REM is broken in the current kernel, so create a fresh table
         * for each round and delete it untimed instead of unmapping. */
        let raddr = bench_kom_alloc(0x1000, 0x1000);
        if !bench_setup_try(
            stat,
            "PGT_CRT map target",
            bench_pgt_crt(CAP_TEST_PGT_DST, raddr, 0, 0, RME_PGT_SIZE_4K),
        ) {
            break;
        }

        let start = bench_read_cycle();
        let _ = stat.set_ret(bench_pgt_add(CAP_TEST_PGT_DST, 0, RME_BOOT_INIT_PGT_L2, 0) as i64);
        let end = bench_read_cycle();

        stat.record((end - start) as u32);

        let _ = rme_cpt_frz(RME_BOOT_INIT_CPT, CAP_TEST_PGT_DST);
        let _ = rme_pgt_del(RME_BOOT_INIT_CPT, CAP_TEST_PGT_DST);
        count += 1;
    }
}
/* End Function:bench_test_pgt_add *******************************************/

/* Function:bench_test_pgt_rem **********************************************
Description : Measure PGT_REM. The mapping is created outside the timing
              window.
Input       : stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_test_pgt_rem(stat: &mut BenchStat) {
    let raddr = bench_kom_alloc(0x1000, 0x1000);
    if !bench_setup_try(
        stat,
        "PGT_CRT unmap target",
        bench_pgt_crt(CAP_TEST_PGT_DST, raddr, 0, 0, RME_PGT_SIZE_4K),
    ) {
        return;
    }

    stat.reset();

    let mut count: u64 = 0;
    while count < BENCH_OBJ_LOOP_NUM && stat.fail == 0 {
        if !bench_setup_try(
            stat,
            "PGT_ADD mapped page",
            bench_pgt_add(CAP_TEST_PGT_DST, 0, RME_BOOT_INIT_PGT_L2, 0),
        ) {
            break;
        }

        let start = bench_read_cycle();
        let _ = stat.set_ret(rme_pgt_rem(CAP_TEST_PGT_DST, 0) as i64);
        let end = bench_read_cycle();

        stat.record((end - start) as u32);
        count += 1;
    }

    let _ = rme_cpt_frz(RME_BOOT_INIT_CPT, CAP_TEST_PGT_DST);
    let _ = rme_pgt_del(RME_BOOT_INIT_CPT, CAP_TEST_PGT_DST);
}
/* End Function:bench_test_pgt_rem *******************************************/

/* Function:bench_test_pgt_con **********************************************
Description : Measure PGT_CON by connecting a 4KB child to a 2MB parent. The
              connection is destroyed untimed.
Input       : stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_test_pgt_con(stat: &mut BenchStat) {
    let parent_raddr = bench_kom_alloc(0x1000, 0x1000);
    if !bench_setup_try(
        stat,
        "PGT_CRT connect parent",
        bench_pgt_crt(CAP_TEST_PGT_CON_PARENT, parent_raddr, 0, 0, RME_PGT_SIZE_2M),
    ) {
        return;
    }

    /* PGT_DES is broken in the current kernel, so each child can only be
     * connected once. Create one child per parent slot and connect each. */
    let mut i: u64 = 0;
    while i < BENCH_PGT_CON_ROUNDS {
        let child_raddr = bench_kom_alloc(0x1000, 0x1000);
        if !bench_setup_try(
            stat,
            "PGT_CRT connect child",
            bench_pgt_crt(
                CAP_TEST_PGT_CHILD_BASE + i,
                child_raddr,
                i << RME_PGT_SIZE_2M,
                0,
                RME_PGT_SIZE_4K,
            ),
        ) {
            return;
        }
        i += 1;
    }

    stat.reset();

    i = 0;
    while i < BENCH_PGT_CON_ROUNDS && stat.fail == 0 {
        let start = bench_read_cycle();
        let _ = stat
            .set_ret(bench_pgt_con(CAP_TEST_PGT_CON_PARENT, i, CAP_TEST_PGT_CHILD_BASE + i) as i64);
        let end = bench_read_cycle();

        stat.record((end - start) as u32);
        i += 1;
    }
}
/* End Function:bench_test_pgt_con *******************************************/

/* Function:bench_test_pgt_des **********************************************
Description : Measure PGT_DES. The connection is created outside the timing
              window.
Input       : stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_test_pgt_des(stat: &mut BenchStat) {
    let parent_raddr = bench_kom_alloc(0x1000, 0x1000);
    let child_raddr = bench_kom_alloc(0x1000, 0x1000);
    if !bench_setup_try(
        stat,
        "PGT_CRT disconnect parent",
        bench_pgt_crt(CAP_TEST_PGT_PARENT, parent_raddr, 0, 0, RME_PGT_SIZE_2M),
    ) {
        return;
    }
    if !bench_setup_try(
        stat,
        "PGT_CRT disconnect child",
        bench_pgt_crt(CAP_TEST_PGT_CHILD, child_raddr, 0, 0, RME_PGT_SIZE_4K),
    ) {
        return;
    }
    stat.reset();

    let mut count: u64 = 0;
    while count < BENCH_OBJ_LOOP_NUM && stat.fail == 0 {
        if !bench_setup_try(
            stat,
            "PGT_CON connected child",
            bench_pgt_con(CAP_TEST_PGT_PARENT, 0, CAP_TEST_PGT_CHILD),
        ) {
            break;
        }

        let start = bench_read_cycle();
        let _ = stat.set_ret(rme_pgt_des(CAP_TEST_PGT_PARENT, 0, CAP_TEST_PGT_CHILD) as i64);
        let end = bench_read_cycle();

        stat.record((end - start) as u32);
        count += 1;
    }

    let _ = rme_cpt_frz(RME_BOOT_INIT_CPT, CAP_TEST_PGT_CHILD);
    let _ = rme_pgt_del(RME_BOOT_INIT_CPT, CAP_TEST_PGT_CHILD);
    let _ = rme_cpt_frz(RME_BOOT_INIT_CPT, CAP_TEST_PGT_PARENT);
    let _ = rme_pgt_del(RME_BOOT_INIT_CPT, CAP_TEST_PGT_PARENT);
}
/* End Function:bench_test_pgt_des *******************************************/

/* Function:bench_test_prc_crt **********************************************
Description : Measure PRC_CRT with a pre-created capability table and root page
              table. The process is deleted untimed.
Input       : stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_test_prc_crt(stat: &mut BenchStat) {
    let cpt_raddr = bench_kom_alloc(16 * 64, 64);
    let pgt_raddr = bench_kom_alloc(0x1000, 0x1000);
    if !bench_setup_try(
        stat,
        "CPT_CRT process captbl",
        rme_cpt_crt(
            RME_BOOT_INIT_CPT,
            RME_BOOT_INIT_KOM,
            CAP_TEST_CPT,
            cpt_raddr,
            16,
        ),
    ) {
        return;
    }
    if !bench_setup_try(
        stat,
        "PGT_CRT process root",
        bench_pgt_crt(CAP_TEST_PGT, pgt_raddr, 0, 1, RME_PGT_SIZE_1G),
    ) {
        return;
    }
    stat.reset();

    let mut count: u64 = 0;
    while count < BENCH_OBJ_LOOP_NUM && stat.fail == 0 {
        let start = bench_read_cycle();
        let _ =
            stat.set_ret(
                rme_prc_crt(RME_BOOT_INIT_CPT, CAP_TEST_PRC, CAP_TEST_CPT, CAP_TEST_PGT) as i64,
            );
        let end = bench_read_cycle();

        stat.record((end - start) as u32);

        let _ = rme_cpt_frz(RME_BOOT_INIT_CPT, CAP_TEST_PRC);
        let _ = rme_prc_del(RME_BOOT_INIT_CPT, CAP_TEST_PRC);
        count += 1;
    }

    let _ = rme_cpt_frz(RME_BOOT_INIT_CPT, CAP_TEST_CPT);
    let _ = rme_cpt_del(RME_BOOT_INIT_CPT, CAP_TEST_CPT);
    let _ = rme_cpt_frz(RME_BOOT_INIT_CPT, CAP_TEST_PGT);
    let _ = rme_pgt_del(RME_BOOT_INIT_CPT, CAP_TEST_PGT);
}
/* End Function:bench_test_prc_crt *******************************************/

/* Function:bench_test_prc_del **********************************************
Description : Measure PRC_DEL. The process is created and frozen outside the
              timing window.
Input       : stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_test_prc_del(stat: &mut BenchStat) {
    let cpt_raddr = bench_kom_alloc(16 * 64, 64);
    let pgt_raddr = bench_kom_alloc(0x1000, 0x1000);
    if !bench_setup_try(
        stat,
        "CPT_CRT process captbl",
        rme_cpt_crt(
            RME_BOOT_INIT_CPT,
            RME_BOOT_INIT_KOM,
            CAP_TEST_CPT,
            cpt_raddr,
            16,
        ),
    ) {
        return;
    }
    if !bench_setup_try(
        stat,
        "PGT_CRT process root",
        bench_pgt_crt(CAP_TEST_PGT, pgt_raddr, 0, 1, RME_PGT_SIZE_1G),
    ) {
        return;
    }
    stat.reset();

    let mut count: u64 = 0;
    while count < BENCH_OBJ_LOOP_NUM && stat.fail == 0 {
        if !bench_setup_try(
            stat,
            "PRC_CRT process",
            rme_prc_crt(RME_BOOT_INIT_CPT, CAP_TEST_PRC, CAP_TEST_CPT, CAP_TEST_PGT),
        ) {
            break;
        }
        let _ = rme_cpt_frz(RME_BOOT_INIT_CPT, CAP_TEST_PRC);

        let start = bench_read_cycle();
        let _ = stat.set_ret(rme_prc_del(RME_BOOT_INIT_CPT, CAP_TEST_PRC) as i64);
        let end = bench_read_cycle();

        stat.record((end - start) as u32);
        count += 1;
    }

    let _ = rme_cpt_frz(RME_BOOT_INIT_CPT, CAP_TEST_CPT);
    let _ = rme_cpt_del(RME_BOOT_INIT_CPT, CAP_TEST_CPT);
    let _ = rme_cpt_frz(RME_BOOT_INIT_CPT, CAP_TEST_PGT);
    let _ = rme_pgt_del(RME_BOOT_INIT_CPT, CAP_TEST_PGT);
}
/* End Function:bench_test_prc_del *******************************************/

/* Function:bench_test_prc_cpt **********************************************
Description : Measure PRC_CPT by alternating a dummy process between two
              capability tables. The reverse switch is untimed.
Input       : stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_test_prc_cpt(stat: &mut BenchStat) {
    let cpt_a_raddr = bench_kom_alloc(16 * 64, 64);
    let cpt_b_raddr = bench_kom_alloc(16 * 64, 64);
    let pgt_raddr = bench_kom_alloc(0x1000, 0x1000);
    if !bench_setup_try(
        stat,
        "CPT_CRT switch target A",
        rme_cpt_crt(
            RME_BOOT_INIT_CPT,
            RME_BOOT_INIT_KOM,
            CAP_TEST_CPT,
            cpt_a_raddr,
            16,
        ),
    ) {
        return;
    }
    if !bench_setup_try(
        stat,
        "CPT_CRT switch target B",
        rme_cpt_crt(
            RME_BOOT_INIT_CPT,
            RME_BOOT_INIT_KOM,
            CAP_TEST_CPT2,
            cpt_b_raddr,
            16,
        ),
    ) {
        return;
    }
    if !bench_setup_try(
        stat,
        "PGT_CRT process root",
        bench_pgt_crt(CAP_TEST_PGT, pgt_raddr, 0, 1, RME_PGT_SIZE_1G),
    ) {
        return;
    }
    if !bench_setup_try(
        stat,
        "PRC_CRT switch process",
        rme_prc_crt(RME_BOOT_INIT_CPT, CAP_TEST_PRC, CAP_TEST_CPT, CAP_TEST_PGT),
    ) {
        return;
    }
    stat.reset();

    let mut count: u64 = 0;
    while count < BENCH_OBJ_LOOP_NUM && stat.fail == 0 {
        let start = bench_read_cycle();
        let _ = stat.set_ret(rme_prc_cpt(CAP_TEST_PRC, CAP_TEST_CPT2) as i64);
        let end = bench_read_cycle();

        stat.record((end - start) as u32);

        if !bench_setup_try(
            stat,
            "PRC_CPT restore captbl",
            rme_prc_cpt(CAP_TEST_PRC, CAP_TEST_CPT),
        ) {
            break;
        }
        count += 1;
    }

    let _ = rme_cpt_frz(RME_BOOT_INIT_CPT, CAP_TEST_PRC);
    let _ = rme_prc_del(RME_BOOT_INIT_CPT, CAP_TEST_PRC);
    let _ = rme_cpt_frz(RME_BOOT_INIT_CPT, CAP_TEST_CPT);
    let _ = rme_cpt_del(RME_BOOT_INIT_CPT, CAP_TEST_CPT);
    let _ = rme_cpt_frz(RME_BOOT_INIT_CPT, CAP_TEST_CPT2);
    let _ = rme_cpt_del(RME_BOOT_INIT_CPT, CAP_TEST_CPT2);
    let _ = rme_cpt_frz(RME_BOOT_INIT_CPT, CAP_TEST_PGT);
    let _ = rme_pgt_del(RME_BOOT_INIT_CPT, CAP_TEST_PGT);
}
/* End Function:bench_test_prc_cpt *******************************************/

/* Function:bench_test_prc_pgt **********************************************
Description : Measure PRC_PGT by alternating a dummy process between two root
              page tables. The reverse switch is untimed.
Input       : stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_test_prc_pgt(stat: &mut BenchStat) {
    let cpt_raddr = bench_kom_alloc(16 * 64, 64);
    let pgt_a_raddr = bench_kom_alloc(0x1000, 0x1000);
    let pgt_b_raddr = bench_kom_alloc(0x1000, 0x1000);
    if !bench_setup_try(
        stat,
        "CPT_CRT switch process captbl",
        rme_cpt_crt(
            RME_BOOT_INIT_CPT,
            RME_BOOT_INIT_KOM,
            CAP_TEST_CPT,
            cpt_raddr,
            16,
        ),
    ) {
        return;
    }
    if !bench_setup_try(
        stat,
        "PGT_CRT switch root A",
        bench_pgt_crt(CAP_TEST_PGT, pgt_a_raddr, 0, 1, RME_PGT_SIZE_1G),
    ) {
        return;
    }
    if !bench_setup_try(
        stat,
        "PGT_CRT switch root B",
        bench_pgt_crt(CAP_TEST_PGT2, pgt_b_raddr, 0, 1, RME_PGT_SIZE_1G),
    ) {
        return;
    }
    if !bench_setup_try(
        stat,
        "PRC_CRT switch process",
        rme_prc_crt(RME_BOOT_INIT_CPT, CAP_TEST_PRC, CAP_TEST_CPT, CAP_TEST_PGT),
    ) {
        return;
    }
    stat.reset();

    let mut count: u64 = 0;
    while count < BENCH_OBJ_LOOP_NUM && stat.fail == 0 {
        let start = bench_read_cycle();
        let _ = stat.set_ret(rme_prc_pgt(CAP_TEST_PRC, CAP_TEST_PGT2) as i64);
        let end = bench_read_cycle();

        stat.record((end - start) as u32);

        if !bench_setup_try(
            stat,
            "PRC_PGT restore root",
            rme_prc_pgt(CAP_TEST_PRC, CAP_TEST_PGT),
        ) {
            break;
        }
        count += 1;
    }

    let _ = rme_cpt_frz(RME_BOOT_INIT_CPT, CAP_TEST_PRC);
    let _ = rme_prc_del(RME_BOOT_INIT_CPT, CAP_TEST_PRC);
    let _ = rme_cpt_frz(RME_BOOT_INIT_CPT, CAP_TEST_CPT);
    let _ = rme_cpt_del(RME_BOOT_INIT_CPT, CAP_TEST_CPT);
    let _ = rme_cpt_frz(RME_BOOT_INIT_CPT, CAP_TEST_PGT);
    let _ = rme_pgt_del(RME_BOOT_INIT_CPT, CAP_TEST_PGT);
    let _ = rme_cpt_frz(RME_BOOT_INIT_CPT, CAP_TEST_PGT2);
    let _ = rme_pgt_del(RME_BOOT_INIT_CPT, CAP_TEST_PGT2);
}
/* End Function:bench_test_prc_pgt *******************************************/

/* Function:bench_test_thd_crt **********************************************
Description : Measure THD_CRT on a fresh thread object.
Input       : stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_test_thd_crt(stat: &mut BenchStat) {
    stat.reset();

    let mut count: u64 = 0;
    while count < BENCH_OBJ_LOOP_NUM && stat.fail == 0 {
        let raddr = bench_kom_alloc(0x2000, 0x1000);

        let start = bench_read_cycle();
        let _ =
            stat.set_ret(
                bench_thd_crt(RME_BOOT_INIT_CPT, CAP_TEST_THD, RME_BOOT_INIT_PRC, raddr) as i64,
            );
        let end = bench_read_cycle();

        stat.record((end - start) as u32);

        let _ = rme_cpt_frz(RME_BOOT_INIT_CPT, CAP_TEST_THD);
        let _ = rme_thd_del(RME_BOOT_INIT_CPT, CAP_TEST_THD);
        count += 1;
    }
}
/* End Function:bench_test_thd_crt *******************************************/

/* Function:bench_test_thd_del **********************************************
Description : Measure THD_DEL. The thread is created and frozen outside the
              timing window.
Input       : stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_test_thd_del(stat: &mut BenchStat) {
    stat.reset();

    let mut count: u64 = 0;
    while count < BENCH_OBJ_LOOP_NUM && stat.fail == 0 {
        let raddr = bench_kom_alloc(0x2000, 0x1000);
        if !bench_setup_try(
            stat,
            "THD_CRT delete target",
            bench_thd_crt(RME_BOOT_INIT_CPT, CAP_TEST_THD, RME_BOOT_INIT_PRC, raddr),
        ) {
            break;
        }
        let _ = rme_cpt_frz(RME_BOOT_INIT_CPT, CAP_TEST_THD);

        let start = bench_read_cycle();
        let _ = stat.set_ret(rme_thd_del(RME_BOOT_INIT_CPT, CAP_TEST_THD) as i64);
        let end = bench_read_cycle();

        stat.record((end - start) as u32);
        count += 1;
    }
}
/* End Function:bench_test_thd_del *******************************************/

/* Function:bench_test_thd_bind **********************************************
Description : Measure THD_SCHED_BIND. The thread is created and freed outside
              the timing window.
Input       : stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_test_thd_bind(stat: &mut BenchStat) {
    stat.reset();

    let mut count: u64 = 0;
    while count < BENCH_OBJ_LOOP_NUM && stat.fail == 0 {
        let raddr = bench_kom_alloc(0x2000, 0x1000);
        if !bench_setup_try(
            stat,
            "THD_CRT bind target",
            bench_thd_crt(RME_BOOT_INIT_CPT, CAP_TEST_THD, RME_BOOT_INIT_PRC, raddr),
        ) {
            break;
        }
        let cap_thd = rme_cid_2l(RME_BOOT_INIT_CPT, CAP_TEST_THD);

        let start = bench_read_cycle();
        let _ = stat.set_ret(bench_thd_bind(cap_thd, 0) as i64);
        let end = bench_read_cycle();

        stat.record((end - start) as u32);

        let _ = rme_thd_sched_free(cap_thd);
        let _ = rme_cpt_frz(RME_BOOT_INIT_CPT, CAP_TEST_THD);
        let _ = rme_thd_del(RME_BOOT_INIT_CPT, CAP_TEST_THD);
        count += 1;
    }
}
/* End Function:bench_test_thd_bind ******************************************/

/* Function:bench_test_thd_free **********************************************
Description : Measure THD_SCHED_FREE. The thread is created and bound outside
              the timing window.
Input       : stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_test_thd_free(stat: &mut BenchStat) {
    stat.reset();

    let mut count: u64 = 0;
    while count < BENCH_OBJ_LOOP_NUM && stat.fail == 0 {
        let raddr = bench_kom_alloc(0x2000, 0x1000);
        if !bench_setup_try(
            stat,
            "THD_CRT free target",
            bench_thd_crt(RME_BOOT_INIT_CPT, CAP_TEST_THD, RME_BOOT_INIT_PRC, raddr),
        ) {
            break;
        }
        let cap_thd = rme_cid_2l(RME_BOOT_INIT_CPT, CAP_TEST_THD);
        if !bench_setup_try(
            stat,
            "THD_SCHED_BIND free target",
            bench_thd_bind(cap_thd, 0),
        ) {
            break;
        }

        let start = bench_read_cycle();
        let _ = stat.set_ret(rme_thd_sched_free(cap_thd) as i64);
        let end = bench_read_cycle();

        stat.record((end - start) as u32);

        let _ = rme_cpt_frz(RME_BOOT_INIT_CPT, CAP_TEST_THD);
        let _ = rme_thd_del(RME_BOOT_INIT_CPT, CAP_TEST_THD);
        count += 1;
    }
}
/* End Function:bench_test_thd_free ******************************************/

/* Function:bench_test_thd_exec_set ******************************************
Description : Measure THD_EXEC_SET on a bound thread.
Input       : stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_test_thd_exec_set(stat: &mut BenchStat) {
    let entry = bench_noop_entry as extern "C" fn(u64) -> ! as u64;
    let stack = bench_stack_top(unsafe {
        core::ptr::addr_of!(BENCH_STACK_TEST[BENCH_STACK_WORD_NUM - 1]) as u64
    });
    stat.reset();

    let mut count: u64 = 0;
    while count < BENCH_OBJ_LOOP_NUM && stat.fail == 0 {
        let raddr = bench_kom_alloc(0x2000, 0x1000);
        if !bench_setup_try(
            stat,
            "THD_CRT exec_set target",
            bench_thd_crt(RME_BOOT_INIT_CPT, CAP_TEST_THD, RME_BOOT_INIT_PRC, raddr),
        ) {
            break;
        }
        let cap_thd = rme_cid_2l(RME_BOOT_INIT_CPT, CAP_TEST_THD);
        if !bench_setup_try(
            stat,
            "THD_SCHED_BIND exec_set target",
            bench_thd_bind(cap_thd, 0),
        ) {
            break;
        }

        let start = bench_read_cycle();
        let _ = stat.set_ret(rme_thd_exec_set(cap_thd, entry, stack, 0) as i64);
        let end = bench_read_cycle();

        stat.record((end - start) as u32);

        let _ = rme_thd_sched_free(cap_thd);
        let _ = rme_cpt_frz(RME_BOOT_INIT_CPT, CAP_TEST_THD);
        let _ = rme_thd_del(RME_BOOT_INIT_CPT, CAP_TEST_THD);
        count += 1;
    }
}
/* End Function:bench_test_thd_exec_set **************************************/

/* Function:bench_test_thd_prio **********************************************
Description : Measure THD_SCHED_PRIO by toggling a bound dummy thread between
              priorities 0 and 1.
Input       : stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_test_thd_prio(stat: &mut BenchStat) {
    stat.reset();

    let mut count: u64 = 0;
    while count < BENCH_OBJ_LOOP_NUM && stat.fail == 0 {
        let raddr = bench_kom_alloc(0x2000, 0x1000);
        if !bench_setup_try(
            stat,
            "THD_CRT prio target",
            bench_thd_crt(RME_BOOT_INIT_CPT, CAP_TEST_THD, RME_BOOT_INIT_PRC, raddr),
        ) {
            break;
        }
        let cap_thd = rme_cid_2l(RME_BOOT_INIT_CPT, CAP_TEST_THD);
        if !bench_setup_try(
            stat,
            "THD_SCHED_BIND prio target",
            bench_thd_bind(cap_thd, 0),
        ) {
            break;
        }

        let start = bench_read_cycle();
        let _ = stat.set_ret(rme_thd_sched_prio(cap_thd, 1) as i64);
        let end = bench_read_cycle();

        stat.record((end - start) as u32);

        let _ = rme_thd_sched_prio(cap_thd, 0);
        let _ = rme_thd_sched_free(cap_thd);
        let _ = rme_cpt_frz(RME_BOOT_INIT_CPT, CAP_TEST_THD);
        let _ = rme_thd_del(RME_BOOT_INIT_CPT, CAP_TEST_THD);
        count += 1;
    }
}
/* End Function:bench_test_thd_prio ******************************************/

/* Function:bench_test_thd_time_xfer ****************************************
Description : Measure THD_TIME_XFER by donating one timeslice from the init
              thread to a bound dummy thread.
Input       : stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_test_thd_time_xfer(stat: &mut BenchStat) {
    stat.reset();

    let mut count: u64 = 0;
    while count < BENCH_OBJ_LOOP_NUM && stat.fail == 0 {
        let raddr = bench_kom_alloc(0x2000, 0x1000);
        if !bench_setup_try(
            stat,
            "THD_CRT time_xfer target",
            bench_thd_crt(RME_BOOT_INIT_CPT, CAP_TEST_THD, RME_BOOT_INIT_PRC, raddr),
        ) {
            break;
        }
        let cap_thd = rme_cid_2l(RME_BOOT_INIT_CPT, CAP_TEST_THD);
        if !bench_setup_try(
            stat,
            "THD_SCHED_BIND time_xfer target",
            bench_thd_bind(cap_thd, 0),
        ) {
            break;
        }

        let start = bench_read_cycle();
        let _ = stat.set_ret(rme_thd_time_xfer(cap_thd, RME_BOOT_INIT_THD, 1) as i64);
        let end = bench_read_cycle();

        stat.record((end - start) as u32);

        let _ = rme_thd_sched_free(cap_thd);
        let _ = rme_cpt_frz(RME_BOOT_INIT_CPT, CAP_TEST_THD);
        let _ = rme_thd_del(RME_BOOT_INIT_CPT, CAP_TEST_THD);
        count += 1;
    }
}
/* End Function:bench_test_thd_time_xfer *************************************/

/* Function:bench_test_thd_sched_rcv ****************************************
Description : Measure THD_SCHED_RCV. Each round first forces the dummy thread
              into TIMEOUT so a notification is queued, then times the receive,
              then re-arms the dummy thread.
Input       : stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_test_thd_sched_rcv(stat: &mut BenchStat) {
    let raddr = bench_kom_alloc(0x2000, 0x1000);
    if !bench_setup_try(
        stat,
        "THD_CRT sched_rcv dummy",
        bench_thd_crt(RME_BOOT_INIT_CPT, CAP_TEST_THD, RME_BOOT_INIT_PRC, raddr),
    ) {
        return;
    }
    let cap_thd = rme_cid_2l(RME_BOOT_INIT_CPT, CAP_TEST_THD);
    if !bench_setup_try(
        stat,
        "THD_SCHED_BIND sched_rcv dummy",
        bench_thd_bind(cap_thd, 0),
    ) {
        return;
    }
    if !bench_setup_try(
        stat,
        "THD_TIME_XFER sched_rcv arm",
        rme_thd_time_xfer(cap_thd, RME_BOOT_INIT_THD, 1),
    ) {
        return;
    }

    stat.reset();

    /* Drop any stale notification from an earlier helper test. */
    while rme_thd_sched_rcv(RME_BOOT_INIT_THD) as i64 >= 0 {
        core::hint::spin_loop();
    }

    let mut count: u64 = 0;
    while count < BENCH_OBJ_LOOP_NUM && stat.fail == 0 {
        /* Make the dummy thread time out and notify the init thread. */
        let drain = rme_thd_time_xfer(RME_BOOT_INIT_THD, cap_thd, u64::MAX);
        if (drain as i64) < 0 {
            let _ = stat.set_ret(drain as i64);
            break;
        }

        let start = bench_read_cycle();
        let _ = stat.set_ret(rme_thd_sched_rcv(RME_BOOT_INIT_THD) as i64);
        let end = bench_read_cycle();

        stat.record((end - start) as u32);

        /* Re-arm the dummy thread for the next round. */
        let arm = rme_thd_time_xfer(cap_thd, RME_BOOT_INIT_THD, 1);
        if (arm as i64) < 0 {
            let _ = stat.set_ret(arm as i64);
            break;
        }
        count += 1;
    }

    let _ = rme_thd_sched_free(cap_thd);
    let _ = rme_cpt_frz(RME_BOOT_INIT_CPT, CAP_TEST_THD);
    let _ = rme_thd_del(RME_BOOT_INIT_CPT, CAP_TEST_THD);
}
/* End Function:bench_test_thd_sched_rcv *************************************/

/* Function:bench_test_sig_crt **********************************************
Description : Measure SIG_CRT.
Input       : stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_test_sig_crt(stat: &mut BenchStat) {
    stat.reset();

    let mut count: u64 = 0;
    while count < BENCH_OBJ_LOOP_NUM && stat.fail == 0 {
        let start = bench_read_cycle();
        let _ = stat.set_ret(bench_sig_crt(CAP_TEST_SIG) as i64);
        let end = bench_read_cycle();

        stat.record((end - start) as u32);

        let _ = rme_cpt_frz(RME_BOOT_INIT_CPT, CAP_TEST_SIG);
        let _ = rme_sig_del(RME_BOOT_INIT_CPT, CAP_TEST_SIG);
        count += 1;
    }
}
/* End Function:bench_test_sig_crt *******************************************/

/* Function:bench_test_sig_del **********************************************
Description : Measure SIG_DEL. The endpoint is created and frozen outside the
              timing window.
Input       : stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_test_sig_del(stat: &mut BenchStat) {
    stat.reset();

    let mut count: u64 = 0;
    while count < BENCH_OBJ_LOOP_NUM && stat.fail == 0 {
        if !bench_setup_try(stat, "SIG_CRT delete target", bench_sig_crt(CAP_TEST_SIG)) {
            break;
        }
        let _ = rme_cpt_frz(RME_BOOT_INIT_CPT, CAP_TEST_SIG);

        let start = bench_read_cycle();
        let _ = stat.set_ret(rme_sig_del(RME_BOOT_INIT_CPT, CAP_TEST_SIG) as i64);
        let end = bench_read_cycle();

        stat.record((end - start) as u32);
        count += 1;
    }
}
/* End Function:bench_test_sig_del *******************************************/

/* Function:bench_test_inv_crt **********************************************
Description : Measure INV_CRT targeting the existing second process.
Input       : stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_test_inv_crt(stat: &mut BenchStat) {
    stat.reset();

    let mut count: u64 = 0;
    while count < BENCH_OBJ_LOOP_NUM && stat.fail == 0 {
        let raddr = bench_kom_alloc(0x2000, 0x1000);

        let start = bench_read_cycle();
        let _ = stat.set_ret(rme_inv_crt(
            RME_BOOT_INIT_CPT,
            RME_BOOT_INIT_KOM,
            CAP_TEST_INV,
            CAP_BENCH_PRC,
            raddr,
        ) as i64);
        let end = bench_read_cycle();

        stat.record((end - start) as u32);

        let _ = rme_cpt_frz(RME_BOOT_INIT_CPT, CAP_TEST_INV);
        let _ = rme_inv_del(RME_BOOT_INIT_CPT, CAP_TEST_INV);
        count += 1;
    }
}
/* End Function:bench_test_inv_crt *******************************************/

/* Function:bench_test_inv_del **********************************************
Description : Measure INV_DEL. The stub is created and frozen outside the
              timing window.
Input       : stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_test_inv_del(stat: &mut BenchStat) {
    stat.reset();

    let mut count: u64 = 0;
    while count < BENCH_OBJ_LOOP_NUM && stat.fail == 0 {
        let raddr = bench_kom_alloc(0x2000, 0x1000);
        let _ = rme_inv_crt(
            RME_BOOT_INIT_CPT,
            RME_BOOT_INIT_KOM,
            CAP_TEST_INV,
            CAP_BENCH_PRC,
            raddr,
        );
        let _ = rme_cpt_frz(RME_BOOT_INIT_CPT, CAP_TEST_INV);

        let start = bench_read_cycle();
        let _ = stat.set_ret(rme_inv_del(RME_BOOT_INIT_CPT, CAP_TEST_INV) as i64);
        let end = bench_read_cycle();

        stat.record((end - start) as u32);
        count += 1;
    }
}
/* End Function:bench_test_inv_del *******************************************/

/* Function:bench_test_inv_set **********************************************
Description : Measure INV_SET on a fresh stub.
Input       : stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_test_inv_set(stat: &mut BenchStat) {
    let entry = bench_inv_entry as extern "C" fn(u64) -> ! as u64;
    let stack = bench_stack_top(unsafe {
        core::ptr::addr_of!(BENCH_STACK_INV[BENCH_STACK_WORD_NUM - 1]) as u64
    });
    stat.reset();

    let mut count: u64 = 0;
    while count < BENCH_OBJ_LOOP_NUM && stat.fail == 0 {
        let raddr = bench_kom_alloc(0x2000, 0x1000);
        let _ = rme_inv_crt(
            RME_BOOT_INIT_CPT,
            RME_BOOT_INIT_KOM,
            CAP_TEST_INV,
            CAP_BENCH_PRC,
            raddr,
        );
        let cap_inv = rme_cid_2l(RME_BOOT_INIT_CPT, CAP_TEST_INV);

        let start = bench_read_cycle();
        let _ = stat.set_ret(rme_inv_set(cap_inv, entry, stack, 0) as i64);
        let end = bench_read_cycle();

        stat.record((end - start) as u32);

        let _ = rme_cpt_frz(RME_BOOT_INIT_CPT, CAP_TEST_INV);
        let _ = rme_inv_del(RME_BOOT_INIT_CPT, CAP_TEST_INV);
        count += 1;
    }
}
/* End Function:bench_test_inv_set *******************************************/

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
    let stack = bench_stack_top(unsafe {
        core::ptr::addr_of!(BENCH_STACK_INV[BENCH_STACK_WORD_NUM - 1]) as u64
    });
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
              the next timer tick, so the result includes the sleep latency.
Input       : stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_test_kfn_idle_sleep(stat: &mut BenchStat) {
    stat.reset();

    let mut count: u64 = 0;
    while count < BENCH_LOOP_REDUCED_NUM && stat.fail == 0 {
        let start = bench_read_cycle();
        let _ = stat.set_ret(rme_kfn(RME_BOOT_INIT_KFN, RME_KFN_IDLE_SLEEP, 0, 0, 0) as i64);
        let end = bench_read_cycle();

        stat.record((end - start) as u32);
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

    /* ---- THD lifecycle/state ---- */
    bench_test_thd_crt(&mut stat);
    bench_report("THD_CRT", &stat);
    bench_test_thd_del(&mut stat);
    bench_report("THD_DEL", &stat);
    bench_test_thd_bind(&mut stat);
    bench_report("THD_SCHED_BIND", &stat);
    bench_test_thd_free(&mut stat);
    bench_report("THD_SCHED_FREE", &stat);
    bench_test_thd_exec_set(&mut stat);
    bench_report("THD_EXEC_SET", &stat);
    bench_test_thd_prio(&mut stat);
    bench_report("THD_SCHED_PRIO", &stat);
    bench_test_thd_time_xfer(&mut stat);
    bench_report("THD_TIME_XFER", &stat);
    bench_test_thd_sched_rcv(&mut stat);
    bench_report("THD_SCHED_RCV", &stat);

    /* ---- Signal: operations and object lifecycle ---- */
    bench_test_sig_crt(&mut stat);
    bench_report("SIG_CRT", &stat);

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

    bench_test_sig_del(&mut stat);
    bench_report("SIG_DEL", &stat);

    /* The tick preemption test reuses the process-1 helper after the signal
     * tests have parked it again. */
    bench_test_tick_preempt(&mut stat);
    bench_report("THD_TIME_XFER+THD_SCHED_RCV(tick preempt)", &stat);

    /* ---- Invocation: operations and object lifecycle ---- */
    bench_test_inv_crt(&mut stat);
    bench_report("INV_CRT", &stat);
    bench_test_inv_set(&mut stat);
    bench_report("INV_SET", &stat);

    bench_test_inv_act(&mut stat);
    bench_report("INV_ACT(one-way entry)", &stat);
    bench_test_invocation(&mut stat);
    bench_report("INV_ACT+INV_RET(round-trip)", &stat);
    bench_test_inv_ret(&mut stat);
    bench_report("INV_RET(nested return)", &stat);

    bench_test_inv_del(&mut stat);
    bench_report("INV_DEL", &stat);

    /* ---- Capability table lifecycle ---- */
    bench_test_cpt_crt(&mut stat);
    bench_report("CPT_CRT", &stat);
    bench_test_cpt_del(&mut stat);
    bench_report("CPT_DEL", &stat);
    bench_test_cpt_frz(&mut stat);
    bench_report("CPT_FRZ", &stat);
    bench_test_cpt_add(&mut stat);
    bench_report("CPT_ADD", &stat);
    bench_test_cpt_rem(&mut stat);
    bench_report("CPT_REM", &stat);

    /* ---- Page table lifecycle ---- */
    bench_test_pgt_crt(&mut stat);
    bench_report("PGT_CRT", &stat);
    bench_test_pgt_del(&mut stat);
    bench_report("PGT_DEL", &stat);
    bench_test_pgt_add(&mut stat);
    bench_report("PGT_ADD", &stat);
    bench_test_pgt_rem(&mut stat);
    bench_report("PGT_REM", &stat);
    bench_test_pgt_con(&mut stat);
    bench_report("PGT_CON", &stat);
    bench_test_pgt_des(&mut stat);
    bench_report("PGT_DES", &stat);

    /* ---- Process lifecycle ---- */
    bench_test_prc_crt(&mut stat);
    bench_report("PRC_CRT", &stat);
    bench_test_prc_del(&mut stat);
    bench_report("PRC_DEL", &stat);
    bench_test_prc_cpt(&mut stat);
    bench_report("PRC_CPT", &stat);
    bench_test_prc_pgt(&mut stat);
    bench_report("PRC_PGT", &stat);

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

    bench_test_kfn_idle_sleep(&mut stat);
    bench_report("KFN(IDLE_SLEEP tick)", &stat);

    bench_print_str("\r\nRME BENCH REPORT OK\r\n");

    /* Idle forever - busy loop */
    loop {
        core::hint::spin_loop();
    }
}
/* End Function:bench_main ***********************************************/

/* End Of File ***************************************************************/

/* Copyright (C) Evo-Devo Instrum. All rights reserved ***********************/
