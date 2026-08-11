// SPDX-License-Identifier: MPL-2.0

//! Implement the `syslog` system call (`dmesg`).
//!
//! BusyBox `dmesg` reads the kernel log ring via `syslog(2)`:
//! `syslog(10, NULL, 0)` to probe the size, then `syslog(3, buf, len)` to
//! copy the ring (non-consuming, Linux semantics).  `dmesg -c` writes
//! `syslog(5, NULL, 0)`.  The ring itself lives in the logger crate and is
//! also served by `/dev/kmsg` (consume-on-read).

use core::ffi::c_int;

use super::SyscallReturn;
use crate::prelude::*;

// Linux syslog(2) action types (linux/syslog.h).
const SYSLOG_ACTION_READ_ALL: c_int = 3;
const SYSLOG_ACTION_READ_CLEAR: c_int = 4;
const SYSLOG_ACTION_CLEAR: c_int = 5;
const SYSLOG_ACTION_SIZE_BUFFER: c_int = 10;

pub fn sys_syslog(type_: c_int, bufp: Vaddr, len: c_int, ctx: &Context) -> Result<SyscallReturn> {
    match type_ {
        SYSLOG_ACTION_READ_ALL | SYSLOG_ACTION_READ_CLEAR => {
            if len <= 0 {
                return_errno_with_message!(Errno::EINVAL, "invalid buffer length");
            }
            let user_space = ctx.user_space();
            let mut writer = user_space.writer(bufp, len as usize)?;
            let n = if type_ == SYSLOG_ACTION_READ_CLEAR {
                aster_logger::kmsg::read_all_clear(&mut writer)?
            } else {
                aster_logger::kmsg::read_all(&mut writer)?
            };
            Ok(SyscallReturn::Return(n as isize))
        }
        SYSLOG_ACTION_CLEAR => {
            aster_logger::kmsg::clear();
            Ok(SyscallReturn::Return(0))
        }
        SYSLOG_ACTION_SIZE_BUFFER => {
            let size = aster_logger::kmsg::size_now();
            Ok(SyscallReturn::Return(size as isize))
        }
        _ => return_errno_with_message!(Errno::EINVAL, "unsupported syslog action"),
    }
}
