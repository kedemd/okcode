//! A mirror of include/okdb_bridge.h (ABI 1.0). Keep field order and types exactly as the header.
#![allow(non_camel_case_types, dead_code)]
use std::ffi::{c_char, c_void};

pub type okdb_status = i32;
pub const OKDB_OK: okdb_status = 0;
pub const OKDB_ERR: okdb_status = 1;
pub const OKDB_INVALID: okdb_status = 2;
pub const OKDB_WRONG_THREAD: okdb_status = 3;
pub const OKDB_CLOSED: okdb_status = 4;
pub const OKDB_PANIC: okdb_status = 5;
pub const OKDB_UNSUPPORTED: okdb_status = 6;

pub const OKDB_BRIDGE_ABI_MAJOR: u32 = 1;
pub const OKDB_OPEN_OWNER_THREAD: u32 = 0x1;
pub const OKDB_OPEN_DEFER: u32 = 0x2;

pub type okdb_handle = u64;
pub type okdb_done_fn = unsafe extern "C" fn(ctx: *mut c_void, status: okdb_status, reply: *const u8, reply_len: usize);

#[repr(C)]
pub struct OkdbBridgeHostV1 {
    pub struct_size: u32,
    pub reserved: u32,
    pub ctx: *mut c_void,
    pub call: unsafe extern "C" fn(ctx: *mut c_void, op: *const c_char, args: *const u8, args_len: usize, out: *mut *mut u8, out_len: *mut usize) -> okdb_status,
    pub release: Option<unsafe extern "C" fn(ctx: *mut c_void, id: u64)>,
    pub free_reply: Option<unsafe extern "C" fn(ctx: *mut c_void, ptr: *mut u8, len: usize)>,
    pub drop: Option<unsafe extern "C" fn(ctx: *mut c_void)>,
}

#[repr(C)]
pub struct OkdbBridgeV1 {
    pub abi_major: u32,
    pub abi_minor: u32,
    pub struct_size: u32,
    pub reserved: u32,
    pub version: unsafe extern "C" fn() -> *const c_char,
    pub open: unsafe extern "C" fn(path: *const c_char, opts: *const u8, opts_len: usize, flags: u32, out_h: *mut okdb_handle, err: *mut *mut u8, err_len: *mut usize) -> okdb_status,
    pub attach: unsafe extern "C" fn(native_handle: u64, out_h: *mut okdb_handle, err: *mut *mut u8, err_len: *mut usize) -> okdb_status,
    pub close: unsafe extern "C" fn(h: okdb_handle) -> okdb_status,
    pub dispatch: unsafe extern "C" fn(h: okdb_handle, req: *const u8, req_len: usize, out: *mut *mut u8, out_len: *mut usize) -> okdb_status,
    pub dispatch_batch: unsafe extern "C" fn(h: okdb_handle, reqs: *const u8, reqs_len: usize, out: *mut *mut u8, out_len: *mut usize) -> okdb_status,
    pub submit: unsafe extern "C" fn(h: okdb_handle, req: *const u8, req_len: usize, done: okdb_done_fn, ctx: *mut c_void) -> okdb_status,
    pub free: unsafe extern "C" fn(ptr: *mut u8, len: usize),
    pub set_host: unsafe extern "C" fn(h: okdb_handle, host: *const OkdbBridgeHostV1) -> okdb_status,
    pub pump: unsafe extern "C" fn(h: okdb_handle, budget_ms: u32, next_ms: *mut i64) -> okdb_status,
    pub cli_main: unsafe extern "C" fn(argc: i32, argv: *const *const c_char) -> i32,
    // ── ABI 1.1 (present iff struct_size covers it) ──
    pub submit_batch: unsafe extern "C" fn(h: okdb_handle, reqs: *const u8, reqs_len: usize, done: okdb_done_fn, ctx: *mut c_void) -> okdb_status,
    // ── ABI 1.2 ──
    pub read: unsafe extern "C" fn(h: okdb_handle, req: *const u8, req_len: usize, out: *mut *mut u8, out_len: *mut usize) -> okdb_status,
}

/// The ABI 1.0 size of the table (everything up to cli_main): the minimum okcode accepts.
pub const V1_0_SIZE: usize = std::mem::offset_of!(OkdbBridgeV1, cli_main) + std::mem::size_of::<usize>();
pub const V1_1_SIZE: usize = std::mem::offset_of!(OkdbBridgeV1, submit_batch) + std::mem::size_of::<usize>();
pub const V1_2_SIZE: usize = std::mem::offset_of!(OkdbBridgeV1, read) + std::mem::size_of::<usize>();

pub type okdb_bridge_fn = unsafe extern "C" fn(abi_major: u32) -> *const OkdbBridgeV1;
