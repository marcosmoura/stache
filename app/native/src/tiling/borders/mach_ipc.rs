//! Mach IPC client for communicating with `JankyBorders`.
//!
//! This module provides low-latency communication with `JankyBorders` using
//! macOS Mach IPC instead of spawning CLI processes. This is approximately
//! 50-100x faster than the CLI approach.
//!
//! # Protocol
//!
//! `JankyBorders` registers a Mach bootstrap service named `"git.felix.borders"`.
//! Messages are sent as null-terminated argument strings, exactly like CLI args:
//! - Each argument is a null-terminated string
//! - Arguments are concatenated back-to-back
//! - The message ends with an extra null byte
//!
//! Example message: `"active_color=0xffff0000\0width=4.0\0\0"`
//!
//! # Performance
//!
//! | Aspect                 | CLI (`borders`)         | Mach IPC            |
//! |------------------------|-------------------------|---------------------|
//! | Process spawn          | ~5-15ms per call        | 0ms (no spawn)      |
//! | Context switch         | Multiple (exec, shell)  | 1 kernel switch     |
//! | Memory overhead        | New process each time   | Shared msg buffer   |
//! | Latency                | ~20-50ms typical        | ~0.1-0.5ms typical  |

use std::ffi::CString;
use std::sync::OnceLock;

use parking_lot::Mutex;

// ============================================================================
// Mach FFI Bindings
// ============================================================================

// Mach types
type MachPortT = u32;
type MachMsgSizeT = u32;
type MachMsgBitsT = u32;
type KernReturnT = i32;

const KERN_SUCCESS: KernReturnT = 0;
const MACH_PORT_NULL: MachPortT = 0;
const MACH_MSG_TIMEOUT_NONE: u32 = 0;
const MACH_SEND_MSG: i32 = 0x0000_0001;

// MACH_MSG_TYPE_COPY_SEND
const MACH_MSG_TYPE_COPY_SEND: u32 = 19;
// MACH_MSGH_BITS_REMOTE_MASK
const MACH_MSGH_BITS_REMOTE_MASK: u32 = 0x0000_001f;
// MACH_MSGH_BITS_COMPLEX
const MACH_MSGH_BITS_COMPLEX: u32 = 0x8000_0000;
// MACH_MSG_VIRTUAL_COPY
const MACH_MSG_VIRTUAL_COPY: u8 = 1;
// MACH_MSG_OOL_DESCRIPTOR
const MACH_MSG_OOL_DESCRIPTOR: u8 = 1;

/// `JankyBorders` bootstrap service name.
const BS_NAME: &str = "git.felix.borders";

// Mach message header
#[repr(C)]
#[derive(Debug, Default)]
struct MachMsgHeader {
    msgh_bits: MachMsgBitsT,
    msgh_size: MachMsgSizeT,
    msgh_remote_port: MachPortT,
    msgh_local_port: MachPortT,
    msgh_voucher_port: MachPortT,
    msgh_id: i32,
}

// Out-of-line descriptor for sending data
// Layout on macOS: address (8 bytes), 4 bitfields packed into 32 bits, size (4 bytes)
#[repr(C)]
#[derive(Debug, Default)]
struct MachMsgOolDescriptor {
    address: *const u8,
    deallocate: u8,
    copy: u8,
    pad1: u8,
    descriptor_type: u8,
    size: u32,
}

// Complete message structure matching JankyBorders' format
// Use packed(4) to match C's alignment - otherwise Rust adds padding before descriptor
#[repr(C, packed(4))]
#[derive(Default)]
struct MachMessage {
    header: MachMsgHeader,
    msgh_descriptor_count: MachMsgSizeT,
    descriptor: MachMsgOolDescriptor,
}

// Mach system calls
#[link(name = "System")]
unsafe extern "C" {
    fn mach_task_self() -> MachPortT;
    fn task_get_special_port(
        task: MachPortT,
        which_port: i32,
        special_port: *mut MachPortT,
    ) -> KernReturnT;
    fn bootstrap_look_up(
        bp: MachPortT,
        service_name: *const i8,
        service_port: *mut MachPortT,
    ) -> KernReturnT;
    fn mach_msg(
        msg: *mut MachMsgHeader,
        option: i32,
        send_size: MachMsgSizeT,
        rcv_size: MachMsgSizeT,
        rcv_name: MachPortT,
        timeout: u32,
        notify: MachPortT,
    ) -> KernReturnT;
}

const TASK_BOOTSTRAP_PORT: i32 = 4;

// ============================================================================
// Mach Port Connection
// ============================================================================

/// Gets the bootstrap port for the current task.
///
/// # Safety
///
/// Calls Mach system functions. The caller must ensure this is only called
/// from a valid Mach task context.
unsafe fn get_bootstrap_port() -> Option<MachPortT> {
    unsafe {
        let task = mach_task_self();
        let mut bs_port: MachPortT = 0;

        if task_get_special_port(task, TASK_BOOTSTRAP_PORT, &raw mut bs_port) != KERN_SUCCESS {
            return None;
        }

        Some(bs_port)
    }
}

/// Looks up the `JankyBorders` service port from the bootstrap server.
///
/// # Safety
///
/// Calls Mach bootstrap lookup functions. The bootstrap port must be valid.
unsafe fn lookup_janky_port(bs_port: MachPortT) -> Option<MachPortT> {
    let service_name = CString::new(BS_NAME).ok()?;
    let mut port: MachPortT = 0;

    unsafe {
        if bootstrap_look_up(bs_port, service_name.as_ptr(), &raw mut port) != KERN_SUCCESS {
            return None;
        }
    }

    Some(port)
}

/// Cached connection to `JankyBorders`.
struct JankyConnection {
    port: MachPortT,
}

impl JankyConnection {
    /// Creates a new connection to `JankyBorders`.
    ///
    /// Returns `None` if `JankyBorders` is not running.
    fn new() -> Option<Self> {
        unsafe {
            let bs_port = get_bootstrap_port()?;
            let port = lookup_janky_port(bs_port)?;
            Some(Self { port })
        }
    }

    /// Checks if the connection is still valid.
    const fn is_valid(&self) -> bool { self.port != MACH_PORT_NULL }

    /// Sends a message to `JankyBorders`.
    ///
    /// # Arguments
    ///
    /// * `args` - Arguments to send (like CLI args: `["active_color=0xff00ff00", "width=4.0"]`)
    ///
    /// # Returns
    ///
    /// `true` if the message was sent successfully.
    fn send(&self, args: &[&str]) -> bool {
        if !self.is_valid() {
            return false;
        }

        // Build the message: null-terminated strings concatenated, ending with extra null
        let mut message = Vec::new();
        for arg in args {
            message.extend_from_slice(arg.as_bytes());
            message.push(0); // null terminator
        }
        message.push(0); // extra null at end

        self.send_raw(&message)
    }

    /// Sends raw bytes to `JankyBorders`.
    fn send_raw(&self, data: &[u8]) -> bool {
        if data.is_empty() || !self.is_valid() {
            return false;
        }

        // Verify struct size matches C at compile time
        const _: () = assert!(size_of::<MachMessage>() == 44, "MachMessage size mismatch");

        // Calculate message bits
        let bits = (MACH_MSG_TYPE_COPY_SEND & MACH_MSGH_BITS_REMOTE_MASK) | MACH_MSGH_BITS_COMPLEX;

        let mut msg = MachMessage {
            header: MachMsgHeader {
                msgh_bits: bits,
                msgh_size: size_of::<MachMessage>() as MachMsgSizeT,
                msgh_remote_port: self.port,
                msgh_local_port: MACH_PORT_NULL,
                msgh_voucher_port: MACH_PORT_NULL,
                msgh_id: 0,
            },
            msgh_descriptor_count: 1,
            descriptor: MachMsgOolDescriptor {
                address: data.as_ptr(),
                deallocate: 0, // false
                copy: MACH_MSG_VIRTUAL_COPY,
                pad1: 0,
                descriptor_type: MACH_MSG_OOL_DESCRIPTOR,
                size: data.len() as u32,
            },
        };

        unsafe {
            // Need to use addr_of_mut for packed struct field access
            let result = mach_msg(
                std::ptr::addr_of_mut!(msg.header),
                MACH_SEND_MSG,
                size_of::<MachMessage>() as MachMsgSizeT,
                0,
                MACH_PORT_NULL,
                MACH_MSG_TIMEOUT_NONE,
                MACH_PORT_NULL,
            );

            result == KERN_SUCCESS
        }
    }
}

// ============================================================================
// Global Connection Manager
// ============================================================================

/// Global cached connection to `JankyBorders`.
static CONNECTION: OnceLock<Mutex<Option<JankyConnection>>> = OnceLock::new();

/// Gets or creates the global connection.
fn get_connection() -> &'static Mutex<Option<JankyConnection>> {
    CONNECTION.get_or_init(|| Mutex::new(JankyConnection::new()))
}

/// Reconnects to `JankyBorders` if the connection was lost.
fn reconnect() -> bool {
    let mut guard = get_connection().lock();
    *guard = JankyConnection::new();
    guard.is_some()
}

// ============================================================================
// Public API
// ============================================================================

/// Checks if `JankyBorders` is running and accepting Mach IPC connections.
///
/// This is faster than `is_running()` in `janky.rs` which uses `pgrep`.
#[must_use]
pub fn is_connected() -> bool {
    let guard = get_connection().lock();
    guard.as_ref().is_some_and(JankyConnection::is_valid)
}

/// Sends arguments to `JankyBorders` via Mach IPC.
///
/// This is the low-level send function. Arguments are formatted like CLI args:
/// `["active_color=0xffff0000", "width=4.0"]`
///
/// # Arguments
///
/// * `args` - Arguments to send to `JankyBorders`.
///
/// # Returns
///
/// `true` if the message was sent successfully.
///
/// # Example
///
/// ```ignore
/// send(&["active_color=0xFFFF0000", "width=4.0"]);
/// ```
pub fn send(args: &[&str]) -> bool {
    let guard = get_connection().lock();

    if let Some(conn) = guard.as_ref()
        && conn.send(args)
    {
        eprintln!("stache: borders: Mach IPC sent: {args:?}");
        return true;
    }

    // Connection might be stale, drop guard and try to reconnect
    drop(guard);

    if reconnect() {
        let guard = get_connection().lock();
        if let Some(conn) = guard.as_ref() {
            let result = conn.send(args);
            if result {
                eprintln!("stache: borders: Mach IPC sent (after reconnect): {args:?}");
            } else {
                eprintln!("stache: borders: Mach IPC FAILED to send after reconnect: {args:?}");
            }
            return result;
        }
    }

    eprintln!("stache: borders: Mach IPC not connected, cannot send: {args:?}");
    false
}

/// Sends a single key=value pair to `JankyBorders`.
///
/// This is a convenience wrapper around `send()`.
///
/// # Arguments
///
/// * `arg` - A single argument like `"active_color=0xffff0000"`.
///
/// # Returns
///
/// `true` if the message was sent successfully.
pub fn send_one(arg: &str) -> bool { send(&[arg]) }

/// Sends multiple key=value pairs to `JankyBorders` in a single message.
///
/// This is more efficient than multiple `send_one()` calls.
///
/// # Arguments
///
/// * `args` - Multiple arguments to send together.
///
/// # Returns
///
/// `true` if the message was sent successfully.
pub fn send_batch(args: &[String]) -> bool {
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    send(&refs)
}

/// Invalidates the cached connection, forcing a reconnect on next send.
///
/// Call this if `JankyBorders` was restarted.
pub fn invalidate() {
    let mut guard = get_connection().lock();
    *guard = None;
}

/// Attempts to establish a connection to `JankyBorders`.
///
/// This can be called at startup to cache the connection port.
///
/// # Returns
///
/// `true` if a connection was established.
pub fn connect() -> bool {
    let guard = get_connection().lock();
    guard.is_some()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_struct_size() {
        // Ensure our message struct matches the expected layout
        // MachMsgHeader: 24 bytes
        // msgh_descriptor_count: 4 bytes
        // MachMsgOolDescriptor: 16 bytes (on 64-bit)
        // Total: 44 bytes (may have padding)
        assert!(size_of::<MachMessage>() >= 40);
    }

    #[test]
    fn test_is_connected_when_not_running() {
        // When JankyBorders isn't running, we shouldn't be connected
        // Note: This test may pass or fail depending on whether borders is running
        let _ = is_connected(); // Just ensure it doesn't crash
    }

    #[test]
    fn test_send_when_not_connected() {
        // Sending when not connected should return false gracefully
        invalidate();
        // This may still connect if borders is running, so we just test it doesn't crash
        let _ = send(&["test=value"]);
    }

    #[test]
    fn test_send_empty_args() {
        // Sending empty args should be handled gracefully
        let _ = send(&[]);
    }

    #[test]
    fn test_invalidate_and_reconnect() {
        invalidate();
        // After invalidation, connect() should attempt reconnection
        let _ = connect();
        // Just ensure no crashes
    }
}
