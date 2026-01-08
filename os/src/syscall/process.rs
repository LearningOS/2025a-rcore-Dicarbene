//! Process management syscalls
use crate::mm::translated_byte_buffer;
use crate::task::{change_program_brk, current_user_token, exit_current_and_run_next, get_syscall_count as task_get_syscall_count, suspend_current_and_run_next, is_current_page_mapped, map_current_page, unmap_current_page};
use crate::timer::get_time_us;
use crate::config::PAGE_SIZE;
use crate::mm::{VirtAddr, VirtPageNum, PTEFlags, frame_alloc, VPNRange};
#[derive(Debug)]
#[allow(dead_code)]
pub struct TimeVal {
    pub sec: usize,
    pub usec: usize,
}

/// task exits and submit an exit code
pub fn sys_exit(_exit_code: i32) -> ! {
    trace!("kernel: sys_exit");
    exit_current_and_run_next();
    panic!("Unreachable in sys_exit!");
}

/// current task gives up resources for other tasks
pub fn sys_yield() -> isize {
    trace!("kernel: sys_yield");
    suspend_current_and_run_next();
    0
}

/// 获取当前时间（秒和微秒）
/// 
/// # 参数
/// - `ts`: 指向用户空间 TimeVal 结构体的指针，用于存储时间信息
/// - `_tz`: 时区参数（未使用）
/// 
/// # 返回值
/// - 成功返回 0
/// 
/// # 实现细节
/// 使用虚拟内存管理来安全地访问用户空间内存，即使 TimeVal 结构体跨越两个页面也能正确处理。
/// TimeVal 结构体包含两个字段：
/// - sec: 秒数（8字节）
/// - usec: 微秒数（8字节）
/// 总共 16 字节，可能跨越两个页面。
pub fn sys_get_time(ts: *mut TimeVal, _tz: usize) -> isize {
    trace!("kernel: sys_get_time");
    
    // 获取当前时间（微秒）
    let us = get_time_us();
    
    // 获取当前进程的页表 token，用于虚拟地址翻译
    let token = current_user_token();
    
    // 使用 translated_byte_buffer 将用户空间的虚拟地址转换为物理地址的切片向量
    // 这样可以安全地访问用户空间内存，即使跨越多个页面
    let buffers = translated_byte_buffer(token, ts as *const u8, core::mem::size_of::<TimeVal>());
    
    // 将微秒转换为秒和微秒
    let sec = us / 1_000_000;
    let usec = us % 1_000_000;
    
    // 将秒和微秒转换为小端字节数组
    let sec_bytes = sec.to_le_bytes();
    let usec_bytes = usec.to_le_bytes();
    
    // 遍历所有缓冲区，将时间数据写入用户空间
    // offset 用于跟踪已经写入的字节数
    let mut offset = 0;
    for buffer in buffers {
        let len = buffer.len();
        
        // 处理秒数字段（前8字节）
        if offset < 8 {
            let sec_end = (offset + len).min(8);
            buffer[0..(sec_end - offset)].copy_from_slice(&sec_bytes[offset..sec_end]);
        }
        
        // 处理跨越两个页面的情况：秒数字段和微秒数字段在同一个缓冲区中
        if offset < 8 && offset + len > 8 {
            let usec_start = 8.max(offset);
            buffer[(usec_start - offset)..len].copy_from_slice(&usec_bytes[0..(len - (usec_start - offset))]);
        } else if offset >= 8 {
            // 处理微秒数字段（后8字节）
            let usec_end = (offset + len).min(16);
            buffer[0..(usec_end - offset)].copy_from_slice(&usec_bytes[(offset - 8)..(usec_end - 8)]);
        }
        
        // 更新偏移量
        offset += len;
    }
    
    0
}

/// 系统调用追踪接口
/// 
/// # 参数
/// - `trace_request`: 追踪请求类型
///   - 0: 从用户空间读取一个字节
///   - 1: 向用户空间写入一个字节
///   - 2: 查询指定系统调用的调用次数
/// - `id`: 用户空间地址或系统调用 ID
/// - `data`: 要写入的数据（仅在 trace_request=1 时使用）
/// 
/// # 返回值
/// - trace_request=0: 读取的字节值
/// - trace_request=1: 成功返回 0，失败返回 -1
/// - trace_request=2: 系统调用次数
/// - 其他: -1
/// 
/// # 实现细节
/// 使用虚拟内存管理来安全地访问用户空间内存，即使数据跨越多个页面也能正确处理。
pub fn sys_trace(trace_request: usize, id: usize, data: usize) -> isize {
    trace!("kernel: sys_trace");
    
    // 获取当前进程的页表 token，用于虚拟地址翻译
    let token = current_user_token();
    
    match trace_request {
        // 请求类型 0：从用户空间读取一个字节
        0 => {
            // 将用户空间的虚拟地址转换为物理地址的切片向量
            let buffers = translated_byte_buffer(token, id as *const u8, 1);
            if !buffers.is_empty() {
                // 返回读取到的字节值
                return buffers[0][0] as isize;
            }
            -1
        }
        
        // 请求类型 1：向用户空间写入一个字节
        1 => {
            // 将用户空间的虚拟地址转换为物理地址的可变切片向量
            let mut buffers = translated_byte_buffer(token, id as *const u8, 1);
            if !buffers.is_empty() {
                // 写入数据
                buffers[0][0] = data as u8;
                return 0;
            }
            -1
        }
        
        // 请求类型 2：查询指定系统调用的调用次数
        2 => {
            // 将用户空间的虚拟地址转换为物理地址的切片向量
            // 读取一个 usize 类型的系统调用 ID
            let buffers = translated_byte_buffer(token, id as *const u8, core::mem::size_of::<usize>());
            if !buffers.is_empty() {
                // 创建一个字节数组来存储系统调用 ID
                let mut syscall_id_bytes = [0u8; core::mem::size_of::<usize>()];
                let mut offset = 0;
                
                // 遍历所有缓冲区，将数据复制到字节数组中
                // 处理跨越多个页面的情况
                for buffer in buffers {
                    let len = buffer.len();
                    syscall_id_bytes[offset..offset + len].copy_from_slice(&buffer[..len]);
                    offset += len;
                }
                
                // 将字节数组转换为系统调用 ID
                let syscall_id = usize::from_le_bytes(syscall_id_bytes);
                
                // 返回该系统调用的调用次数
                return task_get_syscall_count(syscall_id);
            }
            -1
        }
        
        // 无效的请求类型
        _ => -1
    }
}

/// 内存映射系统调用
/// 
/// # 参数
/// - `start`: 需要映射的虚存起始地址，要求按页对齐
/// - `len`: 映射字节长度，可以为 0
/// - `prot`: 内存保护属性
///   - 第 0 位表示是否可读
///   - 第 1 位表示是否可写
///   - 第 2 位表示是否可执行
///   - 其他位无效且必须为 0
/// 
/// # 返回值
/// - 执行成功则返回 0
/// - 错误返回 -1
/// 
/// # 实现细节
/// 申请长度为 len 字节的物理内存，将其映射到 start 开始的虚存。
/// 可能的错误：
/// 1. start 没有按页大小对齐
/// 2. prot & !0x7 != 0 (prot 其余位必须为0)
/// 3. prot & 0x7 = 0 (这样的内存无意义)
/// 4. [start, start + len) 中存在已经被映射的页
/// 5. 物理内存不足
pub fn sys_mmap(start: usize, len: usize, prot: usize) -> isize {
    trace!("kernel: sys_mmap start={:#x}, len={}, prot={:#x}", start, len, prot);
    
    // 检查 start 是否按页对齐
    if start % PAGE_SIZE != 0 {
        return -1;
    }
    
    // 检查 prot 的其他位是否为 0
    if prot & !0x7 != 0 {
        return -1;
    }
    
    // 检查 prot 是否有意义（至少有一个权限位被设置）
    if prot & 0x7 == 0 {
        return -1;
    }
    
    // 如果 len 为 0，直接返回成功
    if len == 0 {
        return 0;
    }
    
    // 检查 [start, start + len) 中是否存在已经被映射的页
    let start_vpn = VirtPageNum::from(VirtAddr::from(start));
    let end_vpn = VirtPageNum::from(VirtAddr::from(start + len));
    let vpn_range = VPNRange::new(start_vpn, end_vpn);
    
    for vpn in vpn_range {
        if is_current_page_mapped(vpn) {
            return -1;
        }
    }
    
    // 将 prot 转换为 PTEFlags
    let mut flags = PTEFlags::U;
    if prot & 0x1 != 0 {
        flags |= PTEFlags::R;
    }
    if prot & 0x2 != 0 {
        flags |= PTEFlags::W;
    }
    if prot & 0x4 != 0 {
        flags |= PTEFlags::X;
    }
    
    // 为每个页面分配物理内存并建立映射
    let vpn_range = VPNRange::new(start_vpn, end_vpn);
    for vpn in vpn_range {
        let frame = match frame_alloc() {
            Some(f) => f,
            None => return -1,
        };
        let ppn = frame.ppn;
        map_current_page(vpn, ppn, flags);
    }
    
    0
}

/// 解除内存映射系统调用
/// 
/// # 参数
/// - `start`: 需要解除映射的虚存起始地址，要求按页对齐
/// - `len`: 解除映射的字节长度
/// 
/// # 返回值
/// - 执行成功则返回 0
/// - 错误返回 -1
/// 
/// # 实现细节
/// 解除从 start 开始，长度为 len 的内存映射。
/// 可能的错误：
/// 1. start 没有按页大小对齐
/// 2. [start, start + len) 中存在未被映射的页
pub fn sys_munmap(start: usize, len: usize) -> isize {
    trace!("kernel: sys_munmap start={:#x}, len={}", start, len);
    
    // 检查 start 是否按页对齐
    if start % PAGE_SIZE != 0 {
        return -1;
    }
    
    // 如果 len 为 0，直接返回成功
    if len == 0 {
        return 0;
    }
    
    // 检查 [start, start + len) 中是否存在未被映射的页
    let start_vpn = VirtPageNum::from(VirtAddr::from(start));
    let end_vpn = VirtPageNum::from(VirtAddr::from(start + len));
    let vpn_range = VPNRange::new(start_vpn, end_vpn);
    
    for vpn in vpn_range {
        if !is_current_page_mapped(vpn) {
            return -1;
        }
    }
    
    // 解除每个页面的映射
    let vpn_range = VPNRange::new(start_vpn, end_vpn);
    for vpn in vpn_range {
        unmap_current_page(vpn);
    }
    
    0
}
/// change data segment size
pub fn sys_sbrk(size: i32) -> isize {
    trace!("kernel: sys_sbrk");
    if let Some(old_brk) = change_program_brk(size) {
        old_brk as isize
    } else {
        -1
    }
}
