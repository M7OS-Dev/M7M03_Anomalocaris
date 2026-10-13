/******************************************************************************
Filename    : syscall.rs
Author      : yhy
Date        : 13/10/2026
Licence     : The Unlicense; see LICENSE for details.
Description : User-mode system call interface for the RV64V port on QEMU
              virt, mirroring the C rme_syscall.h. Mirrors the kernel's
              syscall ABI and constants (Include/rme.h) without depending on
              kernel headers, so any user program can include it standalone.
              Syscall ABI on RV64V:
                a0 = (Svc_Num << 16) | Cid
                a1 = Param0, a2 = Param1, a3 = Param2
                ecall                 (cause 8, delegated to S-mode by OpenSBI)
                return value comes back in a0
              rme_syscall() is the ONLY ecall in a user program - everything
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

/* Boot capability slot for the init KFN (mirror of rme_platform_rv64v.h) ****/
pub const RME_BOOT_INIT_KFN: RmePtr = 4;
/* End Define ****************************************************************/

/* Function:rme_syscall *******************************************************
Description : The generic system call - the only ecall in the user program.
              Everything else is built on top of this primitive.
Input       : svc_num - The system call number.
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
    let a0: RmePtr = (svc_num << 16) | cid;
    let ret: RmePtr;

    unsafe {
        core::arch::asm!(
            "ecall",
            inlateout("a0") a0 => ret,
            in("a1") param0,
            in("a2") param1,
            in("a3") param2,
            options(nostack),
        );
    }

    ret
}
/* End Function:rme_syscall **************************************************/

/* End Of File ***************************************************************/

/* Copyright (C) Evo-Devo Instrum. All rights reserved ***********************/
