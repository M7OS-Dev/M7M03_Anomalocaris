/******************************************************************************
Filename    : syscall.rs
Author      : yhy
Date        : 13/10/2026
Licence     : The Unlicense; see LICENSE for details.
Description : User-mode system call interface for the RV64V port on QEMU
              virt, mirroring the C RME_Svc.h. Mirrors the kernel's
              syscall ABI and constants (Include/rme.h) without depending on
              kernel headers, so any user program can include it standalone.
              Syscall ABI on RV64V (64-bit split):
                a0 = (Svc << 32) | Cid
                a1 = Param0, a2 = Param1, a3 = Param2
                ecall                 (cause 8, delegated to S-mode by OpenSBI)
                return value comes back in a0; some calls also use a1
              RME_Svc_Raw() is the ONLY ecall in a user program - everything
              else is built on top of it.
******************************************************************************/

/* Attribute *****************************************************************/
/* This module is a complete ABI mirror of the kernel's syscall and kernel
 * function IDs (Include/rme.h). Not every constant is used by the init
 * program yet, so dead-code warnings are suppressed on purpose. */
#![allow(dead_code)]
/* End Attribute *************************************************************/

/* Type ***********************************************************************/
/// rme_ptr_t - a pointer-sized unsigned integer (u64 on RV64V).
pub type rme_ptr_t = u64;
/* End Type ******************************************************************/

/* Define ********************************************************************/
/* System call numbers (mirror of Include/rme.h) *****************************/
pub const RME_SVC_INV_RET: rme_ptr_t = 0;
pub const RME_SVC_INV_ACT: rme_ptr_t = 1;
pub const RME_SVC_SIG_SND: rme_ptr_t = 2;
pub const RME_SVC_SIG_RCV: rme_ptr_t = 3;
pub const RME_SVC_KFN: rme_ptr_t = 4;
pub const RME_SVC_THD_SCHED_FREE: rme_ptr_t = 5;
pub const RME_SVC_THD_EXEC_SET: rme_ptr_t = 6;
pub const RME_SVC_THD_SCHED_PRIO: rme_ptr_t = 7;
pub const RME_SVC_THD_TIME_XFER: rme_ptr_t = 8;
pub const RME_SVC_THD_SWT: rme_ptr_t = 9;
pub const RME_SVC_CPT_CRT: rme_ptr_t = 10;
pub const RME_SVC_CPT_DEL: rme_ptr_t = 11;
pub const RME_SVC_CPT_FRZ: rme_ptr_t = 12;
pub const RME_SVC_CPT_ADD: rme_ptr_t = 13;
pub const RME_SVC_CPT_REM: rme_ptr_t = 14;
pub const RME_SVC_PGT_CRT: rme_ptr_t = 15;
pub const RME_SVC_PGT_DEL: rme_ptr_t = 16;
pub const RME_SVC_PGT_ADD: rme_ptr_t = 17;
pub const RME_SVC_PGT_REM: rme_ptr_t = 18;
pub const RME_SVC_PGT_CON: rme_ptr_t = 19;
pub const RME_SVC_PGT_DES: rme_ptr_t = 20;
pub const RME_SVC_PRC_CRT: rme_ptr_t = 21;
pub const RME_SVC_PRC_DEL: rme_ptr_t = 22;
pub const RME_SVC_PRC_CPT: rme_ptr_t = 23;
pub const RME_SVC_PRC_PGT: rme_ptr_t = 24;
pub const RME_SVC_THD_CRT: rme_ptr_t = 25;
pub const RME_SVC_THD_DEL: rme_ptr_t = 26;
pub const RME_SVC_THD_SCHED_BIND: rme_ptr_t = 27;
pub const RME_SVC_THD_SCHED_RCV: rme_ptr_t = 28;
pub const RME_SVC_SIG_CRT: rme_ptr_t = 29;
pub const RME_SVC_SIG_DEL: rme_ptr_t = 30;
pub const RME_SVC_INV_CRT: rme_ptr_t = 31;
pub const RME_SVC_INV_DEL: rme_ptr_t = 32;
pub const RME_SVC_INV_SET: rme_ptr_t = 33;

/* Kernel function IDs (mirror of Include/rme.h) *****************************/
pub const RME_KFN_PGT_CACHE_CLR: rme_ptr_t = 0xF000;
pub const RME_KFN_PGT_LINE_CLR: rme_ptr_t = 0xF001;
pub const RME_KFN_PGT_ASID_SET: rme_ptr_t = 0xF002;
pub const RME_KFN_PGT_TLB_LOCK: rme_ptr_t = 0xF003;
pub const RME_KFN_PGT_ENTRY_MOD: rme_ptr_t = 0xF004;
pub const RME_KFN_INT_LOCAL_MOD: rme_ptr_t = 0xF100;
pub const RME_KFN_INT_GLOBAL_MOD: rme_ptr_t = 0xF101;
pub const RME_KFN_INT_LOCAL_TRIG: rme_ptr_t = 0xF102;
pub const RME_KFN_EVT_LOCAL_TRIG: rme_ptr_t = 0xF103;
pub const RME_KFN_CACHE_MOD: rme_ptr_t = 0xF200;
pub const RME_KFN_CACHE_CONFIG: rme_ptr_t = 0xF201;
pub const RME_KFN_CACHE_MAINT: rme_ptr_t = 0xF202;
pub const RME_KFN_CACHE_LOCK: rme_ptr_t = 0xF203;
pub const RME_KFN_PRFTH_MOD: rme_ptr_t = 0xF204;
pub const RME_KFN_HPNP_PCPU_MOD: rme_ptr_t = 0xF300;
pub const RME_KFN_HPNP_LCPU_MOD: rme_ptr_t = 0xF301;
pub const RME_KFN_HPNP_PMEM_MOD: rme_ptr_t = 0xF302;
pub const RME_KFN_IDLE_SLEEP: rme_ptr_t = 0xF400;
pub const RME_KFN_SYS_REBOOT: rme_ptr_t = 0xF401;
pub const RME_KFN_SYS_SHDN: rme_ptr_t = 0xF402;
pub const RME_KFN_VOLT_MOD: rme_ptr_t = 0xF403;
pub const RME_KFN_FREQ_MOD: rme_ptr_t = 0xF404;
pub const RME_KFN_PMOD_MOD: rme_ptr_t = 0xF405;
pub const RME_KFN_SAFETY_MOD: rme_ptr_t = 0xF406;
pub const RME_KFN_PERF_CPU_FUNC: rme_ptr_t = 0xF500;
pub const RME_KFN_PERF_MON_MOD: rme_ptr_t = 0xF501;
pub const RME_KFN_PERF_CNT_MOD: rme_ptr_t = 0xF502;
pub const RME_KFN_PERF_CYCLE_MOD: rme_ptr_t = 0xF503;
pub const RME_KFN_PERF_DATA_MOD: rme_ptr_t = 0xF504;
pub const RME_KFN_PERF_PHYS_MOD: rme_ptr_t = 0xF505;
pub const RME_KFN_PERF_CUMUL_MOD: rme_ptr_t = 0xF506;
pub const RME_KFN_VM_CRT: rme_ptr_t = 0xF600;
pub const RME_KFN_VM_DEL: rme_ptr_t = 0xF601;
pub const RME_KFN_VM_PGT: rme_ptr_t = 0xF602;
pub const RME_KFN_VM_MOD: rme_ptr_t = 0xF603;
pub const RME_KFN_VCPU_CRT: rme_ptr_t = 0xF604;
pub const RME_KFN_VCPU_BIND: rme_ptr_t = 0xF605;
pub const RME_KFN_VCPU_FREE: rme_ptr_t = 0xF606;
pub const RME_KFN_VCPU_DEL: rme_ptr_t = 0xF607;
pub const RME_KFN_VCPU_MOD: rme_ptr_t = 0xF608;
pub const RME_KFN_VCPU_RUN: rme_ptr_t = 0xF609;
pub const RME_KFN_ECLV_CRT: rme_ptr_t = 0xF700;
pub const RME_KFN_ECLV_MOD: rme_ptr_t = 0xF701;
pub const RME_KFN_ECLV_DEL: rme_ptr_t = 0xF702;
pub const RME_KFN_ECLV_ACT: rme_ptr_t = 0xF703;
pub const RME_KFN_ECLV_RET: rme_ptr_t = 0xF704;
pub const RME_KFN_DEBUG_PRINT: rme_ptr_t = 0xF800;
pub const RME_KFN_DEBUG_REG_MOD: rme_ptr_t = 0xF801;
pub const RME_KFN_DEBUG_INV_MOD: rme_ptr_t = 0xF802;
pub const RME_KFN_DEBUG_EXC_GET: rme_ptr_t = 0xF803;
pub const RME_KFN_DEBUG_MODE_MOD: rme_ptr_t = 0xF804;
pub const RME_KFN_DEBUG_IBP_MOD: rme_ptr_t = 0xF805;
pub const RME_KFN_DEBUG_DBP_MOD: rme_ptr_t = 0xF806;

/* RV64V cache maintenance KFN sub-IDs (mirror of rme_platform_rv64v.h) ******/
pub const RME_RV64V_KFN_CACHE_ICACHE: rme_ptr_t = 0;
pub const RME_RV64V_KFN_CACHE_DCACHE: rme_ptr_t = 1;
pub const RME_RV64V_KFN_CACHE_CLEAN_ALL: rme_ptr_t = 0;
pub const RME_RV64V_KFN_CACHE_CLEAN_ADDR: rme_ptr_t = 1;
pub const RME_RV64V_KFN_CACHE_INV_ALL: rme_ptr_t = 5;
pub const RME_RV64V_KFN_CACHE_INV_ADDR: rme_ptr_t = 6;
pub const RME_RV64V_KFN_CACHE_CLEAN_INV_ALL: rme_ptr_t = 10;
pub const RME_RV64V_KFN_CACHE_CLEAN_INV_ADDR: rme_ptr_t = 11;
pub const RME_RV64V_CACHE_BLOCK_SIZE: rme_ptr_t = 64;

/* RV64V page-table entry query KFN sub-IDs **********************************/
pub const RME_RV64V_KFN_PGT_ENTRY_MOD_FLAG_GET: rme_ptr_t = 0;
pub const RME_RV64V_KFN_PGT_ENTRY_MOD_SZORD_GET: rme_ptr_t = 1;
pub const RME_RV64V_KFN_PGT_ENTRY_MOD_NMORD_GET: rme_ptr_t = 2;

/* RV64V debug KFN sub-IDs ***************************************************/
pub const RME_RV64V_KFN_DEBUG_REG_MOD_GET: rme_ptr_t = 0;
pub const RME_RV64V_KFN_DEBUG_REG_MOD_SET: rme_ptr_t = 1 << 16;
pub const RME_RV64V_KFN_DEBUG_REG_MOD_X10_A0: rme_ptr_t = 11;

pub const RME_RV64V_KFN_DEBUG_INV_MOD_SP_GET: rme_ptr_t = 0;
pub const RME_RV64V_KFN_DEBUG_INV_MOD_SP_SET: rme_ptr_t = 1;
pub const RME_RV64V_KFN_DEBUG_INV_MOD_PC_GET: rme_ptr_t = 2;
pub const RME_RV64V_KFN_DEBUG_INV_MOD_PC_SET: rme_ptr_t = 3;

pub const RME_RV64V_KFN_DEBUG_EXC_CAUSE_GET: rme_ptr_t = 0;
pub const RME_RV64V_KFN_DEBUG_EXC_ADDR_GET: rme_ptr_t = 1;
pub const RME_RV64V_KFN_DEBUG_EXC_VALUE_GET: rme_ptr_t = 2;

/* KFN capability range: high limit in [63:32], low limit in [31:0]. The boot
 * KFN capability allows every function ID, so delegating it uses the same
 * full range. */
pub const RME_KFN_FLAG_FULL_RANGE: rme_ptr_t = 0xFFFF_FFFF_0000_0000;

/* Signal endpoint flags (mirror of Include/rme.h) *****************************/
pub const RME_SIG_FLAG_SND: rme_ptr_t = 1 << 0;
pub const RME_SIG_FLAG_RCV_BS: rme_ptr_t = 1 << 1;
pub const RME_SIG_FLAG_RCV_BM: rme_ptr_t = 1 << 2;
pub const RME_SIG_FLAG_RCV_NS: rme_ptr_t = 1 << 3;
pub const RME_SIG_FLAG_RCV_NM: rme_ptr_t = 1 << 4;
pub const RME_SIG_FLAG_SCHED: rme_ptr_t = 1 << 5;
pub const RME_SIG_FLAG_ALL: rme_ptr_t = RME_SIG_FLAG_SND
    | RME_SIG_FLAG_RCV_BS
    | RME_SIG_FLAG_RCV_BM
    | RME_SIG_FLAG_RCV_NS
    | RME_SIG_FLAG_RCV_NM
    | RME_SIG_FLAG_SCHED;

/* Receive options (mirror of Include/rme.h) **********************************/
pub const RME_RCV_BS: rme_ptr_t = 0;
pub const RME_RCV_BM: rme_ptr_t = 1;
pub const RME_RCV_NS: rme_ptr_t = 2;
pub const RME_RCV_NM: rme_ptr_t = 3;

/* Invocation stub flags (mirror of Include/rme.h) ****************************/
pub const RME_INV_FLAG_SET: rme_ptr_t = 1 << 0;
pub const RME_INV_FLAG_ACT: rme_ptr_t = 1 << 1;
pub const RME_INV_FLAG_ALL: rme_ptr_t = RME_INV_FLAG_SET | RME_INV_FLAG_ACT;

/* Capability table flags (mirror of Include/rme.h) ***************************/
pub const RME_CPT_FLAG_CRT: rme_ptr_t = 1 << 0;
pub const RME_CPT_FLAG_DEL: rme_ptr_t = 1 << 1;
pub const RME_CPT_FLAG_FRZ: rme_ptr_t = 1 << 2;
pub const RME_CPT_FLAG_ADD_SRC: rme_ptr_t = 1 << 3;
pub const RME_CPT_FLAG_ADD_DST: rme_ptr_t = 1 << 4;
pub const RME_CPT_FLAG_REM: rme_ptr_t = 1 << 5;
pub const RME_CPT_FLAG_PRC_CRT: rme_ptr_t = 1 << 6;
pub const RME_CPT_FLAG_PRC_CPT: rme_ptr_t = 1 << 7;
pub const RME_CPT_FLAG_ALL: rme_ptr_t = RME_CPT_FLAG_CRT
    | RME_CPT_FLAG_DEL
    | RME_CPT_FLAG_FRZ
    | RME_CPT_FLAG_ADD_SRC
    | RME_CPT_FLAG_ADD_DST
    | RME_CPT_FLAG_REM
    | RME_CPT_FLAG_PRC_CRT
    | RME_CPT_FLAG_PRC_CPT;

/* Process flags (mirror of Include/rme.h) ************************************/
pub const RME_PRC_FLAG_INV: rme_ptr_t = 1 << 0;
pub const RME_PRC_FLAG_THD: rme_ptr_t = 1 << 1;
pub const RME_PRC_FLAG_CPT: rme_ptr_t = 1 << 2;
pub const RME_PRC_FLAG_PGT: rme_ptr_t = 1 << 3;
pub const RME_PRC_FLAG_ALL: rme_ptr_t =
    RME_PRC_FLAG_INV | RME_PRC_FLAG_THD | RME_PRC_FLAG_CPT | RME_PRC_FLAG_PGT;

/* Kernel memory flags (mirror of Include/rme.h) ******************************/
pub const RME_KOM_FLAG_CPT: rme_ptr_t = 1 << 0;
pub const RME_KOM_FLAG_PGT: rme_ptr_t = 1 << 1;
pub const RME_KOM_FLAG_THD: rme_ptr_t = 1 << 2;
pub const RME_KOM_FLAG_INV: rme_ptr_t = 1 << 3;
pub const RME_KOM_FLAG_ALL: rme_ptr_t =
    RME_KOM_FLAG_CPT | RME_KOM_FLAG_PGT | RME_KOM_FLAG_THD | RME_KOM_FLAG_INV;

/* Boot capability slots (mirror of rme_platform_rv64v.h) ********************/
pub const RME_BOOT_INIT_CPT: rme_ptr_t = 0;
pub const RME_BOOT_INIT_PGT: rme_ptr_t = 1;
pub const RME_BOOT_INIT_PRC: rme_ptr_t = 2;
/* The per-CPU initial-thread capability table - one initial thread per hart.
 * The init thread of CPU0 therefore lives in slot 0 of this table, which
 * makes its capability ID two-level. */
pub const RME_BOOT_TBL_THD: rme_ptr_t = 3;
pub const RME_BOOT_INIT_KFN: rme_ptr_t = 4;
pub const RME_BOOT_INIT_KOM: rme_ptr_t = 5;
/* The per-CPU timer and general-interrupt endpoint tables */
pub const RME_BOOT_TBL_TIMER: rme_ptr_t = 6;
pub const RME_BOOT_TBL_INT: rme_ptr_t = 7;
pub const RME_BOOT_INIT_PGT_L1: rme_ptr_t = 8;
pub const RME_BOOT_INIT_PGT_L2: rme_ptr_t = 9;
/* Two-level capability encoding: (Table << 16) | 2L | Slot */
pub const RME_CID_2L: rme_ptr_t = 1 << 15;
/* CPU0's initial thread: slot 0 of the per-CPU thread table */
pub const RME_BOOT_INIT_THD: rme_ptr_t = (RME_BOOT_TBL_THD << 16) | RME_CID_2L;
/* The init thread's slot *inside* RME_BOOT_TBL_THD. CPT_ADD/REM address a
 * capability with one-level IDs, so they need this slot number and the table
 * slot, not the two-level RME_BOOT_INIT_THD. */
pub const RME_BOOT_INIT_THD_SLOT: rme_ptr_t = 0;
/* The null capability ID - high bit of the D1 field */
pub const RME_CID_NULL: rme_ptr_t = 0x80000000;

/* Page table flags (mirror of Include/rme.h) ********************************/
pub const RME_PGT_READ: rme_ptr_t = 1 << 0;
pub const RME_PGT_WRITE: rme_ptr_t = 1 << 1;
pub const RME_PGT_EXECUTE: rme_ptr_t = 1 << 2;
pub const RME_PGT_CACHE: rme_ptr_t = 1 << 3;
pub const RME_PGT_BUFFER: rme_ptr_t = 1 << 4;
pub const RME_PGT_STATIC: rme_ptr_t = 1 << 5;
pub const RME_PGT_ALL_DYN: rme_ptr_t =
    RME_PGT_READ | RME_PGT_WRITE | RME_PGT_EXECUTE | RME_PGT_CACHE | RME_PGT_BUFFER;
pub const RME_PGT_ALL_PERM: rme_ptr_t = RME_PGT_ALL_DYN | RME_PGT_STATIC;

/* Page table size/num orders (mirror of Include/rme.h) **********************/
pub const RME_PGT_SIZE_4K: rme_ptr_t = 12;
pub const RME_PGT_SIZE_2M: rme_ptr_t = 21;
pub const RME_PGT_SIZE_1G: rme_ptr_t = 30;
pub const RME_PGT_NUM_512: rme_ptr_t = 9;

/* Thread flags (mirror of Include/rme.h) ************************************/
pub const RME_THD_FLAG_EXEC_SET: rme_ptr_t = 1 << 0;
pub const RME_THD_FLAG_SCHED_CHILD: rme_ptr_t = 1 << 1;
pub const RME_THD_FLAG_SCHED_PARENT: rme_ptr_t = 1 << 2;
pub const RME_THD_FLAG_SCHED_PRIO: rme_ptr_t = 1 << 3;
pub const RME_THD_FLAG_SCHED_FREE: rme_ptr_t = 1 << 4;
pub const RME_THD_FLAG_SCHED_RCV: rme_ptr_t = 1 << 5;
pub const RME_THD_FLAG_XFER_SRC: rme_ptr_t = 1 << 6;
pub const RME_THD_FLAG_XFER_DST: rme_ptr_t = 1 << 7;
pub const RME_THD_FLAG_SWT: rme_ptr_t = 1 << 8;
pub const RME_THD_FLAG_ALL: rme_ptr_t = RME_THD_FLAG_EXEC_SET
    | RME_THD_FLAG_SCHED_CHILD
    | RME_THD_FLAG_SCHED_PARENT
    | RME_THD_FLAG_SCHED_PRIO
    | RME_THD_FLAG_SCHED_FREE
    | RME_THD_FLAG_SCHED_RCV
    | RME_THD_FLAG_XFER_SRC
    | RME_THD_FLAG_XFER_DST
    | RME_THD_FLAG_SWT;
/* End Define ****************************************************************/

/* Parameter packing *********************************************************/
/* On RV64V the word is 64 bits (RME_WORD_ORDER=6, RME_WORD_BYTE=8), so:
 *   D1/D0 : 32-bit halves
 *   Q3..Q0: 16-bit quarters
 *   O7..O0: 8-bit eighths
 * These mirror the user-side packing of the C benchmark (RME_PARAM_*). */
#[inline(always)]
pub const fn RME_Param_D1(X: rme_ptr_t) -> rme_ptr_t {
    (X & 0xFFFFFFFF) << 32
}
#[inline(always)]
pub const fn RME_Param_D0(X: rme_ptr_t) -> rme_ptr_t {
    X & 0xFFFFFFFF
}
#[inline(always)]
pub const fn RME_Param_Q3(X: rme_ptr_t) -> rme_ptr_t {
    (X & 0xFFFF) << 48
}
#[inline(always)]
pub const fn RME_Param_Q2(X: rme_ptr_t) -> rme_ptr_t {
    (X & 0xFFFF) << 32
}
#[inline(always)]
pub const fn RME_Param_Q1(X: rme_ptr_t) -> rme_ptr_t {
    (X & 0xFFFF) << 16
}
#[inline(always)]
pub const fn RME_Param_Q0(X: rme_ptr_t) -> rme_ptr_t {
    X & 0xFFFF
}
#[inline(always)]
pub const fn RME_Param_O7(X: rme_ptr_t) -> rme_ptr_t {
    (X & 0xFF) << 56
}
#[inline(always)]
pub const fn RME_Param_O6(X: rme_ptr_t) -> rme_ptr_t {
    (X & 0xFF) << 48
}
#[inline(always)]
pub const fn RME_Param_O5(X: rme_ptr_t) -> rme_ptr_t {
    (X & 0xFF) << 40
}
#[inline(always)]
pub const fn RME_Param_O4(X: rme_ptr_t) -> rme_ptr_t {
    (X & 0xFF) << 32
}
#[inline(always)]
pub const fn RME_Param_O3(X: rme_ptr_t) -> rme_ptr_t {
    (X & 0xFF) << 24
}
#[inline(always)]
pub const fn RME_Param_O2(X: rme_ptr_t) -> rme_ptr_t {
    (X & 0xFF) << 16
}
#[inline(always)]
pub const fn RME_Param_O1(X: rme_ptr_t) -> rme_ptr_t {
    (X & 0xFF) << 8
}
#[inline(always)]
pub const fn RME_Param_O0(X: rme_ptr_t) -> rme_ptr_t {
    X & 0xFF
}
/* End Parameter packing *****************************************************/

/* Type:RME_Ret ***************************************************************
Description : The three registers the kernel can write as return values.
              a0 is the primary status/value, a1 carries the INV_ACT callee
              return, and a2 carries DEBUG_EXC_GET's query value.
*****************************************************************************/
#[repr(C)]
#[derive(Clone, Copy)]
pub struct RME_Ret {
    pub A0: rme_ptr_t,
    pub A1: rme_ptr_t,
    pub A2: rme_ptr_t,
}
/* End Type:RME_Ret ***********************************************************/

impl RME_Ret {
    #[inline(always)]
    pub fn Status(&self) -> i64 {
        self.A0 as i64
    }

    #[inline(always)]
    pub fn Ok(&self) -> bool {
        self.Status() >= 0
    }
}

/* Function:RME_Svc_Raw ***************************************************
Description : The only ecall. It returns a0/a1/a2 together because most SVCs
              only use a0, INV_ACT uses a1, and DEBUG_EXC_GET uses a2.
              The Svc word is (Svc_Num << 32) | (Cid & 0xFFFFFFFF).
Input       : Svc_Num - The system call number (including Attr/PC bits).
              Cid - The capability ID (32 bits).
              Param0 - The first system call parameter (a1).
              Param1 - The second system call parameter (a2).
              Param2 - The third system call parameter (a3).
Output      : None.
Return      : RME_Ret - (a0, a1, a2).
*****************************************************************************/
#[inline(always)]
pub fn RME_Svc_Raw(
    Svc_Num: rme_ptr_t,
    Cid: rme_ptr_t,
    Param0: rme_ptr_t,
    Param1: rme_ptr_t,
    Param2: rme_ptr_t,
) -> RME_Ret {
    let mut A0: rme_ptr_t = (Svc_Num << 32) | (Cid & 0xFFFF_FFFF);
    let mut A1: rme_ptr_t = Param0;
    let mut A2: rme_ptr_t = Param1;

    unsafe {
        core::arch::asm!(
            "ecall",
            inlateout("a0") A0 => A0,
            inlateout("a1") A1 => A1,
            inlateout("a2") A2 => A2,
            in("a3") Param2,
            options(nostack),
        );
    }

    RME_Ret { A0, A1, A2 }
}
/* End Function:RME_Svc_Raw **********************************************/

/* Function:RME_Svc *******************************************************
Description : Scalar wrapper for the SVCs that only need the a0 return value.
Input       : Svc_Num - The system call number (including Attr/PC bits).
              Cid - The capability ID (in the caller's capability table).
              Param0 - The first system call parameter.
              Param1 - The second system call parameter.
              Param2 - The third system call parameter.
Output      : None.
Return      : rme_ptr_t - The kernel return value (in a0).
******************************************************************************/
#[inline(always)]
pub fn RME_Svc(
    Svc_Num: rme_ptr_t,
    Cid: rme_ptr_t,
    Param0: rme_ptr_t,
    Param1: rme_ptr_t,
    Param2: rme_ptr_t,
) -> rme_ptr_t {
    RME_Svc_Raw(Svc_Num, Cid, Param0, Param1, Param2).A0
}
/* End Function:RME_Svc **************************************************/

/* Function:RME_CID_Build ***************************************************
Description : Build a two-level capability ID: the major capability ID is the
              capability table slot that holds the target capability, and the
              minor one is the target capability's slot inside that table.
              This mirrors the kernel's RME_CID(X,Y) macro.
Input       : table - The capability table slot holding the target capability.
              slot - The target capability's slot inside that table.
Output      : None.
Return      : rme_ptr_t - The two-level capability ID.
*****************************************************************************/
#[inline(always)]
pub const fn RME_CID_Build(Table: rme_ptr_t, Slot: rme_ptr_t) -> rme_ptr_t {
    (Table << 16) | RME_CID_2L | Slot
}
/* End Function:RME_CID_Build ***********************************************/

/* Function:RME_Thd_Swt *******************************************************
Description : Switch to another thread. The target must have the same
              preemptive priority, be bound to the same core, and have time
              left. This is the RME equivalent of the Ammonite benchmark's
              RVM_Thd_Swt.
Input       : Cap_Thd - The capability to the target thread (two-level).
              Is_Yield - Non-zero to give up all of the caller's timeslices.
Output      : None.
Return      : rme_ptr_t - The kernel return value (0 on success).
*****************************************************************************/
#[inline(always)]
pub fn RME_Thd_Swt(Cap_Thd: rme_ptr_t, Is_Yield: rme_ptr_t) -> rme_ptr_t {
    RME_Svc(RME_SVC_THD_SWT, 0, Cap_Thd, Is_Yield, 0)
}
/* End Function:RME_Thd_Swt **************************************************/

/* Function:RME_Thd_Crt *******************************************************
Description : Create a thread. The capability is created in the capability
              table given by the major capability ID.
Input       : Cap_Cpt - The capability table to create the thread capability in.
              Cap_Kom - The kernel memory capability used for the thread object.
              Cap_Thd - The capability slot to create the thread capability in.
              Cap_Prc - The process the thread belongs to.
              Prio_Max - The maximum preemptive priority the thread may have.
              Raddr - The relative kernel memory address of the thread object.
Output      : None.
Return      : rme_ptr_t - The kernel return value (0 on success).
*****************************************************************************/
#[inline(always)]
pub fn RME_Thd_Crt(
    Cap_Cpt: rme_ptr_t,
    Cap_Kom: rme_ptr_t,
    Cap_Thd: rme_ptr_t,
    Cap_Prc: rme_ptr_t,
    Prio_Max: rme_ptr_t,
    Raddr: rme_ptr_t,
) -> rme_ptr_t {
    RME_Svc(
        RME_SVC_THD_CRT,
        Cap_Cpt,
        RME_Param_D1(Cap_Kom) | RME_Param_D0(Cap_Thd),
        RME_Param_D1(Cap_Prc) | RME_Param_D0(Prio_Max),
        Raddr,
    )
}
/* End Function:RME_Thd_Crt **************************************************/

/* Function:RME_Thd_Sched_Bind ***********************************************
Description : Bind a thread to the current processor and nominate its
              scheduler.
Input       : Cap_Thd - The capability to the thread (two-level).
              Cap_Sched - The capability to the scheduler thread (two-level).
              Cap_Sig - The capability to the scheduling notification endpoint.
              TID - The thread ID to assign.
              Prio - The preemptive priority of the thread.
              Haddr - The hypervisor address of the thread (unused here).
Output      : None.
Return      : rme_ptr_t - The kernel return value (0 on success).
*****************************************************************************/
#[inline(always)]
pub fn RME_Thd_Sched_Bind(
    Cap_Thd: rme_ptr_t,
    Cap_Sched: rme_ptr_t,
    Cap_Sig: rme_ptr_t,
    TID: rme_ptr_t,
    Prio: rme_ptr_t,
    Haddr: rme_ptr_t,
) -> rme_ptr_t {
    RME_Svc(
        RME_SVC_THD_SCHED_BIND,
        Cap_Thd,
        RME_Param_D1(Cap_Sched) | RME_Param_D0(Cap_Sig),
        RME_Param_D1(TID) | RME_Param_D0(Prio),
        Haddr,
    )
}
/* End Function:RME_Thd_Sched_Bind *******************************************/

/* Function:RME_Thd_Exec_Set *************************************************
Description : Set a thread's entry point, stack pointer and initial argument.
              The argument lands in a0, which is how the benchmark tells the
              freshly started helper thread which role it has to play.
Input       : Cap_Thd - The capability to the thread (two-level).
              Entry - The entry point address.
              Stack - The stack pointer.
              Param - The argument passed in a0.
Output      : None.
Return      : rme_ptr_t - The kernel return value (0 on success).
*****************************************************************************/
#[inline(always)]
pub fn RME_Thd_Exec_Set(
    Cap_Thd: rme_ptr_t,
    Entry: rme_ptr_t,
    Stack: rme_ptr_t,
    Param: rme_ptr_t,
) -> rme_ptr_t {
    RME_Svc(RME_SVC_THD_EXEC_SET, Cap_Thd, Entry, Stack, Param)
}
/* End Function:RME_Thd_Exec_Set **********************************************/

/* Function:RME_Thd_Time_Xfer ************************************************
Description : Transfer timeslices from one thread to another.
Input       : Cap_Dst - The capability to the receiving thread (two-level).
              Cap_Src - The capability to the donating thread (two-level).
              Time - The number of timeslices to transfer.
Output      : None.
Return      : rme_ptr_t - The kernel return value (0 on success).
*****************************************************************************/
#[inline(always)]
pub fn RME_Thd_Time_Xfer(Cap_Dst: rme_ptr_t, Cap_Src: rme_ptr_t, Time: rme_ptr_t) -> rme_ptr_t {
    RME_Svc(RME_SVC_THD_TIME_XFER, 0, Cap_Dst, Cap_Src, Time)
}
/* End Function:RME_Thd_Time_Xfer ********************************************/

/* Function:RME_Thd_Sched_Rcv ************************************************
Description : Poll the scheduler notification queue of a thread. This is the
              user visible trace of the kernel's tick preemption: when a
              thread runs out of timeslices the kernel notifies its scheduler.
              The call returns immediately with RME_ERR_PTH_NOTIF when there
              is nothing to report.
Input       : Cap_Thd - The capability to the scheduler thread (two-level).
Output      : None.
Return      : rme_ptr_t - The kernel return value, or the notification on success.
*****************************************************************************/
#[inline(always)]
pub fn RME_Thd_Sched_Rcv(Cap_Thd: rme_ptr_t) -> rme_ptr_t {
    RME_Svc(RME_SVC_THD_SCHED_RCV, 0, Cap_Thd, 0, 0)
}
/* End Function:RME_Thd_Sched_Rcv ********************************************/

/* Function:RME_Sig_Crt *******************************************************
Description : Create a signal endpoint in a capability table.
Input       : Cap_Cpt - The capability table to create the endpoint in.
              Cap_Sig - The capability slot to create the endpoint in.
Output      : None.
Return      : rme_ptr_t - The kernel return value (0 on success).
*****************************************************************************/
#[inline(always)]
pub fn RME_Sig_Crt(Cap_Cpt: rme_ptr_t, Cap_Sig: rme_ptr_t) -> rme_ptr_t {
    RME_Svc(RME_SVC_SIG_CRT, Cap_Cpt, Cap_Sig, 0, 0)
}
/* End Function:RME_Sig_Crt ***************************************************/

/* Function:RME_Sig_Snd *******************************************************
Description : Send signals to a signal endpoint. When the receiver is blocked
              on the same core, the kernel makes it runnable again.
Input       : Cap_Sig - The capability to the signal endpoint (two-level).
              Number - The number of signals to send.
Output      : None.
Return      : rme_ptr_t - The kernel return value (0 on success).
*****************************************************************************/
#[inline(always)]
pub fn RME_Sig_Snd(Cap_Sig: rme_ptr_t, Number: rme_ptr_t) -> rme_ptr_t {
    RME_Svc(RME_SVC_SIG_SND, 0, Cap_Sig, Number, 0)
}
/* End Function:RME_Sig_Snd ***************************************************/

/* Function:RME_Sig_Rcv *******************************************************
Description : Receive from a signal endpoint.
Input       : Cap_Sig - The capability to the signal endpoint (two-level).
              Option - The receive option (RME_RCV_BM blocks, RME_RCV_NM does
                       not block).
Output      : None.
Return      : rme_ptr_t - The number of signals received, or an error code.
*****************************************************************************/
#[inline(always)]
pub fn RME_Sig_Rcv(Cap_Sig: rme_ptr_t, Option: rme_ptr_t) -> rme_ptr_t {
    RME_Svc(RME_SVC_SIG_RCV, 0, Cap_Sig, Option, 0)
}
/* End Function:RME_Sig_Rcv ***************************************************/

/* Function:RME_Inv_Crt *******************************************************
Description : Create an invocation stub bound to a target process.
Input       : Cap_Cpt - The capability table to create the stub capability in.
              Cap_Kom - The kernel memory capability for the stub object.
              Cap_Inv - The capability slot to create the stub capability in.
              Cap_Prc - The target process that the invocation enters.
              Raddr - The relative kernel memory address of the stub object.
Output      : None.
Return      : rme_ptr_t - The kernel return value (0 on success).
*****************************************************************************/
#[inline(always)]
pub fn RME_Inv_Crt(
    Cap_Cpt: rme_ptr_t,
    Cap_Kom: rme_ptr_t,
    Cap_Inv: rme_ptr_t,
    Cap_Prc: rme_ptr_t,
    Raddr: rme_ptr_t,
) -> rme_ptr_t {
    RME_Svc(
        RME_SVC_INV_CRT,
        Cap_Cpt,
        RME_Param_D1(Cap_Kom) | RME_Param_D0(Cap_Inv),
        Cap_Prc,
        Raddr,
    )
}
/* End Function:RME_Inv_Crt ***************************************************/

/* Function:RME_Inv_Set *******************************************************
Description : Set an invocation stub's entry point and stack.
Input       : Cap_Inv - The capability to the invocation stub (two-level).
              Entry - The entry point address.
              Stack - The stack pointer.
              Is_Exc_Ret - Non-zero to return from the invocation on a fault.
Output      : None.
Return      : rme_ptr_t - The kernel return value (0 on success).
*****************************************************************************/
#[inline(always)]
pub fn RME_Inv_Set(
    Cap_Inv: rme_ptr_t,
    Entry: rme_ptr_t,
    Stack: rme_ptr_t,
    Is_Exc_Ret: rme_ptr_t,
) -> rme_ptr_t {
    RME_Svc(
        RME_SVC_INV_SET,
        0,
        RME_Param_D0(Cap_Inv) | RME_Param_D1(Is_Exc_Ret),
        Entry,
        Stack,
    )
}
/* End Function:RME_Inv_Set ***************************************************/

/* The invocation entry/return path only restores PC and SP. The invocation
 * stub may clobber ra and s0-s11 because it exits through INV_RET instead of
 * running its function epilogue. The compiler cannot express s0/s1 as inline
 * asm clobbers (s0 is the frame pointer and s1 is reserved by LLVM), so this
 * tiny gate saves/restores ra and s0-s11 around the ecall. Caller-saved
 * registers are covered by the normal extern "C" call boundary.
 *
 * The gate returns two registers (a0/a1); the Rust wrapper assembles RME_Ret
 * with a2 = 0. RME_Ret itself is 24 bytes, so it is not returned directly
 * through the C ABI. */
#[repr(C)]
struct RME_Ret_A0A1 {
    A0: rme_ptr_t,
    A1: rme_ptr_t,
}

core::arch::global_asm!(
    r#"
    .section .text.RME_Inv_Act_asm, "ax", @progbits
    .align 2
    .global RME_Inv_Act_ASM
RME_Inv_Act_ASM:
    addi sp, sp, -112
    sd ra, 0(sp)
    sd s0, 8(sp)
    sd s1, 16(sp)
    sd s2, 24(sp)
    sd s3, 32(sp)
    sd s4, 40(sp)
    sd s5, 48(sp)
    sd s6, 56(sp)
    sd s7, 64(sp)
    sd s8, 72(sp)
    sd s9, 80(sp)
    sd s10, 88(sp)
    sd s11, 96(sp)
    mv a2, a1
    mv a1, a0
    li a0, 1
    slli a0, a0, 32
    li a3, 0
    ecall
    ld ra, 0(sp)
    ld s0, 8(sp)
    ld s1, 16(sp)
    ld s2, 24(sp)
    ld s3, 32(sp)
    ld s4, 40(sp)
    ld s5, 48(sp)
    ld s6, 56(sp)
    ld s7, 64(sp)
    ld s8, 72(sp)
    ld s9, 80(sp)
    ld s10, 88(sp)
    ld s11, 96(sp)
    addi sp, sp, 112
    ret
"#
);

unsafe extern "C" {
    fn RME_Inv_Act_ASM(Cap_Inv: rme_ptr_t, Param: rme_ptr_t) -> RME_Ret_A0A1;
}

/* Function:RME_Inv_Act *******************************************************
Description : Activate an invocation stub. a0 is the syscall status (0 or a
              non-negative success value, negative on failure), a1 is the
              callee's INV_RET value, and a2 is unused (zero).
Input       : Cap_Inv - The capability to the invocation stub (two-level).
              Param - The argument passed to the callee.
Output      : None.
Return      : RME_Ret - (a0, a1, 0).
*****************************************************************************/
#[inline(always)]
pub fn RME_Inv_Act(Cap_Inv: rme_ptr_t, Param: rme_ptr_t) -> RME_Ret {
    let Ret = unsafe { RME_Inv_Act_ASM(Cap_Inv, Param) };
    RME_Ret {
        A0: Ret.A0,
        A1: Ret.A1,
        A2: 0,
    }
}
/* End Function:RME_Inv_Act ***************************************************/

/* Function:RME_Inv_Ret *******************************************************
Description : Return from a synchronous invocation.
Input       : Retval - The value returned to the invoker.
Output      : None.
Return      : rme_ptr_t - The kernel return value (0 on success).
*****************************************************************************/
#[inline(always)]
pub fn RME_Inv_Ret(Retval: rme_ptr_t) -> rme_ptr_t {
    RME_Svc(RME_SVC_INV_RET, 0, Retval, 0, 0)
}
/* End Function:RME_Inv_Ret ***************************************************/

/* Function:RME_Cpt_Crt *******************************************************
Description : Create a capability table.
Input       : Cap_Cpt - The capability table to create the new table in.
              Cap_Kom - The kernel memory capability for the table object.
              Cap_Crt - The capability slot to create the new table in.
              Raddr - The relative kernel memory address of the table object.
              Entry_Num - The number of entries in the new table.
Output      : None.
Return      : rme_ptr_t - The kernel return value (0 on success).
*****************************************************************************/
#[inline(always)]
pub fn RME_Cpt_Crt(
    Cap_Cpt: rme_ptr_t,
    Cap_Kom: rme_ptr_t,
    Cap_Crt: rme_ptr_t,
    Raddr: rme_ptr_t,
    Entry_Num: rme_ptr_t,
) -> rme_ptr_t {
    RME_Svc(
        RME_SVC_CPT_CRT,
        Cap_Cpt,
        RME_Param_D1(Cap_Kom) | RME_Param_D0(Cap_Crt),
        Raddr,
        Entry_Num,
    )
}
/* End Function:RME_Cpt_Crt ***************************************************/

/* Function:RME_Cpt_Add *******************************************************
Description : Delegate a capability from one capability table into another.
Input       : Cap_Cpt_Dst - The destination capability table (two-level).
              Cap_Dst - The destination slot inside that table.
              Cap_Cpt_Src - The source capability table (two-level).
              Cap_Src - The source slot inside that table.
              Flag - The subset of the source's rights to delegate.
Output      : None.
Return      : rme_ptr_t - The kernel return value (0 on success).
*****************************************************************************/
#[inline(always)]
pub fn RME_Cpt_Add(
    Cap_Cpt_Dst: rme_ptr_t,
    Cap_Dst: rme_ptr_t,
    Cap_Cpt_Src: rme_ptr_t,
    Cap_Src: rme_ptr_t,
    Flag: rme_ptr_t,
) -> rme_ptr_t {
    RME_Svc(
        RME_SVC_CPT_ADD,
        0,
        RME_Param_D1(Cap_Cpt_Dst) | RME_Param_D0(Cap_Dst),
        RME_Param_D1(Cap_Cpt_Src) | RME_Param_D0(Cap_Src),
        Flag,
    )
}
/* End Function:RME_Cpt_Add ***************************************************/

/* Function:RME_Prc_Crt *******************************************************
Description : Create a process.
Input       : Cap_Cpt - The capability table to create the process in.
              Cap_Prc - The capability slot to create the process in.
              Cap_Cpt_New - The capability table owned by the new process.
              Cap_Pgt - The root page table owned by the new process.
Output      : None.
Return      : rme_ptr_t - The kernel return value (0 on success).
*****************************************************************************/
#[inline(always)]
pub fn RME_Prc_Crt(
    Cap_Cpt: rme_ptr_t,
    Cap_Prc: rme_ptr_t,
    Cap_Cpt_New: rme_ptr_t,
    Cap_Pgt: rme_ptr_t,
) -> rme_ptr_t {
    RME_Svc(RME_SVC_PRC_CRT, Cap_Cpt, Cap_Prc, Cap_Cpt_New, Cap_Pgt)
}
/* End Function:RME_Prc_Crt ***************************************************/

/* Function:RME_Thd_Sched_Free ***********************************************
Description : Free a thread from its current processor binding.
Input       : Cap_Thd - The capability to the target thread.
Output      : None.
Return      : rme_ptr_t - The kernel return value (0 on success).
*****************************************************************************/
#[inline(always)]
pub fn RME_Thd_Sched_Free(Cap_Thd: rme_ptr_t) -> rme_ptr_t {
    RME_Svc(RME_SVC_THD_SCHED_FREE, 0, Cap_Thd, 0, 0)
}
/* End Function:RME_Thd_Sched_Free *******************************************/

/* Function:RME_Thd_Sched_Prio ***********************************************
Description : Change one thread's Priority. This is the one-thread form of
              THD_SCHED_PRIO (Number = 1).
Input       : Cap_Thd - The capability to the target thread.
              Prio - The new preemptive priority.
Output      : None.
Return      : rme_ptr_t - The kernel return value (0 on success).
*****************************************************************************/
#[inline(always)]
pub fn RME_Thd_Sched_Prio(Cap_Thd: rme_ptr_t, Prio: rme_ptr_t) -> rme_ptr_t {
    RME_Svc(
        RME_SVC_THD_SCHED_PRIO,
        1,
        RME_Param_D0(Cap_Thd),
        0,
        RME_Param_Q0(Prio),
    )
}
/* End Function:RME_Thd_Sched_Prio *******************************************/

/* Function:RME_Cpt_Del ******************************************************
Description : Delete an empty capability table. The target capability must be
              frozen first.
Input       : Cap_Cpt - The capability table that contains the target cap.
              Cap_Del - The target capability slot.
Output      : None.
Return      : rme_ptr_t - The kernel return value (0 on success).
*****************************************************************************/
#[inline(always)]
pub fn RME_Cpt_Del(Cap_Cpt: rme_ptr_t, Cap_Del: rme_ptr_t) -> rme_ptr_t {
    RME_Svc(RME_SVC_CPT_DEL, Cap_Cpt, Cap_Del, 0, 0)
}
/* End Function:RME_Cpt_Del **************************************************/

/* Function:RME_Cpt_Frz ******************************************************
Description : Freeze a capability. Freeze is irreversible; deletion/removal
              paths require this state first.
Input       : Cap_Cpt - The capability table that contains the target cap.
              Cap_Frz - The target capability slot.
Output      : None.
Return      : rme_ptr_t - The kernel return value (0 on success).
*****************************************************************************/
#[inline(always)]
pub fn RME_Cpt_Frz(Cap_Cpt: rme_ptr_t, Cap_Frz: rme_ptr_t) -> rme_ptr_t {
    RME_Svc(RME_SVC_CPT_FRZ, Cap_Cpt, Cap_Frz, 0, 0)
}
/* End Function:RME_Cpt_Frz **************************************************/

/* Function:RME_Cpt_Rem ******************************************************
Description : Remove a delegated (leaf) capability from a table. The target
              capability must be frozen first.
Input       : Cap_Cpt - The capability table that contains the target cap.
              Cap_Rem - The target capability slot.
Output      : None.
Return      : rme_ptr_t - The kernel return value (0 on success).
*****************************************************************************/
#[inline(always)]
pub fn RME_Cpt_Rem(Cap_Cpt: rme_ptr_t, Cap_Rem: rme_ptr_t) -> rme_ptr_t {
    RME_Svc(RME_SVC_CPT_REM, Cap_Cpt, Cap_Rem, 0, 0)
}
/* End Function:RME_Cpt_Rem **************************************************/

/* Function:RME_Pgt_Crt ******************************************************
Description : Create a page table. Num_Order is an exponent (512 entries = 9),
              and is carried in the high bits of the service number.
Input       : Cap_Cpt - The capability table to create the cap in.
              Cap_Kom - The kernel memory capability.
              Cap_Pgt - The capability slot for the new table.
              Raddr - The relative KOM address (4KB aligned).
              Base - The virtual base the table covers.
              Is_Top - Non-zero for the top-level table (1G entries only).
              Size_Order - The page size order (4K/2M/1G).
Output      : None.
Return      : rme_ptr_t - The kernel return value (0 on success).
*****************************************************************************/
#[inline(always)]
pub fn RME_Pgt_Crt(
    Cap_Cpt: rme_ptr_t,
    Cap_Kom: rme_ptr_t,
    Cap_Pgt: rme_ptr_t,
    Raddr: rme_ptr_t,
    Base: rme_ptr_t,
    Is_Top: rme_ptr_t,
    Size_Order: rme_ptr_t,
) -> rme_ptr_t {
    let Svc_Num: rme_ptr_t = RME_SVC_PGT_CRT | (RME_PGT_NUM_512 << 16);
    let Param0: rme_ptr_t =
        RME_Param_D1(Cap_Kom) | RME_Param_Q1(Cap_Pgt) | RME_Param_Q0(Size_Order);
    RME_Svc(Svc_Num, Cap_Cpt, Param0, Raddr, Base | Is_Top)
}
/* End Function:RME_Pgt_Crt **************************************************/

/* Function:RME_Pgt_Del ******************************************************
Description : Delete a page table. The target capability must be frozen first.
Input       : Cap_Cpt - The capability table that contains the target cap.
              Cap_Pgt - The target capability slot.
Output      : None.
Return      : rme_ptr_t - The kernel return value (0 on success).
*****************************************************************************/
#[inline(always)]
pub fn RME_Pgt_Del(Cap_Cpt: rme_ptr_t, Cap_Pgt: rme_ptr_t) -> rme_ptr_t {
    RME_Svc(RME_SVC_PGT_DEL, Cap_Cpt, Cap_Pgt, 0, 0)
}
/* End Function:RME_Pgt_Del **************************************************/

/* Function:RME_Pgt_Add ******************************************************
Description : Map a page from a source table into a destination table.
Input       : Cap_Dst - The destination page table capability.
              Pos_Dst - The destination position.
              Cap_Src - The source page table capability.
              Pos_Src - The source position.
              Flag_Dst - The destination page permissions (must be a subset of
                         the source page's flags; use ALL_DYN for init pages).
Output      : None.
Return      : rme_ptr_t - The kernel return value (0 on success).
*****************************************************************************/
#[inline(always)]
pub fn RME_Pgt_Add(
    Cap_Dst: rme_ptr_t,
    Pos_Dst: rme_ptr_t,
    Cap_Src: rme_ptr_t,
    Pos_Src: rme_ptr_t,
    Flag_Dst: rme_ptr_t,
) -> rme_ptr_t {
    RME_Svc(
        RME_SVC_PGT_ADD,
        Flag_Dst,
        RME_Param_D1(Cap_Dst) | RME_Param_D0(Pos_Dst),
        RME_Param_D1(Cap_Src) | RME_Param_D0(Pos_Src),
        0,
    )
}
/* End Function:RME_Pgt_Add **************************************************/

/* Function:RME_Pgt_Rem ******************************************************
Description : Unmap a page from a page table.
Input       : Cap_Pgt - The page table capability.
              Pos - The position to unmap.
Output      : None.
Return      : rme_ptr_t - The kernel return value (0 on success).
*****************************************************************************/
#[inline(always)]
pub fn RME_Pgt_Rem(Cap_Pgt: rme_ptr_t, Pos: rme_ptr_t) -> rme_ptr_t {
    RME_Svc(RME_SVC_PGT_REM, 0, Cap_Pgt, Pos, 0)
}
/* End Function:RME_Pgt_Rem **************************************************/

/* Function:RME_Pgt_Con ******************************************************
Description : Map a child page table into a parent page table.
Input       : Cap_Parent - The parent page table capability.
              Pos - The parent position.
              Cap_Child - The child page table capability.
              Flag_Child - The child mapping flags.
Output      : None.
Return      : rme_ptr_t - The kernel return value (0 on success).
*****************************************************************************/
#[inline(always)]
pub fn RME_Pgt_Con(
    Cap_Parent: rme_ptr_t,
    Pos: rme_ptr_t,
    Cap_Child: rme_ptr_t,
    Flag_Child: rme_ptr_t,
) -> rme_ptr_t {
    RME_Svc(
        RME_SVC_PGT_CON,
        0,
        RME_Param_D1(Cap_Parent) | RME_Param_D0(Cap_Child),
        Pos,
        Flag_Child,
    )
}
/* End Function:RME_Pgt_Con **************************************************/

/* Function:RME_Pgt_Des ******************************************************
Description : Unmap a child page table from a parent page table.
Input       : Cap_Parent - The parent page table capability.
              Pos - The parent position.
              Cap_Child - The child page table capability.
Output      : None.
Return      : rme_ptr_t - The kernel return value (0 on success).
*****************************************************************************/
#[inline(always)]
pub fn RME_Pgt_Des(Cap_Parent: rme_ptr_t, Pos: rme_ptr_t, Cap_Child: rme_ptr_t) -> rme_ptr_t {
    RME_Svc(RME_SVC_PGT_DES, 0, Cap_Parent, Pos, Cap_Child)
}
/* End Function:RME_Pgt_Des **************************************************/

/* Function:RME_Prc_Del ******************************************************
Description : Delete a process. The target capability must be frozen first.
Input       : Cap_Cpt - The capability table that contains the target cap.
              Cap_Prc - The target process capability slot.
Output      : None.
Return      : rme_ptr_t - The kernel return value (0 on success).
*****************************************************************************/
#[inline(always)]
pub fn RME_Prc_Del(Cap_Cpt: rme_ptr_t, Cap_Prc: rme_ptr_t) -> rme_ptr_t {
    RME_Svc(RME_SVC_PRC_DEL, Cap_Cpt, Cap_Prc, 0, 0)
}
/* End Function:RME_Prc_Del **************************************************/

/* Function:RME_Prc_Cpt ******************************************************
Description : Change a process's capability table.
Input       : Cap_Prc - The process capability.
              Cap_Cpt - The new capability table capability.
Output      : None.
Return      : rme_ptr_t - The kernel return value (0 on success).
*****************************************************************************/
#[inline(always)]
pub fn RME_Prc_Cpt(Cap_Prc: rme_ptr_t, Cap_Cpt: rme_ptr_t) -> rme_ptr_t {
    RME_Svc(RME_SVC_PRC_CPT, 0, Cap_Prc, Cap_Cpt, 0)
}
/* End Function:RME_Prc_Cpt **************************************************/

/* Function:RME_Prc_Pgt ******************************************************
Description : Change a process's page table.
Input       : Cap_Prc - The process capability.
              Cap_Pgt - The new root page table capability.
Output      : None.
Return      : rme_ptr_t - The kernel return value (0 on success).
*****************************************************************************/
#[inline(always)]
pub fn RME_Prc_Pgt(Cap_Prc: rme_ptr_t, Cap_Pgt: rme_ptr_t) -> rme_ptr_t {
    RME_Svc(RME_SVC_PRC_PGT, 0, Cap_Prc, Cap_Pgt, 0)
}
/* End Function:RME_Prc_Pgt **************************************************/

/* Function:RME_Thd_Del ******************************************************
Description : Delete a thread. The target capability must be frozen and the
              thread must be unbound.
Input       : Cap_Cpt - The capability table that contains the target cap.
              Cap_Thd - The target thread capability slot.
Output      : None.
Return      : rme_ptr_t - The kernel return value (0 on success).
*****************************************************************************/
#[inline(always)]
pub fn RME_Thd_Del(Cap_Cpt: rme_ptr_t, Cap_Thd: rme_ptr_t) -> rme_ptr_t {
    RME_Svc(RME_SVC_THD_DEL, Cap_Cpt, Cap_Thd, 0, 0)
}
/* End Function:RME_Thd_Del **************************************************/

/* Function:RME_Sig_Del ******************************************************
Description : Delete a signal endpoint. The target capability must be frozen
              and no thread may be blocked on it.
Input       : Cap_Cpt - The capability table that contains the target cap.
              Cap_Sig - The target signal capability slot.
Output      : None.
Return      : rme_ptr_t - The kernel return value (0 on success).
*****************************************************************************/
#[inline(always)]
pub fn RME_Sig_Del(Cap_Cpt: rme_ptr_t, Cap_Sig: rme_ptr_t) -> rme_ptr_t {
    RME_Svc(RME_SVC_SIG_DEL, Cap_Cpt, Cap_Sig, 0, 0)
}
/* End Function:RME_Sig_Del **************************************************/

/* Function:RME_Inv_Del ******************************************************
Description : Delete an invocation stub. The target capability must be frozen
              and the stub must not be active.
Input       : Cap_Cpt - The capability table that contains the target cap.
              Cap_Inv - The target invocation capability slot.
Output      : None.
Return      : rme_ptr_t - The kernel return value (0 on success).
*****************************************************************************/
#[inline(always)]
pub fn RME_Inv_Del(Cap_Cpt: rme_ptr_t, Cap_Inv: rme_ptr_t) -> rme_ptr_t {
    RME_Svc(RME_SVC_INV_DEL, Cap_Cpt, Cap_Inv, 0, 0)
}
/* End Function:RME_Inv_Del **************************************************/

/* Function:RME_Kfn **********************************************************
Description : Invoke a kernel function.
Input       : Cap_Kfn - The kernel function capability.
              Func_ID - The function ID.
              Sub_ID - The subfunction ID.
              Param1 - The first function parameter.
              Param2 - The second function parameter.
Output      : None.
Return      : rme_ptr_t - The kernel return value.
*****************************************************************************/
#[inline(always)]
pub fn RME_Kfn(
    Cap_Kfn: rme_ptr_t,
    Func_ID: rme_ptr_t,
    Sub_ID: rme_ptr_t,
    Param1: rme_ptr_t,
    Param2: rme_ptr_t,
) -> rme_ptr_t {
    RME_Kfn_Raw(Cap_Kfn, Func_ID, Sub_ID, Param1, Param2).A0
}
/* End Function:RME_Kfn ******************************************************/

/* Function:RME_Kfn_Raw *******************************************************
Description : KFN with the full a0/a1/a2 return set. DEBUG_REG_MOD and
              DEBUG_INV_MOD return their value in a1; DEBUG_EXC_GET returns it
              in a2.
*****************************************************************************/
#[inline(always)]
pub fn RME_Kfn_Raw(
    Cap_Kfn: rme_ptr_t,
    Func_ID: rme_ptr_t,
    Sub_ID: rme_ptr_t,
    Param1: rme_ptr_t,
    Param2: rme_ptr_t,
) -> RME_Ret {
    RME_Svc_Raw(
        RME_SVC_KFN,
        Cap_Kfn,
        RME_Param_D0(Func_ID) | RME_Param_D1(Sub_ID),
        Param1,
        Param2,
    )
}
/* End Function:RME_Kfn_Raw **************************************************/

/* End Of File ***************************************************************/

/* Copyright (C) Evo-Devo Instrum. All rights reserved ***********************/
