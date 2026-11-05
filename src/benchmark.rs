/******************************************************************************
Filename    : benchmark.rs
Author      : yhy
Date        : 05/11/2026
Licence     : The Unlicense; see LICENSE for details.
Description : The benchmark test for the RV64V port of RME, written in Rust
              for the user-mode init program. It is the Rust counterpart of
              the C rme_benchmark.c in M7M01_Eukaron/Source/Benchmark/, and
              it mirrors the scenario list of the Ammonite benchmark
              rvm_benchmark.h (M7M02_Ammonite/Include/Test/rvm_benchmark.h):
                - Yield (self, one-way)    THD_SWT onto the calling thread
                - Signal (self)            SIG_SND + SIG_RCV, self to self
                - Invocation (round-trip)  INV_ACT + INV_RET
                - Vector                    tick preemption notification
                - Signal (intra)            SIG_SND + SIG_RCV inside a process
                - Signal (inter)            SIG_SND + SIG_RCV across processes
                - Yield (inter, one-way)   THD_SWT into another process
              The topology mirrors the C benchmark as well: process 1 is the
              init process that runs the measurement, and process 2 is a
              second process created by the benchmark itself. Both processes
              share the init image's physical pages, so the very same Rust
              code runs in either of them.
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
                - The helper thread of process 1 (priority 1) receives the
                  signals of the intra-process test and burns a timeslice in
                  the vector test.
                - The helper thread of process 2 (priority 2) receives the
                  signals of the inter-process test.
                - The switch target of process 2 (priority 0) is the target
                  of the inter-process yield test, because a thread switch
                  needs the very same priority on both sides.
              Syscall ABI (mirror of the kernel's __RME_Svc_Param_Get):
                a0 = (Svc_Num << 16) | Cid
                a1 = Param0, a2 = Param1, a3 = Param2
                ecall
                return value comes back in a0
*****************************************************************************/

/* Attribute *****************************************************************/
/* This module mirrors the whole C rme_benchmark.c. Not every constant is
 * used by every platform, so dead-code warnings are suppressed on purpose. */
#![allow(dead_code)]
/* End Attribute *************************************************************/

/* Define ********************************************************************/
/* The number of rounds each test is repeated for before its average, maximum
 * and minimum results are calculated. */
const BENCH_LOOP_NUM: u64 = 10000;

/* The number of words reserved for each helper thread's stack. */
const BENCH_STACK_WORD_NUM: usize = 512;

/* Capability slots inside the init process' capability table. The table has
 * 64 entries and the boot code consumes slots 0..9, so the benchmark starts
 * at slot 10. */
const CAP_BENCH_THD: RmePtr = 10; /* The helper thread of process 1 */
const CAP_BENCH_CPT: RmePtr = 13; /* The capability table of process 2 */
const CAP_BENCH_PGT_TOP_NEW: RmePtr = 14; /* The root page table of process 2 */
const CAP_BENCH_PGT_L1: RmePtr = 15; /* The second level page table of process 2 */
const CAP_BENCH_PGT_L2: RmePtr = 16; /* The third level page table of process 2 */
const CAP_BENCH_PRC: RmePtr = 17; /* Process 2 itself */
const CAP_BENCH_P2_THD: RmePtr = 18; /* The switch target of process 2 */
const CAP_BENCH_SIG_SELF: RmePtr = 19; /* The self-to-self signal endpoint */
const CAP_BENCH_SIG_PARK: RmePtr = 20; /* The parking endpoint of process 1 */
const CAP_BENCH_SIG_DATA: RmePtr = 21; /* The data channel signal endpoint */
const CAP_BENCH_SIG_PARK2: RmePtr = 22; /* The parking endpoint of process 2 */
const CAP_BENCH_INV: RmePtr = 23; /* The invocation stub */
const CAP_BENCH_THD2: RmePtr = 24; /* The switch target of process 1 */

/* Capability slots inside process 2's own capability table. Every process
 * owns an independent capability ID namespace, so the capabilities that
 * process 2 needs have to be delegated into its table one by one. */
const CAP2_THD: RmePtr = 0; /* The receiver thread of process 2 */
const CAP2_INIT_THD: RmePtr = 1; /* Process 1's init thread, delegated */
const CAP2_SIG_DATA: RmePtr = 2; /* The data channel endpoint, delegated */
const CAP2_SIG_PARK: RmePtr = 3; /* The parking endpoint, delegated */
const CAP2_THD2: RmePtr = 4; /* The switch target thread of process 2 */

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
const PHASE_VECTOR: u32 = 3; /* Process 1's helper burns a timeslice */
const PHASE_SIG_RECV2: u32 = 4; /* Process 2's helper receives and measures */

/* The kernel memory frontier for the objects the benchmark creates. The
 * kernel's boot-time objects occupy the first part of KOM, so the benchmark
 * starts allocating at 0x10000 (64KB) to stay clear of them. */
const BENCH_KOM_FRONTIER: RmePtr = 0x10000;

/* The number of timeslices the measuring thread donates to a helper thread.
 * It is deliberately large enough that a helper never runs out of time
 * during a test, and small enough that the measuring thread keeps a healthy
 * share of the CPU. */
const BENCH_TIME_XFER: RmePtr = 10000000;

/* The number of rounds the vector test runs. A single round spans a whole
 * kernel tick, so there are far fewer rounds than in the other tests. */
const BENCH_VECTOR_ROUNDS: u64 = 100;

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
    RME_BOOT_INIT_PRC, RME_BOOT_INIT_THD, RME_BOOT_INIT_THD_SLOT, RME_BOOT_TBL_THD,
    RME_CID_NULL, RME_KFN_DEBUG_PRINT,
    RME_PGT_ALL_DYN, RME_PGT_ALL_PERM, RME_PGT_NUM_512, RME_PGT_SIZE_1G, RME_PGT_SIZE_2M,
    RME_PGT_SIZE_4K, RME_RCV_BM, RME_SIG_FLAG_ALL, RME_SVC_CPT_CRT, RME_SVC_KFN, RME_SVC_PGT_ADD,
    RME_SVC_PGT_CON, RME_SVC_PGT_CRT, RME_SVC_PRC_CRT, RME_SVC_SIG_CRT, RME_SVC_SIG_RCV,
    RME_SVC_SIG_SND, RME_SVC_THD_CRT, RME_SVC_THD_EXEC_SET, RME_SVC_THD_SCHED_BIND,
    RME_SVC_THD_SCHED_RCV, RME_SVC_THD_SWT, RME_SVC_THD_TIME_XFER, RME_THD_FLAG_ALL,
    RME_THD_FLAG_SWT, RmePtr, rme_cid_2l, rme_cpt_add, rme_inv_act, rme_inv_crt, rme_inv_ret,
    rme_inv_set, rme_param_d0, rme_param_d1, rme_param_q0, rme_param_q1, rme_syscall,
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
/* End Private Variables *****************************************************/

/* Function:bench_tsc ********************************************************
Description : Read the timestamp counter. On RV64V this is the RISC-V cycle
              counter, the analog of the ARM TIM2->CNT used by the C
              benchmark - a free-running counter at CPU clock frequency.
Input       : None.
Output      : None.
Return      : u64 - The current cycle count.
*****************************************************************************/
#[inline(always)]
fn bench_tsc() -> u64 {
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
/* End Function:bench_tsc ****************************************************/

/* Function:rme_print_char ***************************************************
Description : Print one character by invoking the DEBUG_PRINT kernel function.
              Param0 packs D0 = Func_ID (DEBUG_PRINT) and D1 = Sub_ID (the
              character), the same convention as the C init's Print_Char.
Input       : ch - The character to print.
Output      : None.
Return      : None.
*****************************************************************************/
#[inline(always)]
pub fn rme_print_char(ch: u8) {
    /* Param0 = (Sub_ID << 32) | Func_ID */
    let param0: u64 = ((ch as u64) << 32) | RME_KFN_DEBUG_PRINT;

    let _ = rme_syscall(RME_SVC_KFN, RME_BOOT_INIT_KFN, param0, 0, 0);
}
/* End Function:rme_print_char ***********************************************/

/* Function:rme_print_str ****************************************************
Description : Print a string, one syscall per character.
Input       : s - The string to print.
Output      : None.
Return      : None.
*****************************************************************************/
#[inline(always)]
fn rme_print_str(s: &str) {
    for &ch in s.as_bytes() {
        rme_print_char(ch);
    }
}
/* End Function:rme_print_str ************************************************/

/* Function:rme_print_uint ***************************************************
Description : Print an unsigned integer in decimal, mirroring the C
              RME_Sprint_Uint + debug print path.
Input       : value - The integer to print.
Output      : None.
Return      : None.
*****************************************************************************/
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

/* Type ***********************************************************************/
/// The accumulated statistics of a single benchmark test.
struct BenchStat {
    total: u64,
    max: u32,
    min: u32,
    count: u64,
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
        }
    }

    /// Reset the accumulator, so that it can be reused for another test.
    fn reset(&mut self) {
        self.total = 0;
        self.max = 0;
        self.min = u32::MAX;
        self.count = 0;
    }

    /// Accumulate the result of a single round into the current statistics.
    #[inline(always)]
    fn record(&mut self, diff: u32) {
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
    let rounds = if stat.count > 0 { stat.count } else { 1 };

    rme_print_str(title);
    rme_print_str(" : ");
    rme_print_uint(stat.total / rounds);
    rme_print_str(" / ");
    rme_print_uint(stat.max as u64);
    rme_print_str(" / ");
    rme_print_uint(stat.min as u64);
    rme_print_str("\r\n");
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
    rme_syscall(
        RME_SVC_THD_CRT,
        cap_cpt,
        rme_param_d1(RME_BOOT_INIT_KOM) | rme_param_d0(cap_thd),
        rme_param_d1(cap_prc) | rme_param_d0(31),
        raddr,
    )
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
    rme_syscall(
        RME_SVC_THD_SCHED_BIND,
        cap_thd,
        rme_param_d1(RME_BOOT_INIT_THD) | rme_param_d0(RME_CID_NULL),
        rme_param_d1(0) | rme_param_d0(prio),
        0,
    )
}
/* End Function:bench_thd_bind ************************************************/

/* Function:bench_thd_exec_set ***********************************************
Description : Set a thread's entry point, stack pointer and role.
Input       : cap_thd - The capability to the thread (two-level).
              entry - The entry point address.
              stack - The stack pointer.
              param - The role passed to the entry point in a0.
Output      : None.
Return      : u64 - The kernel return value (in a0).
*****************************************************************************/
#[inline(always)]
fn bench_thd_exec_set(cap_thd: u64, entry: u64, stack: u64, param: u64) -> u64 {
    rme_syscall(RME_SVC_THD_EXEC_SET, cap_thd, entry, stack, param)
}
/* End Function:bench_thd_exec_set ********************************************/

/* Function:bench_thd_swt ***************************************************
Description : Switch to another thread. The target must have the same
              preemptive priority and be bound to the same core.
Input       : cap_thd - The capability to the target thread (two-level).
Output      : None.
Return      : u64 - The kernel return value (in a0).
*****************************************************************************/
#[inline(always)]
fn bench_thd_swt(cap_thd: u64) -> u64 {
    rme_syscall(RME_SVC_THD_SWT, 0, cap_thd, 0, 0)
}
/* End Function:bench_thd_swt *************************************************/

/* Function:bench_thd_time_xfer *********************************************
Description : Transfer timeslices between two threads.
Input       : cap_dst - The capability to the receiving thread (two-level).
              cap_src - The capability to the donating thread (two-level).
              time - The number of timeslices to transfer.
Output      : None.
Return      : u64 - The kernel return value (in a0).
*****************************************************************************/
#[inline(always)]
fn bench_thd_time_xfer(cap_dst: u64, cap_src: u64, time: u64) -> u64 {
    rme_syscall(RME_SVC_THD_TIME_XFER, 0, cap_dst, cap_src, time)
}
/* End Function:bench_thd_time_xfer ******************************************/

/* Function:bench_thd_sched_rcv *********************************************
Description : Poll the init thread's scheduler notification queue. The kernel
              pushes a notification there when a thread runs out of its
              timeslices, which is the user visible trace of a tick
              preemption.
Input       : None.
Output      : None.
Return      : i64 - The kernel return value, negative when there is nothing
                    to report yet.
*****************************************************************************/
#[inline(always)]
fn bench_thd_sched_rcv() -> i64 {
    rme_syscall(RME_SVC_THD_SCHED_RCV, 0, RME_BOOT_INIT_THD, 0, 0) as i64
}
/* End Function:bench_thd_sched_rcv ******************************************/

/* Function:bench_sig_crt ***************************************************
Description : Create a signal endpoint in the init capability table.
Input       : cap_sig - The capability slot of the new endpoint.
Output      : None.
Return      : u64 - The kernel return value (in a0).
*****************************************************************************/
#[inline(always)]
fn bench_sig_crt(cap_sig: u64) -> u64 {
    rme_syscall(RME_SVC_SIG_CRT, RME_BOOT_INIT_CPT, cap_sig, 0, 0)
}
/* End Function:bench_sig_crt *************************************************/

/* Function:bench_sig_snd ***************************************************
Description : Send a signal to a signal endpoint.
Input       : cap_sig - The capability to the signal endpoint (two-level).
Output      : None.
Return      : u64 - The kernel return value (in a0).
*****************************************************************************/
#[inline(always)]
fn bench_sig_snd(cap_sig: u64) -> u64 {
    rme_syscall(RME_SVC_SIG_SND, 0, cap_sig, 1, 0)
}
/* End Function:bench_sig_snd *************************************************/

/* Function:bench_sig_rcv ***************************************************
Description : Receive from a signal endpoint. The blocking mode returns as
              soon as at least one signal is pending.
Input       : cap_sig - The capability to the signal endpoint (two-level).
Output      : None.
Return      : u64 - The number of signals received, or an error code.
*****************************************************************************/
#[inline(always)]
fn bench_sig_rcv(cap_sig: u64) -> u64 {
    rme_syscall(RME_SVC_SIG_RCV, 0, cap_sig, RME_RCV_BM, 0)
}
/* End Function:bench_sig_rcv *************************************************/

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

/* Function:bench_p1t2_entry ***********************************************
Description : The entry point of the helper thread of process 1. It plays the
              receiving role in the intra-process signal test and the load
              role in the vector test.
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
            PHASE_VECTOR => {
                bench_vector_load();
                bench_phase_set(PHASE_IDLE);
            }
            _ => {
                /* Nothing to do - park until the measuring thread wakes us */
                let cap_park = unsafe { core::ptr::read_volatile(&raw const CAP_SIG_PARK) };
                let _ = bench_sig_rcv(cap_park);
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
                let _ = bench_sig_rcv(CAP2_SIG_PARK);
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
    /* The ticket of the last inter-process switch this thread served. */
    let mut last_ticket: u64 = 0;

    loop {
        /* The measuring thread raises the ticket before it switches to us,
         * so the ticket tells us that a switch is waiting for us. One
         * ticket is served exactly once. */
        let ticket = bench_ticket_inter_get();

        if ticket != last_ticket {
            last_ticket = ticket;

            /* Latch the arrival of the switch, then hand the CPU back */
            bench_end_set(bench_tsc());
            let cap_p1t1 = unsafe { core::ptr::read_volatile(&raw const CAP_P1T1_IN_P2) };
            let _ = bench_thd_swt(cap_p1t1);
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
Description : The entry point of the switch target of process 1. It latches
              the arrival timestamp of the intra-process switch and switches
              straight back.
Input       : _param - The role passed by the creator (unused).
Output      : None.
Return      : ! - This function never returns.
*****************************************************************************/
extern "C" fn bench_p1t3_entry(_param: u64) -> ! {
    /* The ticket of the last intra-process switch this thread served. */
    let mut last_ticket: u64 = 0;

    loop {
        /* The measuring thread raises the ticket before it switches to us,
         * so the ticket tells us that a switch is waiting for us. One
         * ticket is served exactly once. */
        let ticket = bench_ticket_intra_get();

        if ticket != last_ticket {
            last_ticket = ticket;

            /* Latch the arrival of the switch, then hand the CPU back */
            bench_end_set(bench_tsc());
            let _ = bench_thd_swt(RME_BOOT_INIT_THD);
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

    while count < BENCH_LOOP_NUM {
        let start = bench_tsc();
        let _ = bench_sig_snd(cap_self);
        let _ = bench_sig_rcv(cap_self);
        let end = bench_tsc();

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

    while count < BENCH_LOOP_NUM {
        /* Wait for the workload - this blocks until the sender wakes us */
        let _ = bench_sig_rcv(cap_data);
        let end = bench_tsc();

        bench_stat_record((end - bench_start_get()) as u32);
        count += 1;
    }
}
/* End Function:bench_signal_receive ****************************************/

/* Function:bench_vector_load ***********************************************
Description : The load side of the vector test. The helper thread burns its
              only timeslice in a tight loop, so that the next kernel tick
              preempts it and notifies its scheduler - the measuring thread.
              The timestamp it latches before the loop is the departure of
              the workload.
Input       : None.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_vector_load() {
    /* Latch the departure of the workload */
    bench_start_set(bench_tsc());

    /* Burn the timeslice until the measuring thread lets us go */
    while bench_stop_get() == 0 {
        core::hint::spin_loop();
    }
}
/* End Function:bench_vector_load *******************************************/

/* Function:bench_prepare_process2 *****************************************
Description : Create the second process with its own capability table, its
              own three-level Sv39 page table and its own threads. The new
              process shares the init image's physical pages, so the new
              threads can execute the very same code and use the very same
              stacks.
Input       : None.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_prepare_process2() {
    let mut raddr: u64;

    /* ---- Create the new process's capability table (16 entries) ---- */
    /* CPT_CRT: Cid=Cap_Cpt_Crt, Param0=D1(Cap_Kom)|D0(Cap_Crt),
     * Param1=Raddr, Param2=Entry_Num */
    raddr = BENCH_KOM_FRONTIER;
    let _ = rme_syscall(
        RME_SVC_CPT_CRT,
        RME_BOOT_INIT_CPT,
        rme_param_d1(RME_BOOT_INIT_KOM) | rme_param_d0(CAP_BENCH_CPT),
        raddr,
        16,
    );
    raddr += 0x400;

    /* ---- Create the new process's page tables (root + L1 + L2) ---- */
    /* Root: 1GB entries, covers the low half. Raddr must be 4KB aligned
     * because Sv39 finds a table through its PPN. */
    raddr = (raddr + 0xFFF) & !0xFFF;
    let _ = bench_pgt_crt(CAP_BENCH_PGT_TOP_NEW, raddr, 0, 1, RME_PGT_SIZE_1G);
    raddr += 0x1000;
    /* L1: 2MB entries, covers the user window [0,1GB) */
    let _ = bench_pgt_crt(CAP_BENCH_PGT_L1, raddr, 0, 0, RME_PGT_SIZE_2M);
    raddr += 0x1000;
    /* L2: 4KB entries, covers [0x20000000,0x20200000) */
    let _ = bench_pgt_crt(CAP_BENCH_PGT_L2, raddr, 0x20000000, 0, RME_PGT_SIZE_4K);
    raddr += 0x1000;

    /* ---- Connect the tables (build the address space tree) ---- */
    /* root[0] -> L1 (0x20000000>>30 = 0) */
    let _ = bench_pgt_con(CAP_BENCH_PGT_TOP_NEW, RME_RV64V_TOP_POS, CAP_BENCH_PGT_L1);
    /* L1[256] -> L2 (0x20000000>>21 = 256) */
    let _ = bench_pgt_con(CAP_BENCH_PGT_L1, RME_RV64V_L1_POS, CAP_BENCH_PGT_L2);

    /* ---- Delegate the init image's code/bss pages into the new L2 ---- */
    /* Copy pages 0..15 (VA 0x20000000..0x20010000) from the init L2 table */
    let mut count: u64 = 0;
    while count < RME_RV64V_IMAGE_PAGES {
        let _ = bench_pgt_add(CAP_BENCH_PGT_L2, count, RME_BOOT_INIT_PGT_L2, count);
        count += 1;
    }

    /* ---- Create the new process ---- */
    /* PRC_CRT: Cid=Cap_Cpt_Crt, Param0=Cap_Prc, Param1=Cap_Cpt, Param2=Cap_Pgt.
     * The new process gets its OWN capability table and its OWN page table. */
    let _ = rme_syscall(
        RME_SVC_PRC_CRT,
        RME_BOOT_INIT_CPT,
        CAP_BENCH_PRC,
        CAP_BENCH_CPT,
        CAP_BENCH_PGT_TOP_NEW,
    );

    /* ---- Create the receiver thread in the new process' OWN captbl ---- */
    /* THD_CRT: Cid=Cap_Cpt, Param0=D1(Cap_Kom)|D0(Cap_Thd),
     * Param1=D1(Cap_Prc)|D0(Prio_Max), Param2=Raddr.
     * Cid now points at the NEW captbl, so the new thread's cap slot is
     * created in the new process' independent capability namespace. */
    raddr = (raddr + 0xFFF) & !0xFFF;
    let _ = bench_thd_crt(CAP_BENCH_CPT, CAP2_THD, CAP_BENCH_PRC, raddr);
    raddr += 0x1000;

    /* ---- Create the switch target thread in the new process' OWN captbl ---- */
    let _ = bench_thd_crt(CAP_BENCH_CPT, CAP2_THD2, CAP_BENCH_PRC, raddr);
    raddr += 0x1000;

    /* ---- Create the helper thread of process 1 ---- */
    let _ = bench_thd_crt(RME_BOOT_INIT_CPT, CAP_BENCH_THD, RME_BOOT_INIT_PRC, raddr);
    raddr += 0x1000;

    /* ---- Create the switch target of process 1 ---- */
    /* It shares the init process' capability table, so no delegation is
     * needed for it. */
    let _ = bench_thd_crt(RME_BOOT_INIT_CPT, CAP_BENCH_THD2, RME_BOOT_INIT_PRC, raddr);

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
    let _ = rme_cpt_add(
        RME_BOOT_INIT_CPT,
        CAP_BENCH_P2_THD,
        CAP_BENCH_CPT,
        CAP2_THD2,
        RME_THD_FLAG_ALL,
    );
    let _ = rme_cpt_add(
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
Return      : None.
*****************************************************************************/
fn bench_prepare_signals() {
    /* ---- Create the signal endpoints in the init capability table ---- */
    let _ = bench_sig_crt(CAP_BENCH_SIG_SELF);
    let _ = bench_sig_crt(CAP_BENCH_SIG_PARK);
    let _ = bench_sig_crt(CAP_BENCH_SIG_DATA);
    let _ = bench_sig_crt(CAP_BENCH_SIG_PARK2);

    /* ---- Delegate the endpoints the second process needs ---- */
    let _ = rme_cpt_add(
        CAP_BENCH_CPT,
        CAP2_SIG_DATA,
        RME_BOOT_INIT_CPT,
        CAP_BENCH_SIG_DATA,
        RME_SIG_FLAG_ALL,
    );
    let _ = rme_cpt_add(
        CAP_BENCH_CPT,
        CAP2_SIG_PARK,
        RME_BOOT_INIT_CPT,
        CAP_BENCH_SIG_PARK2,
        RME_SIG_FLAG_ALL,
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
Return      : None.
*****************************************************************************/
fn bench_prepare_invocation() {
    /* The invocation stub object lives in the kernel memory, right behind the
     * thread objects the preparation has allocated so far. */
    let raddr: u64 = BENCH_KOM_FRONTIER + 0x18000;

    /* INV_CRT: Cid=Cap_Cpt, Param0=D1(Cap_Kom)|D0(Cap_Inv), Param1=Cap_Prc,
     * Param2=Raddr. The target process is the second process. */
    let _ = rme_inv_crt(
        RME_BOOT_INIT_CPT,
        RME_BOOT_INIT_KOM,
        CAP_BENCH_INV,
        CAP_BENCH_PRC,
        raddr,
    );

    /* INV_SET: Param0=D0(Cap_Inv)|D1(Is_Exc_Ret), Param1=Entry, Param2=Stack.
     * The stack lives in the init image's bss, which the second process
     * maps physically one to one. */
    let entry = bench_inv_entry as extern "C" fn(u64) -> ! as u64;
    let stack = bench_stack_top(unsafe {
        core::ptr::addr_of!(BENCH_STACK_INV[BENCH_STACK_WORD_NUM - 1]) as u64
    });
    let _ = rme_inv_set(
        rme_cid_2l(RME_BOOT_INIT_CPT, CAP_BENCH_INV),
        entry,
        stack,
        0,
    );
}
/* End Function:bench_prepare_invocation ************************************/

/* Function:bench_prepare_threads ******************************************
Description : Start the helper threads. All of them are bound to the current
              core and get a share of the init thread's timeslice.
              The helper thread of process 1 runs one priority level above
              the init thread and the receiver of process 2 runs two levels
              above it, so that a signal wakes them up right away and the
              init thread is not scheduled in their place.
              The switch target of process 2 runs on the init thread's own
              priority level, because a thread switch needs the very same
              priority on both sides.
Input       : None.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_prepare_threads() {
    /* The helper thread of process 1 */
    let cap_p1t2 = rme_cid_2l(RME_BOOT_INIT_CPT, CAP_BENCH_THD);
    let _ = bench_thd_bind(cap_p1t2, BENCH_PRIO_RECV);
    let entry = bench_p1t2_entry as extern "C" fn(u64) -> ! as u64;
    let stack = bench_stack_top(unsafe {
        core::ptr::addr_of!(BENCH_STACK_P1T2[BENCH_STACK_WORD_NUM - 1]) as u64
    });
    let _ = bench_thd_exec_set(cap_p1t2, entry, stack, 0);

    /* The receiver thread of the second process */
    let cap_p2t1 = rme_cid_2l(CAP_BENCH_CPT, CAP2_THD);
    let _ = bench_thd_bind(cap_p2t1, BENCH_PRIO_RECV2);
    let entry = bench_p2t1_entry as extern "C" fn(u64) -> ! as u64;
    let stack = bench_stack_top(unsafe {
        core::ptr::addr_of!(BENCH_STACK_P2T1[BENCH_STACK_WORD_NUM - 1]) as u64
    });
    let _ = bench_thd_exec_set(cap_p2t1, entry, stack, 0);

    /* The switch target of the second process */
    let cap_p2t2 = unsafe { core::ptr::read_volatile(&raw const CAP_P2_THD) };
    let _ = bench_thd_bind(cap_p2t2, BENCH_PRIO_INIT);
    let entry = bench_p2t2_entry as extern "C" fn(u64) -> ! as u64;
    let stack = bench_stack_top(unsafe {
        core::ptr::addr_of!(BENCH_STACK_P2T2[BENCH_STACK_WORD_NUM - 1]) as u64
    });
    let _ = bench_thd_exec_set(cap_p2t2, entry, stack, 0);

    /* The switch target of process 1 */
    let cap_p1t3 = rme_cid_2l(RME_BOOT_INIT_CPT, CAP_BENCH_THD2);
    let _ = bench_thd_bind(cap_p1t3, BENCH_PRIO_INIT);
    let entry = bench_p1t3_entry as extern "C" fn(u64) -> ! as u64;
    let stack = bench_stack_top(unsafe {
        core::ptr::addr_of!(BENCH_STACK_P1T3[BENCH_STACK_WORD_NUM - 1]) as u64
    });
    let _ = bench_thd_exec_set(cap_p1t3, entry, stack, 0);

    /* The init thread donates its timeslice to the helper threads. This
     * also takes the init thread out of the "infinite timeslice" state it
     * was booted with, which is what allows it to be scheduled out while
     * it waits for a helper thread. */
    let _ = bench_thd_time_xfer(cap_p1t2, RME_BOOT_INIT_THD, BENCH_TIME_XFER);
    let _ = bench_thd_time_xfer(cap_p1t3, RME_BOOT_INIT_THD, BENCH_TIME_XFER);
    let _ = bench_thd_time_xfer(cap_p2t1, RME_BOOT_INIT_THD, BENCH_TIME_XFER);
    let _ = bench_thd_time_xfer(cap_p2t2, RME_BOOT_INIT_THD, BENCH_TIME_XFER);

    unsafe {
        core::ptr::write_volatile(&raw mut CAP_P1T2, cap_p1t2);
        core::ptr::write_volatile(&raw mut CAP_P1T3, cap_p1t3);
    }
}
/* End Function:bench_prepare_threads **************************************/

/* Function:bench_wake_helper **********************************************
Description : Publish a phase and wake the helper thread of process 1 up.
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
    let _ = bench_sig_snd(cap_park);
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
    let _ = bench_sig_snd(cap_park);
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

    while count < BENCH_LOOP_NUM {
        let start = bench_tsc();
        let _ = bench_thd_swt(RME_BOOT_INIT_THD);
        let end = bench_tsc();

        stat.record((end - start) as u32);
        count += 1;
    }
}
/* End Function:bench_test_yield_self ****************************************/

/* Function:bench_test_signal_self ******************************************
Description : Measure the cost of a signal that is sent to, and received
              from, the very same thread. The helper thread of process 1
              runs the test, because the kernel forbids a boot-time thread
              from receiving a signal at all.
Input       : stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_test_signal_self(stat: &mut BenchStat) {
    /* Wake the helper thread - it runs the whole test and parks itself
     * again before this call returns */
    bench_wake_helper(PHASE_SIG_SELF);

    stat.reset();
    stat.load_shared();
}
/* End Function:bench_test_signal_self **************************************/

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

    while count < BENCH_LOOP_NUM {
        let start = bench_tsc();
        let _ = rme_inv_act(cap_inv, 0x1234);
        let end = bench_tsc();

        stat.record((end - start) as u32);
        count += 1;
    }
}
/* End Function:bench_test_invocation ****************************************/

/* Function:bench_test_signal_intra *****************************************
Description : Measure the cost of a signal that travels between two threads
              of the same process. The init thread is the sender, the helper
              thread of process 1 is the receiver.
Input       : stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_test_signal_intra(stat: &mut BenchStat) {
    let cap_data = unsafe { core::ptr::read_volatile(&raw const CAP_SIG_DATA) };
    let mut count: u64 = 0;

    /* Let the helper thread take the receiving role and wait for the
     * workload on the data channel */
    bench_wake_helper(PHASE_SIG_RECV);

    while count < BENCH_LOOP_NUM {
        bench_start_set(bench_tsc());
        let _ = bench_sig_snd(cap_data);
        count += 1;
    }

    stat.reset();
    stat.load_shared();
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

    /* Let the receiver in the second process take the receiving role and
     * wait for the workload on the data channel */
    bench_wake_helper2(PHASE_SIG_RECV2);

    while count < BENCH_LOOP_NUM {
        bench_start_set(bench_tsc());
        let _ = bench_sig_snd(cap_data);
        count += 1;
    }

    stat.reset();
    stat.load_shared();
}
/* End Function:bench_test_signal_inter *************************************/

/* Function:bench_test_yield_intra *******************************************
Description : Measure the cost of switching between two threads of the same
              process. The switch is one way: the target thread latches the
              arrival timestamp and switches straight back, so the
              measurement does not contain the cost of the way back.
              This is the counterpart of the inter-process yield test, and
              it covers the path the C rme_benchmark.c measured with
              RME_Same_Prc_Thd_Switch_Test.
Input       : stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_test_yield_intra(stat: &mut BenchStat) {
    let cap_p1t3 = unsafe { core::ptr::read_volatile(&raw const CAP_P1T3) };
    let mut count: u64 = 0;

    stat.reset();

    while count < BENCH_LOOP_NUM {
        /* Raise the ticket, so that the target thread latches the arrival
         * exactly once for this switch */
        bench_ticket_intra_raise();
        bench_start_set(bench_tsc());
        let _ = bench_thd_swt(cap_p1t3);

        stat.record((bench_end_get() - bench_start_get()) as u32);
        count += 1;
    }
}
/* End Function:bench_test_yield_intra **************************************/

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

    while count < BENCH_LOOP_NUM {
        /* Raise the ticket, so that the target thread latches the arrival
         * exactly once for this switch */
        bench_ticket_inter_raise();
        bench_start_set(bench_tsc());
        let _ = bench_thd_swt(cap_p2t2);

        stat.record((bench_end_get() - bench_start_get()) as u32);
        count += 1;
    }
}
/* End Function:bench_test_yield_inter *************************************/

/* Function:bench_test_vector ***********************************************
Description : Measure the cost of the kernel's tick preemption reaching the
              user space. The helper thread of process 1 is given a single
              timeslice and burns it in a tight loop, so the next tick
              preempts it and notifies its scheduler - the init thread. The
              measurement is the distance between the helper's departure and
              the moment the notification becomes visible to the user.
              This is the RME counterpart of the vector delivery test of
              rvm_benchmark.h: RME has no user level interrupt handler, so
              the tick preemption notification is the only user visible
              trace of a physical event that reaches the user space.
Input       : stat - The statistics accumulator.
Output      : None.
Return      : None.
*****************************************************************************/
fn bench_test_vector(stat: &mut BenchStat) {
    let cap_p1t2 = unsafe { core::ptr::read_volatile(&raw const CAP_P1T2) };
    let cap_park = unsafe { core::ptr::read_volatile(&raw const CAP_SIG_PARK) };
    let mut count: u64 = 0;

    stat.reset();

    while count < BENCH_VECTOR_ROUNDS {
        /* Arm the load thread again */
        bench_stop_set(0);
        bench_phase_set(PHASE_VECTOR);

        /* Drain the notifications that are still pending */
        while bench_thd_sched_rcv() >= 0 {
            core::hint::spin_loop();
        }

        /* Revoke the helper thread's timeslice and give it a single one, so
         * that the very next kernel tick preempts it. The helper thread is
         * parked at this point, and a parked thread only becomes runnable
         * again when it is woken up with a signal. */
        let _ = bench_thd_time_xfer(RME_BOOT_INIT_THD, cap_p1t2, u64::MAX);
        let _ = bench_thd_time_xfer(cap_p1t2, RME_BOOT_INIT_THD, 1);

        /* Wake the helper thread up - it latches the departure timestamp and
         * burns its timeslice until the next tick preempts it */
        let _ = bench_sig_snd(cap_park);

        /* The tick preemption notifies this thread, as the helper's
         * scheduler */
        while bench_thd_sched_rcv() < 0 {
            core::hint::spin_loop();
        }
        bench_end_set(bench_tsc());

        stat.record((bench_end_get() - bench_start_get()) as u32);

        /* Release the helper thread, so that it can leave its loop */
        bench_stop_set(1);
        let _ = bench_thd_time_xfer(cap_p1t2, RME_BOOT_INIT_THD, BENCH_TIME_XFER);

        count += 1;
    }
}
/* End Function:bench_test_vector *******************************************/

/* Function:rme_benchmark ***************************************************
Description : The benchmark entry, also the init thread. It runs every test
              of the suite and prints the results in the order of the
              reference benchmark.
Input       : None.
Output      : None.
Return      : ! - This function never returns.
*****************************************************************************/
pub fn rme_benchmark() -> ! {
    let mut stat = BenchStat::new();

    /* Print table header - the banner is generated with
     * "figlet -f smslant -k RME" */
    rme_print_str("\r\n");
    rme_print_str("   ___   __  ___ ____\r\n");
    rme_print_str("  / _ \\ /  |/  // __/\r\n");
    rme_print_str(" / , _// /|_/ // _/\r\n");
    rme_print_str("/_/|_|/_/  /_//___/\r\n");
    rme_print_str("RME Concord Microkernel - Standard benchmark test\r\n");
    rme_print_str("====================================================\r\n");
    rme_print_str("Test (number in CPU cycles)        : AVG / MAX / MIN\r\n");

    /* The self yield test must run before any helper thread exists, so that
     * the kernel has nothing else to switch to. */
    bench_test_yield_self(&mut stat);
    bench_report("Yield (self, one-way)             ", &stat);

    /* Set up the second process, the helper threads, the signal endpoints
     * and the invocation stub */
    bench_prepare_process2();
    bench_prepare_signals();
    bench_prepare_invocation();
    bench_prepare_threads();

    /* The self signal test - the helper thread of process 1 runs it */
    bench_test_signal_self(&mut stat);
    bench_report("Signal (self)                     ", &stat);

    /* The invocation test */
    bench_test_invocation(&mut stat);
    bench_report("Invocation (round-trip)           ", &stat);

    /* The vector test - it exhausts the helper thread's timeslice, so it
     * has to come before the signal tests that need that thread again. */
    bench_test_vector(&mut stat);
    bench_report("Vector                            ", &stat);

    /* The intra-process signal test */
    bench_test_signal_intra(&mut stat);
    bench_report("Signal (intra)                    ", &stat);

    /* The inter-process signal test */
    bench_test_signal_inter(&mut stat);
    bench_report("Signal (inter)                    ", &stat);

    /* The inter-process yield test */
    bench_test_yield_inter(&mut stat);
    bench_report("Yield (inter, one-way)            ", &stat);

    /* The intra-process yield test */
    bench_test_yield_intra(&mut stat);
    bench_report("Yield (intra, one-way)            ", &stat);

    rme_print_str("RME BENCH REPORT OK\r\n");

    /* Idle forever - busy loop, no WFI (illegal instruction on QEMU) */
    loop {}
}
/* End Function:rme_benchmark ***********************************************/

/* End Of File ***************************************************************/

/* Copyright (C) Evo-Devo Instrum. All rights reserved ***********************/
