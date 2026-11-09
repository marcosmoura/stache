//! Mach message layout, port-right ownership and the real/fake kernel seam.
//!
//! Layout citations are from `<mach/message.h>` in the macOS SDK
//! (`#pragma pack(push, 4)` at line 291 through `pack(pop)` at 625):
//! `mach_msg_header_t` (24 bytes), `mach_msg_body_t` (4 bytes) and
//! `mach_msg_ool_descriptor_t` (`void *address; boolean_t deallocate : 8;
//! mach_msg_copy_options_t copy : 8; unsigned int pad1 : 8;
//! mach_msg_descriptor_type_t type : 8; mach_msg_size_t size;`, 16 bytes on
//! 64-bit). The complete message is 24 + 4 + 16 = 44 bytes with 4-byte packing,
//! so the 8-byte pointer sits at offset 28.

use std::ffi::{CString, c_char};
use std::marker::PhantomData;
use std::mem::{align_of, offset_of, size_of};
use std::time::Duration;

use super::encode::encode_mach_args;
use super::transport::{BordersTransport, MissingService, Rejection, SendOutcome};

/// Mach service registered by `JankyBorders`.
pub const SERVICE_NAME: &str = "git.felix.borders";

/// Upper bound of one `mach_msg` send (a deliberate bound; legacy used 0 ms for
/// frames and 100 ms for commands).
pub const MACH_SEND_DEADLINE_MAX: Duration = Duration::from_millis(500);

const MACH_PORT_NULL: u32 = 0;
const KERN_SUCCESS: i32 = 0;
const TASK_BOOTSTRAP_PORT: i32 = 4; // task_special_ports.h:78
const MACH_SEND_MSG: i32 = 0x0000_0001; // message.h:709
const MACH_SEND_TIMEOUT: i32 = 0x0000_0010; // message.h:715
const MACH_MSGH_BITS_COMPLEX: u32 = 0x8000_0000; // message.h:149
const MACH_MSG_TYPE_COPY_SEND: u32 = 19; // message.h:243
const MACH_MSG_VIRTUAL_COPY: u8 = 1; // message.h:254
const MACH_MSG_OOL_DESCRIPTOR: u8 = 1; // message.h:282

const MACH_SEND_INVALID_DATA: i32 = 0x1000_0002;
const MACH_SEND_INVALID_DEST: i32 = 0x1000_0003;
/// `MACH_SEND_TIMED_OUT`: the send deadline elapsed; delivery is uncertain.
pub const MACH_SEND_TIMED_OUT: i32 = 0x1000_0004;
const MACH_SEND_INVALID_VOUCHER: i32 = 0x1000_0005;
const MACH_SEND_MSG_TOO_SMALL: i32 = 0x1000_0008;
const MACH_SEND_INVALID_REPLY: i32 = 0x1000_0009;
const MACH_SEND_INVALID_RIGHT: i32 = 0x1000_000a;
const MACH_SEND_INVALID_NOTIFY: i32 = 0x1000_000b;
const MACH_SEND_INVALID_MEMORY: i32 = 0x1000_000c;
const MACH_SEND_NO_BUFFER: i32 = 0x1000_000d;
const MACH_SEND_TOO_LARGE: i32 = 0x1000_000e;
const MACH_SEND_INVALID_TYPE: i32 = 0x1000_000f;
const MACH_SEND_INVALID_HEADER: i32 = 0x1000_0010;
const MACH_SEND_INVALID_TRAILER: i32 = 0x1000_0011;
const MACH_SEND_INVALID_CONTEXT: i32 = 0x1000_0012;
const MACH_SEND_INVALID_OPTIONS: i32 = 0x1000_0013;
const MACH_SEND_INVALID_RT_OOL_SIZE: i32 = 0x1000_0015;
const MACH_SEND_NO_GRANT_DEST: i32 = 0x1000_0016;
const MACH_SEND_MSG_FILTERED: i32 = 0x1000_0017;
const MACH_SEND_AUX_TOO_SMALL: i32 = 0x1000_0018;
const MACH_SEND_AUX_TOO_LARGE: i32 = 0x1000_0019;

/// `mach_msg_header_t`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct MachMsgHeader {
    bits: u32,
    size: u32,
    remote_port: u32,
    local_port: u32,
    voucher_port: u32,
    id: i32,
}

/// `mach_msg_body_t`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct MachMsgBody {
    descriptor_count: u32,
}

/// `mach_msg_ool_descriptor_t`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct MachMsgOolDescriptor {
    address: *const u8,
    deallocate: u8,
    copy: u8,
    pad1: u8,
    type_: u8,
    size: u32,
}

/// Header + body + one OOL descriptor, with the SDK's 4-byte packing.
#[repr(C, packed(4))]
#[derive(Clone, Copy)]
pub struct MachMessage {
    header: MachMsgHeader,
    body: MachMsgBody,
    descriptor: MachMsgOolDescriptor,
}

#[allow(clippy::cast_possible_truncation)]
const MESSAGE_SIZE: u32 = size_of::<MachMessage>() as u32;

const _: () = {
    assert!(MESSAGE_SIZE == 44);
    assert!(size_of::<MachMsgHeader>() == 24);
    assert!(align_of::<MachMsgHeader>() == 4);
    assert!(size_of::<MachMsgBody>() == 4);
    assert!(size_of::<MachMsgOolDescriptor>() == 16);
    assert!(align_of::<MachMsgOolDescriptor>() == 8);
    assert!(size_of::<MachMessage>() == 44);
    assert!(align_of::<MachMessage>() == 4);
    assert!(offset_of!(MachMessage, body) == 24);
    assert!(offset_of!(MachMessage, descriptor) == 28);
    assert!(offset_of!(MachMsgOolDescriptor, address) == 0);
    assert!(offset_of!(MachMsgOolDescriptor, deallocate) == 8);
    assert!(offset_of!(MachMsgOolDescriptor, copy) == 9);
    assert!(offset_of!(MachMsgOolDescriptor, type_) == 11);
    assert!(offset_of!(MachMsgOolDescriptor, size) == 12);
};

impl MachMessage {
    /// Builds a complex one-descriptor message. `payload` must outlive the send:
    /// the descriptor borrows it (`deallocate = 0`, virtual copy).
    #[allow(clippy::cast_possible_truncation)]
    pub const fn new(port: u32, payload: &[u8]) -> Self {
        Self {
            header: MachMsgHeader {
                bits: MACH_MSGH_BITS_COMPLEX | MACH_MSG_TYPE_COPY_SEND,
                size: MESSAGE_SIZE,
                remote_port: port,
                local_port: MACH_PORT_NULL,
                voucher_port: MACH_PORT_NULL,
                id: 0,
            },
            body: MachMsgBody { descriptor_count: 1 },
            descriptor: MachMsgOolDescriptor {
                address: payload.as_ptr(),
                deallocate: 0,
                copy: MACH_MSG_VIRTUAL_COPY,
                pad1: 0,
                type_: MACH_MSG_OOL_DESCRIPTOR,
                size: payload.len() as u32, // bounded by MAX_PAYLOAD_BYTES upstream
            },
        }
    }

    /// The destination port name.
    #[cfg(test)]
    pub const fn remote_port(&self) -> u32 { self.header.remote_port }

    /// The header bits.
    #[cfg(test)]
    pub const fn bits(&self) -> u32 { self.header.bits }

    /// The declared message size.
    pub const fn message_size(&self) -> u32 { self.header.size }

    /// The descriptor count.
    #[cfg(test)]
    pub const fn descriptor_count(&self) -> u32 { self.body.descriptor_count }

    /// The descriptor payload length.
    #[cfg(test)]
    pub const fn payload_len(&self) -> u32 { self.descriptor.size }

    /// The descriptor `deallocate` flag.
    #[cfg(test)]
    pub const fn deallocate_flag(&self) -> u8 { self.descriptor.deallocate }

    /// The descriptor copy option.
    #[cfg(test)]
    pub const fn copy_option(&self) -> u8 { self.descriptor.copy }

    /// The descriptor type.
    #[cfg(test)]
    pub const fn descriptor_type(&self) -> u8 { self.descriptor.type_ }

    /// The payload pointer (valid only while the sender's buffer lives).
    #[cfg(test)]
    pub const fn payload_ptr(&self) -> *const u8 { self.descriptor.address }
}

/// Converts a deadline to the `mach_msg` timeout in milliseconds, clamped to
/// `1..=500`. Zero would make the send a non-waiting try; a larger value breaks
/// the per-message bound.
#[must_use]
pub fn send_timeout_ms(deadline: Duration) -> u32 {
    let capped = deadline.min(MACH_SEND_DEADLINE_MAX).as_millis().max(1);
    u32::try_from(capped).unwrap_or(500)
}

/// Maps a `mach_msg` send result to an outcome.
///
/// | `kern_return_t` | outcome |
/// |---|---|
/// | `KERN_SUCCESS` (0) | `Sent` |
/// | `MACH_SEND_INVALID_{DEST,RIGHT}` (dead destination) | `MissingService` (the right is released; no CLI fallback) |
/// | `MACH_SEND_INVALID_{DATA,VOUCHER,REPLY,NOTIFY,MEMORY,TYPE,HEADER,TRAILER,CONTEXT,OPTIONS,RT_OOL_SIZE}`, `MACH_SEND_{MSG_TOO_SMALL,NO_BUFFER,TOO_LARGE,NO_GRANT_DEST,MSG_FILTERED,AUX_TOO_SMALL,AUX_TOO_LARGE}` | `Rejected(Kernel(code))` (validation failures before enqueue) |
/// | `MACH_SEND_TIMED_OUT`, `MACH_SEND_INTERRUPTED`, `MACH_SEND_IN_PROGRESS`, any other code | `Uncertain(code)` |
///
/// A timed-out send is reported as uncertain, never as not sent, so callers do
/// not fall back to the CLI after it.
#[must_use]
pub const fn classify_send_result(code: i32) -> SendOutcome {
    match code {
        KERN_SUCCESS => SendOutcome::Sent,
        // A dead destination proves the service is gone in this attempt: it is a
        // missing service, never a reason to run the CLI (which could become the
        // service).
        MACH_SEND_INVALID_DEST | MACH_SEND_INVALID_RIGHT => SendOutcome::MissingService,
        MACH_SEND_INVALID_DATA
        | MACH_SEND_INVALID_VOUCHER
        | MACH_SEND_MSG_TOO_SMALL
        | MACH_SEND_INVALID_REPLY
        | MACH_SEND_INVALID_NOTIFY
        | MACH_SEND_INVALID_MEMORY
        | MACH_SEND_NO_BUFFER
        | MACH_SEND_TOO_LARGE
        | MACH_SEND_INVALID_TYPE
        | MACH_SEND_INVALID_HEADER
        | MACH_SEND_INVALID_TRAILER
        | MACH_SEND_INVALID_CONTEXT
        | MACH_SEND_INVALID_OPTIONS
        | MACH_SEND_INVALID_RT_OOL_SIZE
        | MACH_SEND_NO_GRANT_DEST
        | MACH_SEND_MSG_FILTERED
        | MACH_SEND_AUX_TOO_SMALL
        | MACH_SEND_AUX_TOO_LARGE => SendOutcome::Rejected(Rejection::Kernel(code)),
        // MACH_SEND_TIMED_OUT, MACH_SEND_INTERRUPTED, MACH_SEND_IN_PROGRESS, unknown.
        _ => SendOutcome::Uncertain(code),
    }
}

/// The kernel calls the connection needs. Real in [`SystemMach`], fake in tests.
pub trait MachApi {
    /// `bootstrap_look_up`: a send right (name) or the failing code.
    fn look_up(&self, service: &str) -> Result<u32, i32>;
    /// `mach_msg` send-only with `MACH_SEND_TIMEOUT` and `timeout_ms`.
    fn send(&self, message: &mut MachMessage, timeout_ms: u32) -> i32;
    /// `mach_port_deallocate` of one user reference of a send right.
    fn deallocate(&self, port: u32);
}

/// Real libSystem calls.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemMach;

#[link(name = "System", kind = "dylib")]
unsafe extern "C" {
    fn bootstrap_look_up(bp: u32, service_name: *const c_char, sp: *mut u32) -> i32;
    fn task_get_special_port(task: u32, which_port: i32, special_port: *mut u32) -> i32;
    fn mach_port_deallocate(task: u32, name: u32) -> i32;
    fn mach_msg(
        msg: *mut MachMessage,
        option: i32,
        send_size: u32,
        rcv_size: u32,
        rcv_name: u32,
        timeout: u32,
        notify: u32,
    ) -> i32;
    static mach_task_self_: u32;
}

fn task_self() -> u32 {
    // SAFETY: `mach_task_self_` is initialised by libSystem before main and never
    // written afterwards.
    unsafe { mach_task_self_ }
}

impl MachApi for SystemMach {
    fn look_up(&self, service: &str) -> Result<u32, i32> {
        let name = CString::new(service).map_err(|_| -1)?;
        let mut bootstrap = MACH_PORT_NULL;
        // SAFETY: the out pointer is a valid local; the returned right is owned
        // by this function and released below.
        let code =
            unsafe { task_get_special_port(task_self(), TASK_BOOTSTRAP_PORT, &raw mut bootstrap) };
        if code != KERN_SUCCESS || bootstrap == MACH_PORT_NULL {
            return Err(if code == KERN_SUCCESS { -1 } else { code });
        }
        let mut port = MACH_PORT_NULL;
        // SAFETY: `name` is NUL-terminated and outlives the call; `port` is a
        // valid out pointer; `bootstrap` is a live send right owned here.
        let result = unsafe { bootstrap_look_up(bootstrap, name.as_ptr(), &raw mut port) };
        self.deallocate(bootstrap);
        if result == KERN_SUCCESS && port != MACH_PORT_NULL {
            Ok(port)
        } else if result == KERN_SUCCESS {
            Err(-1)
        } else {
            Err(result)
        }
    }

    fn send(&self, message: &mut MachMessage, timeout_ms: u32) -> i32 {
        let size = message.message_size();
        // SAFETY: `message` is a fully initialised complex message whose OOL
        // address points into a buffer the caller keeps alive for this call;
        // `deallocate` is 0 so the kernel only copies. The message size matches
        // the struct and MACH_SEND_TIMEOUT bounds the wait by `timeout_ms`.
        unsafe {
            mach_msg(
                std::ptr::from_mut(message),
                MACH_SEND_MSG | MACH_SEND_TIMEOUT,
                size,
                0,
                MACH_PORT_NULL,
                timeout_ms,
                MACH_PORT_NULL,
            )
        }
    }

    fn deallocate(&self, port: u32) {
        // SAFETY: `port` is a send right this process owns exactly one user
        // reference of; callers deallocate each right once.
        let _ = unsafe { mach_port_deallocate(task_self(), port) };
    }
}

/// An owned send right to the borders service, confined to one thread.
///
/// Holding the right does not own the service. The right is deallocated exactly
/// once, on [`Self::release`], on a replacing lookup, or on drop.
pub struct Connection<A: MachApi> {
    api: A,
    port: Option<u32>,
    _confined: PhantomData<*const ()>,
}

impl<A: MachApi> Connection<A> {
    pub const fn new(api: A) -> Self {
        Self {
            api,
            port: None,
            _confined: PhantomData,
        }
    }

    pub fn release(&mut self) {
        if let Some(port) = self.port.take() {
            self.api.deallocate(port);
        }
    }

    pub fn lookup(&mut self) -> Result<(), MissingService> {
        self.release();
        match self.api.look_up(SERVICE_NAME) {
            Ok(port) => {
                self.port = Some(port);
                Ok(())
            }
            Err(code) => Err(MissingService { code }),
        }
    }

    pub fn send(&mut self, args: &[String], deadline: Duration) -> SendOutcome {
        let Some(port) = self.port else {
            return SendOutcome::MissingService;
        };
        let payload = match encode_mach_args(args) {
            Ok(payload) => payload,
            Err(error) => return SendOutcome::Rejected(Rejection::InvalidArguments(error)),
        };
        let mut message = MachMessage::new(port, &payload);
        let code = self.api.send(&mut message, send_timeout_ms(deadline));
        // `payload` lives until here: it outlived the synchronous send.
        drop(payload);
        let outcome = classify_send_result(code);
        if matches!(outcome, SendOutcome::MissingService) {
            // The right is dead; drop it so the next attempt must look up again.
            self.release();
        }
        outcome
    }
}

impl<A: MachApi> Drop for Connection<A> {
    fn drop(&mut self) { self.release(); }
}

/// The real borders connection: one owner thread, no statics.
pub struct BordersConnection {
    inner: Connection<SystemMach>,
}

impl BordersConnection {
    /// An unconnected owner. Performs no lookup and holds no right.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            inner: Connection::new(SystemMach),
        }
    }

    /// Deallocates the held right, if any. Sends nothing.
    pub fn release(&mut self) { self.inner.release(); }
}

impl Default for BordersConnection {
    fn default() -> Self { Self::new() }
}

impl std::fmt::Debug for BordersConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BordersConnection")
            .field("connected", &self.inner.port.is_some())
            .finish()
    }
}

impl BordersTransport for BordersConnection {
    fn lookup(&mut self) -> Result<(), MissingService> { self.inner.lookup() }

    fn send(&mut self, args: &[String], deadline: Duration) -> SendOutcome {
        self.inner.send(args, deadline)
    }
}
