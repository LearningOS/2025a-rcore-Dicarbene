//! Task management implementation
//!
//! Everything about task management, like starting and switching tasks is
//! implemented here.
//!
//! A single global instance of [`TaskManager`] called `TASK_MANAGER` controls
//! all the tasks in the operating system.
//!
//! Be careful when you see `__switch` ASM function in `switch.S`. Control flow around this function
//! might not be what you expect.

mod context;
mod switch;
#[allow(clippy::module_inception)]
mod task;

use crate::loader::{get_app_data, get_num_app};
use crate::sync::UPSafeCell;
use crate::trap::TrapContext;
use alloc::vec::Vec;
use lazy_static::*;
use switch::__switch;
pub use task::{TaskControlBlock, TaskStatus};

pub use context::TaskContext;

/// The task manager, where all the tasks are managed.
///
/// Functions implemented on `TaskManager` deals with all task state transitions
/// and task context switching. For convenience, you can find wrappers around it
/// in the module level.
///
/// Most of `TaskManager` are hidden behind the field `inner`, to defer
/// borrowing checks to runtime. You can see examples on how to use `inner` in
/// existing functions on `TaskManager`.
pub struct TaskManager {
    /// total number of tasks
    num_app: usize,
    /// use inner value to get mutable access
    inner: UPSafeCell<TaskManagerInner>,
}

/// 任务管理器内部结构体（在 UPSafeCell 中）
struct TaskManagerInner {
    /// 任务列表
    tasks: Vec<TaskControlBlock>,
    /// 当前运行任务的 ID
    current_task: usize,
    /// 系统调用调用次数数组，索引为系统调用 ID
    syscall_counts: [usize; 512],
}

lazy_static! {
    /// a `TaskManager` global instance through lazy_static!
    pub static ref TASK_MANAGER: TaskManager = {
        println!("init TASK_MANAGER");
        let num_app = get_num_app();
        println!("num_app = {}", num_app);
        let mut tasks: Vec<TaskControlBlock> = Vec::new();
        for i in 0..num_app {
            tasks.push(TaskControlBlock::new(get_app_data(i), i));
        }
        TaskManager {
            num_app,
            inner: unsafe {
                UPSafeCell::new(TaskManagerInner {
                    tasks,
                    current_task: 0,
                    syscall_counts: [0; 512],
                })
            },
        }
    };
}

impl TaskManager {
    /// 增加指定系统调用的调用次数
    /// 
    /// # 参数
    /// - `syscall_id`: 系统调用 ID
    /// 
    /// # 实现细节
    /// 如果系统调用 ID 在有效范围内（0-511），则增加对应的计数器。
    pub fn increment_syscall_count(&self, syscall_id: usize) {
        let mut inner = self.inner.exclusive_access();
        if syscall_id < inner.syscall_counts.len() {
            inner.syscall_counts[syscall_id] += 1;
        }
    }

    /// 获取指定系统调用的调用次数
    /// 
    /// # 参数
    /// - `syscall_id`: 系统调用 ID
    /// 
    /// # 返回值
    /// - 如果系统调用 ID 在有效范围内，返回调用次数
    /// - 否则返回 0
    pub fn get_syscall_count(&self, syscall_id: usize) -> isize {
        let inner = self.inner.exclusive_access();
        if syscall_id < inner.syscall_counts.len() {
            inner.syscall_counts[syscall_id] as isize
        } else {
            0
        }
    }

    /// Run the first task in task list.
    ///
    /// Generally, the first task in task list is an idle task (we call it zero process later).
    /// But in ch4, we load apps statically, so the first task is a real app.
    fn run_first_task(&self) -> ! {
        let mut inner = self.inner.exclusive_access();
        let next_task = &mut inner.tasks[0];
        next_task.task_status = TaskStatus::Running;
        let next_task_cx_ptr = &next_task.task_cx as *const TaskContext;
        drop(inner);
        let mut _unused = TaskContext::zero_init();
        // before this, we should drop local variables that must be dropped manually
        unsafe {
            __switch(&mut _unused as *mut _, next_task_cx_ptr);
        }
        panic!("unreachable in run_first_task!");
    }

    /// Change the status of current `Running` task into `Ready`.
    fn mark_current_suspended(&self) {
        let mut inner = self.inner.exclusive_access();
        let cur = inner.current_task;
        inner.tasks[cur].task_status = TaskStatus::Ready;
    }

    /// Change the status of current `Running` task into `Exited`.
    fn mark_current_exited(&self) {
        let mut inner = self.inner.exclusive_access();
        let cur = inner.current_task;
        inner.tasks[cur].task_status = TaskStatus::Exited;
    }

    /// Find next task to run and return task id.
    ///
    /// In this case, we only return the first `Ready` task in task list.
    fn find_next_task(&self) -> Option<usize> {
        let inner = self.inner.exclusive_access();
        let current = inner.current_task;
        (current + 1..current + self.num_app + 1)
            .map(|id| id % self.num_app)
            .find(|id| inner.tasks[*id].task_status == TaskStatus::Ready)
    }

    /// Get the current 'Running' task's token.
    fn get_current_token(&self) -> usize {
        let inner = self.inner.exclusive_access();
        inner.tasks[inner.current_task].get_user_token()
    }

    /// Get the current 'Running' task's trap contexts.
    fn get_current_trap_cx(&self) -> &'static mut TrapContext {
        let inner = self.inner.exclusive_access();
        inner.tasks[inner.current_task].get_trap_cx()
    }

    /// Change the current 'Running' task's program break
    pub fn change_current_program_brk(&self, size: i32) -> Option<usize> {
        let mut inner = self.inner.exclusive_access();
        let cur = inner.current_task;
        inner.tasks[cur].change_program_brk(size)
    }

    /// 检查当前任务的虚拟页面是否已经被映射
    pub fn is_current_page_mapped(&self, vpn: crate::mm::VirtPageNum) -> bool {
        let inner = self.inner.exclusive_access();
        let cur = inner.current_task;
        inner.tasks[cur].memory_set.is_mapped(vpn)
    }

    /// 映射当前任务的虚拟页面到物理页面
    pub fn map_current_page(&self, vpn: crate::mm::VirtPageNum, ppn: crate::mm::PhysPageNum, flags: crate::mm::PTEFlags) {
        let mut inner = self.inner.exclusive_access();
        let cur = inner.current_task;
        inner.tasks[cur].memory_set.map_page(vpn, ppn, flags);
    }

    /// 解除当前任务的虚拟页面映射
    pub fn unmap_current_page(&self, vpn: crate::mm::VirtPageNum) {
        let mut inner = self.inner.exclusive_access();
        let cur = inner.current_task;
        inner.tasks[cur].memory_set.unmap_page(vpn);
    }

    /// Switch current `Running` task to the task we have found,
    /// or there is no `Ready` task and we can exit with all applications completed
    fn run_next_task(&self) {
        if let Some(next) = self.find_next_task() {
            let mut inner = self.inner.exclusive_access();
            let current = inner.current_task;
            inner.tasks[next].task_status = TaskStatus::Running;
            inner.current_task = next;
            let current_task_cx_ptr = &mut inner.tasks[current].task_cx as *mut TaskContext;
            let next_task_cx_ptr = &inner.tasks[next].task_cx as *const TaskContext;
            drop(inner);
            // before this, we should drop local variables that must be dropped manually
            unsafe {
                __switch(current_task_cx_ptr, next_task_cx_ptr);
            }
            // go back to user mode
        } else {
            panic!("All applications completed!");
        }
    }
}

/// Run the first task in task list.
pub fn run_first_task() {
    TASK_MANAGER.run_first_task();
}

/// Switch current `Running` task to the task we have found,
/// or there is no `Ready` task and we can exit with all applications completed
fn run_next_task() {
    TASK_MANAGER.run_next_task();
}

/// Change the status of current `Running` task into `Ready`.
fn mark_current_suspended() {
    TASK_MANAGER.mark_current_suspended();
}

/// Change the status of current `Running` task into `Exited`.
fn mark_current_exited() {
    TASK_MANAGER.mark_current_exited();
}

/// Suspend the current 'Running' task and run the next task in task list.
pub fn suspend_current_and_run_next() {
    mark_current_suspended();
    run_next_task();
}

/// 退出当前运行的任务并运行下一个任务
pub fn exit_current_and_run_next() {
    mark_current_exited();
    run_next_task();
}

/// 获取当前运行任务的页表 token
/// 
/// # 返回值
/// - 当前任务的页表 token，用于虚拟地址翻译
pub fn current_user_token() -> usize {
    TASK_MANAGER.get_current_token()
}

/// 获取当前运行任务的 trap 上下文
/// 
/// # 返回值
/// - 当前任务的 trap 上下文的可变引用
pub fn current_trap_cx() -> &'static mut TrapContext {
    TASK_MANAGER.get_current_trap_cx()
}

/// 增加指定系统调用的调用次数
/// 
/// # 参数
/// - `syscall_id`: 系统调用 ID
/// 
/// # 实现细节
/// 调用任务管理器的方法来增加系统调用计数器
pub fn increment_syscall_count(syscall_id: usize) {
    TASK_MANAGER.increment_syscall_count(syscall_id);
}

/// 获取指定系统调用的调用次数
/// 
/// # 参数
/// - `syscall_id`: 系统调用 ID
/// 
/// # 返回值
/// - 系统调用次数
/// 
/// # 实现细节
/// 调用任务管理器的方法来获取系统调用计数
pub fn get_syscall_count(syscall_id: usize) -> isize {
    TASK_MANAGER.get_syscall_count(syscall_id)
}

/// Change the current 'Running' task's program break
pub fn change_program_brk(size: i32) -> Option<usize> {
    TASK_MANAGER.change_current_program_brk(size)
}

/// 检查当前任务的虚拟页面是否已经被映射
pub fn is_current_page_mapped(vpn: crate::mm::VirtPageNum) -> bool {
    TASK_MANAGER.is_current_page_mapped(vpn)
}

/// 映射当前任务的虚拟页面到物理页面
pub fn map_current_page(vpn: crate::mm::VirtPageNum, ppn: crate::mm::PhysPageNum, flags: crate::mm::PTEFlags) {
    TASK_MANAGER.map_current_page(vpn, ppn, flags);
}

/// 解除当前任务的虚拟页面映射
pub fn unmap_current_page(vpn: crate::mm::VirtPageNum) {
    TASK_MANAGER.unmap_current_page(vpn);
}
