//! 系统调用实现
//!
//! 所有系统调用的单一入口点 [`syscall()`] 在用户空间希望使用 `ecall`
//! 指令执行系统调用时被调用。在这种情况下，处理器会引发一个来自 U 模式的
//! "Environment call" 异常，该异常在 [`crate::trap::trap_handler`] 中作为
//! 一种情况被处理。
//!
//! 为了清晰起见，每个系统调用都实现为它自己的函数，命名为 `sys_` 加上
//! 系统调用的名称。你可以在子模块中找到这样的函数，你也应该以这种方式
//! 实现系统调用。

// 写系统调用
const SYSCALL_WRITE: usize = 64;
// 退出系统调用
const SYSCALL_EXIT: usize = 93;
// 让出 CPU 系统调用
const SYSCALL_YIELD: usize = 124;
// 获取时间系统调用
const SYSCALL_GET_TIME: usize = 169;
// 调整堆指针系统调用
const SYSCALL_SBRK: usize = 214;
// 解除内存映射系统调用
const SYSCALL_MUNMAP: usize = 215;
// 内存映射系统调用
const SYSCALL_MMAP: usize = 222;
// 追踪系统调用
const SYSCALL_TRACE: usize = 410;

mod fs;
mod process;

use crate::task::increment_syscall_count;
use fs::*;
use process::*;

/// 处理系统调用异常
/// 
/// # 参数
/// - `syscall_id`: 系统调用 ID
/// - `args`: 系统调用参数数组（最多 3 个参数）
/// 
/// # 返回值
/// - 系统调用的返回值
/// 
/// # 实现细节
/// 1. 首先增加该系统调用的调用计数
/// 2. 根据 syscall_id 分发到对应的系统调用处理函数
pub fn syscall(syscall_id: usize, args: [usize; 3]) -> isize {
    // 增加系统调用计数
    increment_syscall_count(syscall_id);
    
    // 根据系统调用 ID 分发到对应的处理函数
    match syscall_id {
        SYSCALL_WRITE => sys_write(args[0], args[1] as *const u8, args[2]),
        SYSCALL_EXIT => sys_exit(args[0] as i32),
        SYSCALL_YIELD => sys_yield(),
        SYSCALL_GET_TIME => sys_get_time(args[0] as *mut TimeVal, args[1]),
        SYSCALL_TRACE => sys_trace(args[0], args[1], args[2]),
        SYSCALL_MMAP => sys_mmap(args[0], args[1], args[2]),
        SYSCALL_MUNMAP => sys_munmap(args[0], args[1]),
        SYSCALL_SBRK => sys_sbrk(args[0] as i32),
        _ => panic!("Unsupported syscall_id: {}", syscall_id),
    }
}
