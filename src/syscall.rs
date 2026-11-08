/******************************************************************************
Filename    : syscall.rs
Author      : yhy
Date        : 13/10/2026
Licence     : The Unlicense; see LICENSE for details.
Description : User-mode system call interface for the RV64V port on QEMU
              virt, mirroring the C rme_syscall.h. Mirrors the kernel's
              syscall ABI and constants (Include/rme.h) without depending on
              kernel headers, so any user program can include it standalone.
              Syscall ABI on RV64V (64-bit split):
                a0 = (Svc << 32) | Cid
                a1 = Param0, a2 = Param1, a3 = Param2
                ecall                 (cause 8, delegated to S-mode by OpenSBI)
                return value comes back in a0; some calls also use a1
              rme_syscall_raw() is the ONLY ecall in a user program - everything
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
pub type RmePtr = u64;
/* End Type ******************************************************************/

/* Define ********************************************************************/
/* System call numbers (mirror of Include/rme.h) *****************************/
pub const RME_SVC_INV_RET: RmePtr = 0;
pub const RME_SVC_INV_ACT: RmePtr = 1;
pub const RME_SVC_SIG_SND: RmePtr = 2;
pub const RME_SVC_SIG_RCV: RmePtr = 3;
pub const RME_SVC_KFN: RmePtr = 4;
pub const RME_SVC_THD_SCHED_FREE: RmePtr = 5;
pub const RME_SVC_THD_EXEC_SET: RmePtr = 6;
pub const RME_SVC_THD_SCHED_PRIO: RmePtr = 7;
pub const RME_SVC_THD_TIME_XFER: RmePtr = 8;
pub const RME_SVC_THD_SWT: RmePtr = 9;
pub const RME_SVC_CPT_CRT: RmePtr = 10;
pub const RME_SVC_CPT_DEL: RmePtr = 11;
pub const RME_SVC_CPT_FRZ: RmePtr = 12;
pub const RME_SVC_CPT_ADD: RmePtr = 13;
pub const RME_SVC_CPT_REM: RmePtr = 14;
pub const RME_SVC_PGT_CRT: RmePtr = 15;
pub const RME_SVC_PGT_DEL: RmePtr = 16;
pub const RME_SVC_PGT_ADD: RmePtr = 17;
pub const RME_SVC_PGT_REM: RmePtr = 18;
pub const RME_SVC_PGT_CON: RmePtr = 19;
pub const RME_SVC_PGT_DES: RmePtr = 20;
pub const RME_SVC_PRC_CRT: RmePtr = 21;
pub const RME_SVC_PRC_DEL: RmePtr = 22;
pub const RME_SVC_PRC_CPT: RmePtr = 23;
pub const RME_SVC_PRC_PGT: RmePtr = 24;
pub const RME_SVC_THD_CRT: RmePtr = 25;
pub const RME_SVC_THD_DEL: RmePtr = 26;
pub const RME_SVC_THD_SCHED_BIND: RmePtr = 27;
pub const RME_SVC_THD_SCHED_RCV: RmePtr = 28;
pub const RME_SVC_SIG_CRT: RmePtr = 29;
pub const RME_SVC_SIG_DEL: RmePtr = 30;
pub const RME_SVC_INV_CRT: RmePtr = 31;
pub const RME_SVC_INV_DEL: RmePtr = 32;
pub const RME_SVC_INV_SET: RmePtr = 33;

/* Kernel function IDs (mirror of Include/rme.h) *****************************/
pub const RME_KFN_PGT_CACHE_CLR: RmePtr = 0xF000;
pub const RME_KFN_PGT_LINE_CLR: RmePtr = 0xF001;
pub const RME_KFN_PGT_ASID_SET: RmePtr = 0xF002;
pub const RME_KFN_PGT_TLB_LOCK: RmePtr = 0xF003;
pub const RME_KFN_PGT_ENTRY_MOD: RmePtr = 0xF004;
pub const RME_KFN_INT_LOCAL_MOD: RmePtr = 0xF100;
pub const RME_KFN_INT_GLOBAL_MOD: RmePtr = 0xF101;
pub const RME_KFN_INT_LOCAL_TRIG: RmePtr = 0xF102;
pub const RME_KFN_EVT_LOCAL_TRIG: RmePtr = 0xF103;
pub const RME_KFN_CACHE_MOD: RmePtr = 0xF200;
pub const RME_KFN_CACHE_CONFIG: RmePtr = 0xF201;
pub const RME_KFN_CACHE_MAINT: RmePtr = 0xF202;
pub const RME_KFN_CACHE_LOCK: RmePtr = 0xF203;
pub const RME_KFN_PRFTH_MOD: RmePtr = 0xF204;
pub const RME_KFN_HPNP_PCPU_MOD: RmePtr = 0xF300;
pub const RME_KFN_HPNP_LCPU_MOD: RmePtr = 0xF301;
pub const RME_KFN_HPNP_PMEM_MOD: RmePtr = 0xF302;
pub const RME_KFN_IDLE_SLEEP: RmePtr = 0xF400;
pub const RME_KFN_SYS_REBOOT: RmePtr = 0xF401;
pub const RME_KFN_SYS_SHDN: RmePtr = 0xF402;
pub const RME_KFN_VOLT_MOD: RmePtr = 0xF403;
pub const RME_KFN_FREQ_MOD: RmePtr = 0xF404;
pub const RME_KFN_PMOD_MOD: RmePtr = 0xF405;
pub const RME_KFN_SAFETY_MOD: RmePtr = 0xF406;
pub const RME_KFN_PERF_CPU_FUNC: RmePtr = 0xF500;
pub const RME_KFN_PERF_MON_MOD: RmePtr = 0xF501;
pub const RME_KFN_PERF_CNT_MOD: RmePtr = 0xF502;
pub const RME_KFN_PERF_CYCLE_MOD: RmePtr = 0xF503;
pub const RME_KFN_PERF_DATA_MOD: RmePtr = 0xF504;
pub const RME_KFN_PERF_PHYS_MOD: RmePtr = 0xF505;
pub const RME_KFN_PERF_CUMUL_MOD: RmePtr = 0xF506;
pub const RME_KFN_VM_CRT: RmePtr = 0xF600;
pub const RME_KFN_VM_DEL: RmePtr = 0xF601;
pub const RME_KFN_VM_PGT: RmePtr = 0xF602;
pub const RME_KFN_VM_MOD: RmePtr = 0xF603;
pub const RME_KFN_VCPU_CRT: RmePtr = 0xF604;
pub const RME_KFN_VCPU_BIND: RmePtr = 0xF605;
pub const RME_KFN_VCPU_FREE: RmePtr = 0xF606;
pub const RME_KFN_VCPU_DEL: RmePtr = 0xF607;
pub const RME_KFN_VCPU_MOD: RmePtr = 0xF608;
pub const RME_KFN_VCPU_RUN: RmePtr = 0xF609;
pub const RME_KFN_ECLV_CRT: RmePtr = 0xF700;
pub const RME_KFN_ECLV_MOD: RmePtr = 0xF701;
pub const RME_KFN_ECLV_DEL: RmePtr = 0xF702;
pub const RME_KFN_ECLV_ACT: RmePtr = 0xF703;
pub const RME_KFN_ECLV_RET: RmePtr = 0xF704;
pub const RME_KFN_DEBUG_PRINT: RmePtr = 0xF800;
pub const RME_KFN_DEBUG_REG_MOD: RmePtr = 0xF801;
pub const RME_KFN_DEBUG_INV_MOD: RmePtr = 0xF802;
pub const RME_KFN_DEBUG_EXC_GET: RmePtr = 0xF803;
pub const RME_KFN_DEBUG_MODE_MOD: RmePtr = 0xF804;
pub const RME_KFN_DEBUG_IBP_MOD: RmePtr = 0xF805;
pub const RME_KFN_DEBUG_DBP_MOD: RmePtr = 0xF806;

/* RV64V cache maintenance KFN sub-IDs (mirror of rme_platform_rv64v.h) ******/
pub const RME_RV64V_KFN_CACHE_ICACHE: RmePtr = 0;
pub const RME_RV64V_KFN_CACHE_DCACHE: RmePtr = 1;
pub const RME_RV64V_KFN_CACHE_CLEAN_ALL: RmePtr = 0;
pub const RME_RV64V_KFN_CACHE_CLEAN_ADDR: RmePtr = 1;
pub const RME_RV64V_KFN_CACHE_INV_ALL: RmePtr = 5;
pub const RME_RV64V_KFN_CACHE_INV_ADDR: RmePtr = 6;
pub const RME_RV64V_KFN_CACHE_CLEAN_INV_ALL: RmePtr = 10;
pub const RME_RV64V_KFN_CACHE_CLEAN_INV_ADDR: RmePtr = 11;
pub const RME_RV64V_CACHE_BLOCK_SIZE: RmePtr = 64;

/* RV64V page-table entry query KFN sub-IDs **********************************/
pub const RME_RV64V_KFN_PGT_ENTRY_MOD_FLAG_GET: RmePtr = 0;
pub const RME_RV64V_KFN_PGT_ENTRY_MOD_SZORD_GET: RmePtr = 1;
pub const RME_RV64V_KFN_PGT_ENTRY_MOD_NMORD_GET: RmePtr = 2;

/* RV64V debug KFN sub-IDs ***************************************************/
pub const RME_RV64V_KFN_DEBUG_REG_MOD_GET: RmePtr = 0;
pub const RME_RV64V_KFN_DEBUG_REG_MOD_SET: RmePtr = 1 << 16;
pub const RME_RV64V_KFN_DEBUG_REG_MOD_X10_A0: RmePtr = 11;

pub const RME_RV64V_KFN_DEBUG_INV_MOD_SP_GET: RmePtr = 0;
pub const RME_RV64V_KFN_DEBUG_INV_MOD_SP_SET: RmePtr = 1;
pub const RME_RV64V_KFN_DEBUG_INV_MOD_PC_GET: RmePtr = 2;
pub const RME_RV64V_KFN_DEBUG_INV_MOD_PC_SET: RmePtr = 3;

pub const RME_RV64V_KFN_DEBUG_EXC_CAUSE_GET: RmePtr = 0;
pub const RME_RV64V_KFN_DEBUG_EXC_ADDR_GET: RmePtr = 1;
pub const RME_RV64V_KFN_DEBUG_EXC_VALUE_GET: RmePtr = 2;

/* KFN capability range: high limit in [63:32], low limit in [31:0]. The boot
 * KFN capability allows every function ID, so delegating it uses the same
 * full range. */
pub const RME_KFN_FLAG_FULL_RANGE: RmePtr = 0xFFFF_FFFF_0000_0000;

/* Signal endpoint flags (mirror of Include/rme.h) *****************************/
pub const RME_SIG_FLAG_SND: RmePtr = 1 << 0;
pub const RME_SIG_FLAG_RCV_BS: RmePtr = 1 << 1;
pub const RME_SIG_FLAG_RCV_BM: RmePtr = 1 << 2;
pub const RME_SIG_FLAG_RCV_NS: RmePtr = 1 << 3;
pub const RME_SIG_FLAG_RCV_NM: RmePtr = 1 << 4;
pub const RME_SIG_FLAG_SCHED: RmePtr = 1 << 5;
pub const RME_SIG_FLAG_ALL: RmePtr = RME_SIG_FLAG_SND
    | RME_SIG_FLAG_RCV_BS
    | RME_SIG_FLAG_RCV_BM
    | RME_SIG_FLAG_RCV_NS
    | RME_SIG_FLAG_RCV_NM
    | RME_SIG_FLAG_SCHED;

/* Receive options (mirror of Include/rme.h) **********************************/
pub const RME_RCV_BS: RmePtr = 0;
pub const RME_RCV_BM: RmePtr = 1;
pub const RME_RCV_NS: RmePtr = 2;
pub const RME_RCV_NM: RmePtr = 3;

/* Invocation stub flags (mirror of Include/rme.h) ****************************/
pub const RME_INV_FLAG_SET: RmePtr = 1 << 0;
pub const RME_INV_FLAG_ACT: RmePtr = 1 << 1;
pub const RME_INV_FLAG_ALL: RmePtr = RME_INV_FLAG_SET | RME_INV_FLAG_ACT;

/* Capability table flags (mirror of Include/rme.h) ***************************/
pub const RME_CPT_FLAG_CRT: RmePtr = 1 << 0;
pub const RME_CPT_FLAG_DEL: RmePtr = 1 << 1;
pub const RME_CPT_FLAG_FRZ: RmePtr = 1 << 2;
pub const RME_CPT_FLAG_ADD_SRC: RmePtr = 1 << 3;
pub const RME_CPT_FLAG_ADD_DST: RmePtr = 1 << 4;
pub const RME_CPT_FLAG_REM: RmePtr = 1 << 5;
pub const RME_CPT_FLAG_PRC_CRT: RmePtr = 1 << 6;
pub const RME_CPT_FLAG_PRC_CPT: RmePtr = 1 << 7;
pub const RME_CPT_FLAG_ALL: RmePtr = RME_CPT_FLAG_CRT
    | RME_CPT_FLAG_DEL
    | RME_CPT_FLAG_FRZ
    | RME_CPT_FLAG_ADD_SRC
    | RME_CPT_FLAG_ADD_DST
    | RME_CPT_FLAG_REM
    | RME_CPT_FLAG_PRC_CRT
    | RME_CPT_FLAG_PRC_CPT;

/* Process flags (mirror of Include/rme.h) ************************************/
pub const RME_PRC_FLAG_INV: RmePtr = 1 << 0;
pub const RME_PRC_FLAG_THD: RmePtr = 1 << 1;
pub const RME_PRC_FLAG_CPT: RmePtr = 1 << 2;
pub const RME_PRC_FLAG_PGT: RmePtr = 1 << 3;
pub const RME_PRC_FLAG_ALL: RmePtr =
    RME_PRC_FLAG_INV | RME_PRC_FLAG_THD | RME_PRC_FLAG_CPT | RME_PRC_FLAG_PGT;

/* Kernel memory flags (mirror of Include/rme.h) ******************************/
pub const RME_KOM_FLAG_CPT: RmePtr = 1 << 0;
pub const RME_KOM_FLAG_PGT: RmePtr = 1 << 1;
pub const RME_KOM_FLAG_THD: RmePtr = 1 << 2;
pub const RME_KOM_FLAG_INV: RmePtr = 1 << 3;
pub const RME_KOM_FLAG_ALL: RmePtr =
    RME_KOM_FLAG_CPT | RME_KOM_FLAG_PGT | RME_KOM_FLAG_THD | RME_KOM_FLAG_INV;

/* Boot capability slots (mirror of rme_platform_rv64v.h) ********************/
pub const RME_BOOT_INIT_CPT: RmePtr = 0;
pub const RME_BOOT_INIT_PGT: RmePtr = 1;
pub const RME_BOOT_INIT_PRC: RmePtr = 2;
/* The per-CPU initial-thread capability table - one initial thread per hart.
 * The init thread of CPU0 therefore lives in slot 0 of this table, which
 * makes its capability ID two-level. */
pub const RME_BOOT_TBL_THD: RmePtr = 3;
pub const RME_BOOT_INIT_KFN: RmePtr = 4;
pub const RME_BOOT_INIT_KOM: RmePtr = 5;
/* The per-CPU timer and general-interrupt endpoint tables */
pub const RME_BOOT_TBL_TIMER: RmePtr = 6;
pub const RME_BOOT_TBL_INT: RmePtr = 7;
pub const RME_BOOT_INIT_PGT_L1: RmePtr = 8;
pub const RME_BOOT_INIT_PGT_L2: RmePtr = 9;
/* Two-level capability encoding: (Table << 16) | 2L | Slot */
pub const RME_CID_2L: RmePtr = 1 << 15;
/* CPU0's initial thread: slot 0 of the per-CPU thread table */
pub const RME_BOOT_INIT_THD: RmePtr = (RME_BOOT_TBL_THD << 16) | RME_CID_2L;
/* The init thread's slot *inside* RME_BOOT_TBL_THD. CPT_ADD/REM address a
 * capability with one-level IDs, so they need this slot number and the table
 * slot, not the two-level RME_BOOT_INIT_THD. */
pub const RME_BOOT_INIT_THD_SLOT: RmePtr = 0;
/* The null capability ID - high bit of the D1 field */
pub const RME_CID_NULL: RmePtr = 0x80000000;

/* Page table flags (mirror of Include/rme.h) ********************************/
pub const RME_PGT_READ: RmePtr = 1 << 0;
pub const RME_PGT_WRITE: RmePtr = 1 << 1;
pub const RME_PGT_EXECUTE: RmePtr = 1 << 2;
pub const RME_PGT_CACHE: RmePtr = 1 << 3;
pub const RME_PGT_BUFFER: RmePtr = 1 << 4;
pub const RME_PGT_STATIC: RmePtr = 1 << 5;
pub const RME_PGT_ALL_DYN: RmePtr =
    RME_PGT_READ | RME_PGT_WRITE | RME_PGT_EXECUTE | RME_PGT_CACHE | RME_PGT_BUFFER;
pub const RME_PGT_ALL_PERM: RmePtr = RME_PGT_ALL_DYN | RME_PGT_STATIC;

/* Page table size/num orders (mirror of Include/rme.h) **********************/
pub const RME_PGT_SIZE_4K: RmePtr = 12;
pub const RME_PGT_SIZE_2M: RmePtr = 21;
pub const RME_PGT_SIZE_1G: RmePtr = 30;
pub const RME_PGT_NUM_512: RmePtr = 9;

/* Thread flags (mirror of Include/rme.h) ************************************/
pub const RME_THD_FLAG_EXEC_SET: RmePtr = 1 << 0;
pub const RME_THD_FLAG_SCHED_CHILD: RmePtr = 1 << 1;
pub const RME_THD_FLAG_SCHED_PARENT: RmePtr = 1 << 2;
pub const RME_THD_FLAG_SCHED_PRIO: RmePtr = 1 << 3;
pub const RME_THD_FLAG_SCHED_FREE: RmePtr = 1 << 4;
pub const RME_THD_FLAG_SCHED_RCV: RmePtr = 1 << 5;
pub const RME_THD_FLAG_XFER_SRC: RmePtr = 1 << 6;
pub const RME_THD_FLAG_XFER_DST: RmePtr = 1 << 7;
pub const RME_THD_FLAG_SWT: RmePtr = 1 << 8;
pub const RME_THD_FLAG_ALL: RmePtr = RME_THD_FLAG_EXEC_SET
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
pub const fn rme_param_d1(x: RmePtr) -> RmePtr {
    (x & 0xFFFFFFFF) << 32
}
#[inline(always)]
pub const fn rme_param_d0(x: RmePtr) -> RmePtr {
    x & 0xFFFFFFFF
}
#[inline(always)]
pub const fn rme_param_q3(x: RmePtr) -> RmePtr {
    (x & 0xFFFF) << 48
}
#[inline(always)]
pub const fn rme_param_q2(x: RmePtr) -> RmePtr {
    (x & 0xFFFF) << 32
}
#[inline(always)]
pub const fn rme_param_q1(x: RmePtr) -> RmePtr {
    (x & 0xFFFF) << 16
}
#[inline(always)]
pub const fn rme_param_q0(x: RmePtr) -> RmePtr {
    x & 0xFFFF
}
#[inline(always)]
pub const fn rme_param_o7(x: RmePtr) -> RmePtr {
    (x & 0xFF) << 56
}
#[inline(always)]
pub const fn rme_param_o6(x: RmePtr) -> RmePtr {
    (x & 0xFF) << 48
}
#[inline(always)]
pub const fn rme_param_o5(x: RmePtr) -> RmePtr {
    (x & 0xFF) << 40
}
#[inline(always)]
pub const fn rme_param_o4(x: RmePtr) -> RmePtr {
    (x & 0xFF) << 32
}
#[inline(always)]
pub const fn rme_param_o3(x: RmePtr) -> RmePtr {
    (x & 0xFF) << 24
}
#[inline(always)]
pub const fn rme_param_o2(x: RmePtr) -> RmePtr {
    (x & 0xFF) << 16
}
#[inline(always)]
pub const fn rme_param_o1(x: RmePtr) -> RmePtr {
    (x & 0xFF) << 8
}
#[inline(always)]
pub const fn rme_param_o0(x: RmePtr) -> RmePtr {
    x & 0xFF
}
/* End Parameter packing *****************************************************/

/* Type:RmeRet ***************************************************************
Description : The three registers the kernel can write as return values.
              a0 is the primary status/value, a1 carries the INV_ACT callee
              return, and a2 carries DEBUG_EXC_GET's query value.
*****************************************************************************/
#[repr(C)]
#[derive(Clone, Copy)]
pub struct RmeRet {
    pub a0: RmePtr,
    pub a1: RmePtr,
    pub a2: RmePtr,
}
/* End Type:RmeRet ***********************************************************/

impl RmeRet {
    #[inline(always)]
    pub fn status(&self) -> i64 {
        self.a0 as i64
    }

    #[inline(always)]
    pub fn ok(&self) -> bool {
        self.status() >= 0
    }
}

/* Function:rme_syscall_raw ***************************************************
Description : The only ecall. It returns a0/a1/a2 together because most SVCs
              only use a0, INV_ACT uses a1, and DEBUG_EXC_GET uses a2.
              The Svc word is (svc_num << 32) | (cid & 0xFFFFFFFF).
Input       : svc_num - The system call number (including Attr/PC bits).
              cid - The capability ID (32 bits).
              param0 - The first system call parameter (a1).
              param1 - The second system call parameter (a2).
              param2 - The third system call parameter (a3).
Output      : None.
Return      : RmeRet - (a0, a1, a2).
*****************************************************************************/
#[inline(always)]
pub fn rme_syscall_raw(
    svc_num: RmePtr,
    cid: RmePtr,
    param0: RmePtr,
    param1: RmePtr,
    param2: RmePtr,
) -> RmeRet {
    let mut a0: RmePtr = (svc_num << 32) | (cid & 0xFFFF_FFFF);
    let mut a1: RmePtr = param0;
    let mut a2: RmePtr = param1;

    unsafe {
        core::arch::asm!(
            "ecall",
            inlateout("a0") a0 => a0,
            inlateout("a1") a1 => a1,
            inlateout("a2") a2 => a2,
            in("a3") param2,
            options(nostack),
        );
    }

    RmeRet { a0, a1, a2 }
}
/* End Function:rme_syscall_raw **********************************************/

/* Function:rme_syscall *******************************************************
Description : Scalar wrapper for the SVCs that only need the a0 return value.
Input       : svc_num - The system call number (including Attr/PC bits).
              cid - The capability ID (in the caller's capability table).
              param0 - The first system call parameter.
              param1 - The second system call parameter.
              param2 - The third system call parameter.
Output      : None.
Return      : RmePtr - The kernel return value (in a0).
******************************************************************************/
#[inline(always)]
pub fn rme_syscall(
    svc_num: RmePtr,
    cid: RmePtr,
    param0: RmePtr,
    param1: RmePtr,
    param2: RmePtr,
) -> RmePtr {
    rme_syscall_raw(svc_num, cid, param0, param1, param2).a0
}
/* End Function:rme_syscall **************************************************/

/* Function:rme_cid_2l *******************************************************
Description : Build a two-level capability ID: the major capability ID is the
              capability table slot that holds the target capability, and the
              minor one is the target capability's slot inside that table.
              This mirrors the kernel's RME_CID(X,Y) macro.
Input       : table - The capability table slot holding the target capability.
              slot - The target capability's slot inside that table.
Output      : None.
Return      : RmePtr - The two-level capability ID.
*****************************************************************************/
#[inline(always)]
pub const fn rme_cid_2l(table: RmePtr, slot: RmePtr) -> RmePtr {
    (table << 16) | RME_CID_2L | slot
}
/* End Function:rme_cid_2l **************************************************/

/* Function:rme_thd_swt *******************************************************
Description : Switch to another thread. The target must have the same
              preemptive priority, be bound to the same core, and have time
              left. This is the RME equivalent of the Ammonite benchmark's
              RVM_Thd_Swt.
Input       : cap_thd - The capability to the target thread (two-level).
              is_yield - Non-zero to give up all of the caller's timeslices.
Output      : None.
Return      : RmePtr - The kernel return value (0 on success).
*****************************************************************************/
#[inline(always)]
pub fn rme_thd_swt(cap_thd: RmePtr, is_yield: RmePtr) -> RmePtr {
    rme_syscall(RME_SVC_THD_SWT, 0, cap_thd, is_yield, 0)
}
/* End Function:rme_thd_swt **************************************************/

/* Function:rme_thd_crt *******************************************************
Description : Create a thread. The capability is created in the capability
              table given by the major capability ID.
Input       : cap_cpt - The capability table to create the thread capability in.
              cap_kom - The kernel memory capability used for the thread object.
              cap_thd - The capability slot to create the thread capability in.
              cap_prc - The process the thread belongs to.
              prio_max - The maximum preemptive priority the thread may have.
              raddr - The relative kernel memory address of the thread object.
Output      : None.
Return      : RmePtr - The kernel return value (0 on success).
*****************************************************************************/
#[inline(always)]
pub fn rme_thd_crt(
    cap_cpt: RmePtr,
    cap_kom: RmePtr,
    cap_thd: RmePtr,
    cap_prc: RmePtr,
    prio_max: RmePtr,
    raddr: RmePtr,
) -> RmePtr {
    rme_syscall(
        RME_SVC_THD_CRT,
        cap_cpt,
        rme_param_d1(cap_kom) | rme_param_d0(cap_thd),
        rme_param_d1(cap_prc) | rme_param_d0(prio_max),
        raddr,
    )
}
/* End Function:rme_thd_crt **************************************************/

/* Function:rme_thd_sched_bind ***********************************************
Description : Bind a thread to the current processor and nominate its
              scheduler.
Input       : cap_thd - The capability to the thread (two-level).
              cap_sched - The capability to the scheduler thread (two-level).
              cap_sig - The capability to the scheduling notification endpoint.
              tid - The thread ID to assign.
              prio - The preemptive priority of the thread.
              haddr - The hypervisor address of the thread (unused here).
Output      : None.
Return      : RmePtr - The kernel return value (0 on success).
*****************************************************************************/
#[inline(always)]
pub fn rme_thd_sched_bind(
    cap_thd: RmePtr,
    cap_sched: RmePtr,
    cap_sig: RmePtr,
    tid: RmePtr,
    prio: RmePtr,
    haddr: RmePtr,
) -> RmePtr {
    rme_syscall(
        RME_SVC_THD_SCHED_BIND,
        cap_thd,
        rme_param_d1(cap_sched) | rme_param_d0(cap_sig),
        rme_param_d1(tid) | rme_param_d0(prio),
        haddr,
    )
}
/* End Function:rme_thd_sched_bind *******************************************/

/* Function:rme_thd_exec_set *************************************************
Description : Set a thread's entry point, stack pointer and initial argument.
              The argument lands in a0, which is how the benchmark tells the
              freshly started helper thread which role it has to play.
Input       : cap_thd - The capability to the thread (two-level).
              entry - The entry point address.
              stack - The stack pointer.
              param - The argument passed in a0.
Output      : None.
Return      : RmePtr - The kernel return value (0 on success).
*****************************************************************************/
#[inline(always)]
pub fn rme_thd_exec_set(cap_thd: RmePtr, entry: RmePtr, stack: RmePtr, param: RmePtr) -> RmePtr {
    rme_syscall(RME_SVC_THD_EXEC_SET, cap_thd, entry, stack, param)
}
/* End Function:rme_thd_exec_set **********************************************/

/* Function:rme_thd_time_xfer ************************************************
Description : Transfer timeslices from one thread to another.
Input       : cap_dst - The capability to the receiving thread (two-level).
              cap_src - The capability to the donating thread (two-level).
              time - The number of timeslices to transfer.
Output      : None.
Return      : RmePtr - The kernel return value (0 on success).
*****************************************************************************/
#[inline(always)]
pub fn rme_thd_time_xfer(cap_dst: RmePtr, cap_src: RmePtr, time: RmePtr) -> RmePtr {
    rme_syscall(RME_SVC_THD_TIME_XFER, 0, cap_dst, cap_src, time)
}
/* End Function:rme_thd_time_xfer ********************************************/

/* Function:rme_thd_sched_rcv ************************************************
Description : Poll the scheduler notification queue of a thread. This is the
              user visible trace of the kernel's tick preemption: when a
              thread runs out of timeslices the kernel notifies its scheduler.
              The call returns immediately with RME_ERR_PTH_NOTIF when there
              is nothing to report.
Input       : cap_thd - The capability to the scheduler thread (two-level).
Output      : None.
Return      : RmePtr - The kernel return value, or the notification on success.
*****************************************************************************/
#[inline(always)]
pub fn rme_thd_sched_rcv(cap_thd: RmePtr) -> RmePtr {
    rme_syscall(RME_SVC_THD_SCHED_RCV, 0, cap_thd, 0, 0)
}
/* End Function:rme_thd_sched_rcv ********************************************/

/* Function:rme_sig_crt *******************************************************
Description : Create a signal endpoint in a capability table.
Input       : cap_cpt - The capability table to create the endpoint in.
              cap_sig - The capability slot to create the endpoint in.
Output      : None.
Return      : RmePtr - The kernel return value (0 on success).
*****************************************************************************/
#[inline(always)]
pub fn rme_sig_crt(cap_cpt: RmePtr, cap_sig: RmePtr) -> RmePtr {
    rme_syscall(RME_SVC_SIG_CRT, cap_cpt, cap_sig, 0, 0)
}
/* End Function:rme_sig_crt ***************************************************/

/* Function:rme_sig_snd *******************************************************
Description : Send signals to a signal endpoint. When the receiver is blocked
              on the same core, the kernel makes it runnable again.
Input       : cap_sig - The capability to the signal endpoint (two-level).
              number - The number of signals to send.
Output      : None.
Return      : RmePtr - The kernel return value (0 on success).
*****************************************************************************/
#[inline(always)]
pub fn rme_sig_snd(cap_sig: RmePtr, number: RmePtr) -> RmePtr {
    rme_syscall(RME_SVC_SIG_SND, 0, cap_sig, number, 0)
}
/* End Function:rme_sig_snd ***************************************************/

/* Function:rme_sig_rcv *******************************************************
Description : Receive from a signal endpoint.
Input       : cap_sig - The capability to the signal endpoint (two-level).
              option - The receive option (RME_RCV_BM blocks, RME_RCV_NM does
                       not block).
Output      : None.
Return      : RmePtr - The number of signals received, or an error code.
*****************************************************************************/
#[inline(always)]
pub fn rme_sig_rcv(cap_sig: RmePtr, option: RmePtr) -> RmePtr {
    rme_syscall(RME_SVC_SIG_RCV, 0, cap_sig, option, 0)
}
/* End Function:rme_sig_rcv ***************************************************/

/* Function:rme_inv_crt *******************************************************
Description : Create an invocation stub bound to a target process.
Input       : cap_cpt - The capability table to create the stub capability in.
              cap_kom - The kernel memory capability for the stub object.
              cap_inv - The capability slot to create the stub capability in.
              cap_prc - The target process that the invocation enters.
              raddr - The relative kernel memory address of the stub object.
Output      : None.
Return      : RmePtr - The kernel return value (0 on success).
*****************************************************************************/
#[inline(always)]
pub fn rme_inv_crt(
    cap_cpt: RmePtr,
    cap_kom: RmePtr,
    cap_inv: RmePtr,
    cap_prc: RmePtr,
    raddr: RmePtr,
) -> RmePtr {
    rme_syscall(
        RME_SVC_INV_CRT,
        cap_cpt,
        rme_param_d1(cap_kom) | rme_param_d0(cap_inv),
        cap_prc,
        raddr,
    )
}
/* End Function:rme_inv_crt ***************************************************/

/* Function:rme_inv_set *******************************************************
Description : Set an invocation stub's entry point and stack.
Input       : cap_inv - The capability to the invocation stub (two-level).
              entry - The entry point address.
              stack - The stack pointer.
              is_exc_ret - Non-zero to return from the invocation on a fault.
Output      : None.
Return      : RmePtr - The kernel return value (0 on success).
*****************************************************************************/
#[inline(always)]
pub fn rme_inv_set(cap_inv: RmePtr, entry: RmePtr, stack: RmePtr, is_exc_ret: RmePtr) -> RmePtr {
    rme_syscall(
        RME_SVC_INV_SET,
        0,
        rme_param_d0(cap_inv) | rme_param_d1(is_exc_ret),
        entry,
        stack,
    )
}
/* End Function:rme_inv_set ***************************************************/

/* The invocation entry/return path only restores PC and SP. The invocation
 * stub may clobber ra and s0-s11 because it exits through INV_RET instead of
 * running its function epilogue. The compiler cannot express s0/s1 as inline
 * asm clobbers (s0 is the frame pointer and s1 is reserved by LLVM), so this
 * tiny gate saves/restores ra and s0-s11 around the ecall. Caller-saved
 * registers are covered by the normal extern "C" call boundary.
 *
 * The gate returns two registers (a0/a1); the Rust wrapper assembles RmeRet
 * with a2 = 0. RmeRet itself is 24 bytes, so it is not returned directly
 * through the C ABI. */
#[repr(C)]
struct RmeRet2 {
    a0: RmePtr,
    a1: RmePtr,
}

core::arch::global_asm!(
    r#"
    .section .text.rme_inv_act_asm, "ax", @progbits
    .align 2
    .global rme_inv_act_asm
rme_inv_act_asm:
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
    fn rme_inv_act_asm(cap_inv: RmePtr, param: RmePtr) -> RmeRet2;
}

/* Function:rme_inv_act *******************************************************
Description : Activate an invocation stub. a0 is the syscall status (0 or a
              non-negative success value, negative on failure), a1 is the
              callee's INV_RET value, and a2 is unused (zero).
Input       : cap_inv - The capability to the invocation stub (two-level).
              param - The argument passed to the callee.
Output      : None.
Return      : RmeRet - (a0, a1, 0).
*****************************************************************************/
#[inline(always)]
pub fn rme_inv_act(cap_inv: RmePtr, param: RmePtr) -> RmeRet {
    let ret = unsafe { rme_inv_act_asm(cap_inv, param) };
    RmeRet {
        a0: ret.a0,
        a1: ret.a1,
        a2: 0,
    }
}
/* End Function:rme_inv_act ***************************************************/

/* Function:rme_inv_ret *******************************************************
Description : Return from a synchronous invocation.
Input       : retval - The value returned to the invoker.
Output      : None.
Return      : RmePtr - The kernel return value (0 on success).
*****************************************************************************/
#[inline(always)]
pub fn rme_inv_ret(retval: RmePtr) -> RmePtr {
    rme_syscall(RME_SVC_INV_RET, 0, retval, 0, 0)
}
/* End Function:rme_inv_ret ***************************************************/

/* Function:rme_cpt_crt *******************************************************
Description : Create a capability table.
Input       : cap_cpt - The capability table to create the new table in.
              cap_kom - The kernel memory capability for the table object.
              cap_crt - The capability slot to create the new table in.
              raddr - The relative kernel memory address of the table object.
              entry_num - The number of entries in the new table.
Output      : None.
Return      : RmePtr - The kernel return value (0 on success).
*****************************************************************************/
#[inline(always)]
pub fn rme_cpt_crt(
    cap_cpt: RmePtr,
    cap_kom: RmePtr,
    cap_crt: RmePtr,
    raddr: RmePtr,
    entry_num: RmePtr,
) -> RmePtr {
    rme_syscall(
        RME_SVC_CPT_CRT,
        cap_cpt,
        rme_param_d1(cap_kom) | rme_param_d0(cap_crt),
        raddr,
        entry_num,
    )
}
/* End Function:rme_cpt_crt ***************************************************/

/* Function:rme_cpt_add *******************************************************
Description : Delegate a capability from one capability table into another.
Input       : cap_cpt_dst - The destination capability table (two-level).
              cap_dst - The destination slot inside that table.
              cap_cpt_src - The source capability table (two-level).
              cap_src - The source slot inside that table.
              flag - The subset of the source's rights to delegate.
Output      : None.
Return      : RmePtr - The kernel return value (0 on success).
*****************************************************************************/
#[inline(always)]
pub fn rme_cpt_add(
    cap_cpt_dst: RmePtr,
    cap_dst: RmePtr,
    cap_cpt_src: RmePtr,
    cap_src: RmePtr,
    flag: RmePtr,
) -> RmePtr {
    rme_syscall(
        RME_SVC_CPT_ADD,
        0,
        rme_param_d1(cap_cpt_dst) | rme_param_d0(cap_dst),
        rme_param_d1(cap_cpt_src) | rme_param_d0(cap_src),
        flag,
    )
}
/* End Function:rme_cpt_add ***************************************************/

/* Function:rme_prc_crt *******************************************************
Description : Create a process.
Input       : cap_cpt - The capability table to create the process in.
              cap_prc - The capability slot to create the process in.
              cap_cpt_new - The capability table owned by the new process.
              cap_pgt - The root page table owned by the new process.
Output      : None.
Return      : RmePtr - The kernel return value (0 on success).
*****************************************************************************/
#[inline(always)]
pub fn rme_prc_crt(
    cap_cpt: RmePtr,
    cap_prc: RmePtr,
    cap_cpt_new: RmePtr,
    cap_pgt: RmePtr,
) -> RmePtr {
    rme_syscall(RME_SVC_PRC_CRT, cap_cpt, cap_prc, cap_cpt_new, cap_pgt)
}
/* End Function:rme_prc_crt ***************************************************/

/* Function:rme_thd_sched_free ***********************************************
Description : Free a thread from its current processor binding.
Input       : cap_thd - The capability to the target thread.
Output      : None.
Return      : RmePtr - The kernel return value (0 on success).
*****************************************************************************/
#[inline(always)]
pub fn rme_thd_sched_free(cap_thd: RmePtr) -> RmePtr {
    rme_syscall(RME_SVC_THD_SCHED_FREE, 0, cap_thd, 0, 0)
}
/* End Function:rme_thd_sched_free *******************************************/

/* Function:rme_thd_sched_prio ***********************************************
Description : Change one thread's priority. This is the one-thread form of
              THD_SCHED_PRIO (Number = 1).
Input       : cap_thd - The capability to the target thread.
              prio - The new preemptive priority.
Output      : None.
Return      : RmePtr - The kernel return value (0 on success).
*****************************************************************************/
#[inline(always)]
pub fn rme_thd_sched_prio(cap_thd: RmePtr, prio: RmePtr) -> RmePtr {
    rme_syscall(
        RME_SVC_THD_SCHED_PRIO,
        1,
        rme_param_d0(cap_thd),
        0,
        rme_param_q0(prio),
    )
}
/* End Function:rme_thd_sched_prio *******************************************/

/* Function:rme_cpt_del ******************************************************
Description : Delete an empty capability table. The target capability must be
              frozen first.
Input       : cap_cpt - The capability table that contains the target cap.
              cap_del - The target capability slot.
Output      : None.
Return      : RmePtr - The kernel return value (0 on success).
*****************************************************************************/
#[inline(always)]
pub fn rme_cpt_del(cap_cpt: RmePtr, cap_del: RmePtr) -> RmePtr {
    rme_syscall(RME_SVC_CPT_DEL, cap_cpt, cap_del, 0, 0)
}
/* End Function:rme_cpt_del **************************************************/

/* Function:rme_cpt_frz ******************************************************
Description : Freeze a capability. Freeze is irreversible; deletion/removal
              paths require this state first.
Input       : cap_cpt - The capability table that contains the target cap.
              cap_frz - The target capability slot.
Output      : None.
Return      : RmePtr - The kernel return value (0 on success).
*****************************************************************************/
#[inline(always)]
pub fn rme_cpt_frz(cap_cpt: RmePtr, cap_frz: RmePtr) -> RmePtr {
    rme_syscall(RME_SVC_CPT_FRZ, cap_cpt, cap_frz, 0, 0)
}
/* End Function:rme_cpt_frz **************************************************/

/* Function:rme_cpt_rem ******************************************************
Description : Remove a delegated (leaf) capability from a table. The target
              capability must be frozen first.
Input       : cap_cpt - The capability table that contains the target cap.
              cap_rem - The target capability slot.
Output      : None.
Return      : RmePtr - The kernel return value (0 on success).
*****************************************************************************/
#[inline(always)]
pub fn rme_cpt_rem(cap_cpt: RmePtr, cap_rem: RmePtr) -> RmePtr {
    rme_syscall(RME_SVC_CPT_REM, cap_cpt, cap_rem, 0, 0)
}
/* End Function:rme_cpt_rem **************************************************/

/* Function:rme_pgt_crt ******************************************************
Description : Create a page table. Num_Order is an exponent (512 entries = 9),
              and is carried in the high bits of the service number.
Input       : cap_cpt - The capability table to create the cap in.
              cap_kom - The kernel memory capability.
              cap_pgt - The capability slot for the new table.
              raddr - The relative KOM address (4KB aligned).
              base - The virtual base the table covers.
              is_top - Non-zero for the top-level table (1G entries only).
              size_order - The page size order (4K/2M/1G).
Output      : None.
Return      : RmePtr - The kernel return value (0 on success).
*****************************************************************************/
#[inline(always)]
pub fn rme_pgt_crt(
    cap_cpt: RmePtr,
    cap_kom: RmePtr,
    cap_pgt: RmePtr,
    raddr: RmePtr,
    base: RmePtr,
    is_top: RmePtr,
    size_order: RmePtr,
) -> RmePtr {
    let svc_num: RmePtr = RME_SVC_PGT_CRT | (RME_PGT_NUM_512 << 16);
    let param0: RmePtr = rme_param_d1(cap_kom) | rme_param_q1(cap_pgt) | rme_param_q0(size_order);
    rme_syscall(svc_num, cap_cpt, param0, raddr, base | is_top)
}
/* End Function:rme_pgt_crt **************************************************/

/* Function:rme_pgt_del ******************************************************
Description : Delete a page table. The target capability must be frozen first.
Input       : cap_cpt - The capability table that contains the target cap.
              cap_pgt - The target capability slot.
Output      : None.
Return      : RmePtr - The kernel return value (0 on success).
*****************************************************************************/
#[inline(always)]
pub fn rme_pgt_del(cap_cpt: RmePtr, cap_pgt: RmePtr) -> RmePtr {
    rme_syscall(RME_SVC_PGT_DEL, cap_cpt, cap_pgt, 0, 0)
}
/* End Function:rme_pgt_del **************************************************/

/* Function:rme_pgt_add ******************************************************
Description : Map a page from a source table into a destination table.
Input       : cap_dst - The destination page table capability.
              pos_dst - The destination position.
              cap_src - The source page table capability.
              pos_src - The source position.
              flag_dst - The destination page permissions (must be a subset of
                         the source page's flags; use ALL_DYN for init pages).
Output      : None.
Return      : RmePtr - The kernel return value (0 on success).
*****************************************************************************/
#[inline(always)]
pub fn rme_pgt_add(
    cap_dst: RmePtr,
    pos_dst: RmePtr,
    cap_src: RmePtr,
    pos_src: RmePtr,
    flag_dst: RmePtr,
) -> RmePtr {
    rme_syscall(
        RME_SVC_PGT_ADD,
        flag_dst,
        rme_param_d1(cap_dst) | rme_param_d0(pos_dst),
        rme_param_d1(cap_src) | rme_param_d0(pos_src),
        0,
    )
}
/* End Function:rme_pgt_add **************************************************/

/* Function:rme_pgt_rem ******************************************************
Description : Unmap a page from a page table.
Input       : cap_pgt - The page table capability.
              pos - The position to unmap.
Output      : None.
Return      : RmePtr - The kernel return value (0 on success).
*****************************************************************************/
#[inline(always)]
pub fn rme_pgt_rem(cap_pgt: RmePtr, pos: RmePtr) -> RmePtr {
    rme_syscall(RME_SVC_PGT_REM, 0, cap_pgt, pos, 0)
}
/* End Function:rme_pgt_rem **************************************************/

/* Function:rme_pgt_con ******************************************************
Description : Map a child page table into a parent page table.
Input       : cap_parent - The parent page table capability.
              pos - The parent position.
              cap_child - The child page table capability.
              flag_child - The child mapping flags.
Output      : None.
Return      : RmePtr - The kernel return value (0 on success).
*****************************************************************************/
#[inline(always)]
pub fn rme_pgt_con(
    cap_parent: RmePtr,
    pos: RmePtr,
    cap_child: RmePtr,
    flag_child: RmePtr,
) -> RmePtr {
    rme_syscall(
        RME_SVC_PGT_CON,
        0,
        rme_param_d1(cap_parent) | rme_param_d0(cap_child),
        pos,
        flag_child,
    )
}
/* End Function:rme_pgt_con **************************************************/

/* Function:rme_pgt_des ******************************************************
Description : Unmap a child page table from a parent page table.
Input       : cap_parent - The parent page table capability.
              pos - The parent position.
              cap_child - The child page table capability.
Output      : None.
Return      : RmePtr - The kernel return value (0 on success).
*****************************************************************************/
#[inline(always)]
pub fn rme_pgt_des(cap_parent: RmePtr, pos: RmePtr, cap_child: RmePtr) -> RmePtr {
    rme_syscall(RME_SVC_PGT_DES, 0, cap_parent, pos, cap_child)
}
/* End Function:rme_pgt_des **************************************************/

/* Function:rme_prc_del ******************************************************
Description : Delete a process. The target capability must be frozen first.
Input       : cap_cpt - The capability table that contains the target cap.
              cap_prc - The target process capability slot.
Output      : None.
Return      : RmePtr - The kernel return value (0 on success).
*****************************************************************************/
#[inline(always)]
pub fn rme_prc_del(cap_cpt: RmePtr, cap_prc: RmePtr) -> RmePtr {
    rme_syscall(RME_SVC_PRC_DEL, cap_cpt, cap_prc, 0, 0)
}
/* End Function:rme_prc_del **************************************************/

/* Function:rme_prc_cpt ******************************************************
Description : Change a process's capability table.
Input       : cap_prc - The process capability.
              cap_cpt - The new capability table capability.
Output      : None.
Return      : RmePtr - The kernel return value (0 on success).
*****************************************************************************/
#[inline(always)]
pub fn rme_prc_cpt(cap_prc: RmePtr, cap_cpt: RmePtr) -> RmePtr {
    rme_syscall(RME_SVC_PRC_CPT, 0, cap_prc, cap_cpt, 0)
}
/* End Function:rme_prc_cpt **************************************************/

/* Function:rme_prc_pgt ******************************************************
Description : Change a process's page table.
Input       : cap_prc - The process capability.
              cap_pgt - The new root page table capability.
Output      : None.
Return      : RmePtr - The kernel return value (0 on success).
*****************************************************************************/
#[inline(always)]
pub fn rme_prc_pgt(cap_prc: RmePtr, cap_pgt: RmePtr) -> RmePtr {
    rme_syscall(RME_SVC_PRC_PGT, 0, cap_prc, cap_pgt, 0)
}
/* End Function:rme_prc_pgt **************************************************/

/* Function:rme_thd_del ******************************************************
Description : Delete a thread. The target capability must be frozen and the
              thread must be unbound.
Input       : cap_cpt - The capability table that contains the target cap.
              cap_thd - The target thread capability slot.
Output      : None.
Return      : RmePtr - The kernel return value (0 on success).
*****************************************************************************/
#[inline(always)]
pub fn rme_thd_del(cap_cpt: RmePtr, cap_thd: RmePtr) -> RmePtr {
    rme_syscall(RME_SVC_THD_DEL, cap_cpt, cap_thd, 0, 0)
}
/* End Function:rme_thd_del **************************************************/

/* Function:rme_sig_del ******************************************************
Description : Delete a signal endpoint. The target capability must be frozen
              and no thread may be blocked on it.
Input       : cap_cpt - The capability table that contains the target cap.
              cap_sig - The target signal capability slot.
Output      : None.
Return      : RmePtr - The kernel return value (0 on success).
*****************************************************************************/
#[inline(always)]
pub fn rme_sig_del(cap_cpt: RmePtr, cap_sig: RmePtr) -> RmePtr {
    rme_syscall(RME_SVC_SIG_DEL, cap_cpt, cap_sig, 0, 0)
}
/* End Function:rme_sig_del **************************************************/

/* Function:rme_inv_del ******************************************************
Description : Delete an invocation stub. The target capability must be frozen
              and the stub must not be active.
Input       : cap_cpt - The capability table that contains the target cap.
              cap_inv - The target invocation capability slot.
Output      : None.
Return      : RmePtr - The kernel return value (0 on success).
*****************************************************************************/
#[inline(always)]
pub fn rme_inv_del(cap_cpt: RmePtr, cap_inv: RmePtr) -> RmePtr {
    rme_syscall(RME_SVC_INV_DEL, cap_cpt, cap_inv, 0, 0)
}
/* End Function:rme_inv_del **************************************************/

/* Function:rme_kfn **********************************************************
Description : Invoke a kernel function.
Input       : cap_kfn - The kernel function capability.
              func_id - The function ID.
              sub_id - The subfunction ID.
              param1 - The first function parameter.
              param2 - The second function parameter.
Output      : None.
Return      : RmePtr - The kernel return value.
*****************************************************************************/
#[inline(always)]
pub fn rme_kfn(
    cap_kfn: RmePtr,
    func_id: RmePtr,
    sub_id: RmePtr,
    param1: RmePtr,
    param2: RmePtr,
) -> RmePtr {
    rme_kfn_raw(cap_kfn, func_id, sub_id, param1, param2).a0
}
/* End Function:rme_kfn ******************************************************/

/* Function:rme_kfn_raw *******************************************************
Description : KFN with the full a0/a1/a2 return set. DEBUG_REG_MOD and
              DEBUG_INV_MOD return their value in a1; DEBUG_EXC_GET returns it
              in a2.
*****************************************************************************/
#[inline(always)]
pub fn rme_kfn_raw(
    cap_kfn: RmePtr,
    func_id: RmePtr,
    sub_id: RmePtr,
    param1: RmePtr,
    param2: RmePtr,
) -> RmeRet {
    rme_syscall_raw(
        RME_SVC_KFN,
        cap_kfn,
        rme_param_d0(func_id) | rme_param_d1(sub_id),
        param1,
        param2,
    )
}
/* End Function:rme_kfn_raw **************************************************/

/* End Of File ***************************************************************/

/* Copyright (C) Evo-Devo Instrum. All rights reserved ***********************/
