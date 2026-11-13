//! The production SMC connection: public `IOKit` only (`AppleSMC` service,
//! `IOServiceOpen`, `IOConnectCallStructMethod`), the protocol of
//! smcFanControl / `smctemp` / Stats. No private API, no `IOHIDEventSystemClient`.

use std::collections::HashMap;
use std::ffi::{CStr, c_char, c_int, c_void};

use super::SystemReadError;
use super::smc::{SMC_MAX_BYTES, SmcConnection, SmcKey, SmcOpener, SmcRaw};

type MachPort = u32;
type KernReturn = c_int;

const KERNEL_INDEX_SMC: u32 = 2;
const CMD_READ_BYTES: u8 = 5;
const CMD_READ_KEY_INFO: u8 = 9;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOServiceMatching(name: *const c_char) -> *mut c_void;
    fn IOServiceGetMatchingServices(
        main_port: MachPort,
        matching: *mut c_void,
        existing: *mut MachPort,
    ) -> KernReturn;
    fn IOIteratorNext(iterator: MachPort) -> MachPort;
    fn IOObjectRelease(object: MachPort) -> KernReturn;
    fn IOServiceOpen(
        service: MachPort,
        owning_task: MachPort,
        kind: u32,
        connect: *mut MachPort,
    ) -> KernReturn;
    fn IOServiceClose(connect: MachPort) -> KernReturn;
    fn IOConnectCallStructMethod(
        connect: MachPort,
        selector: u32,
        input: *const c_void,
        input_size: usize,
        output: *mut c_void,
        output_size: *mut usize,
    ) -> KernReturn;
}

unsafe extern "C" {
    static mach_task_self_: MachPort;
    fn sysctlbyname(
        name: *const c_char,
        old: *mut c_void,
        old_len: *mut usize,
        new: *mut c_void,
        new_len: usize,
    ) -> c_int;
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Vers {
    major: u8,
    minor: u8,
    build: u8,
    reserved: u8,
    release: u16,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct PLimit {
    version: u16,
    length: u16,
    cpu: u32,
    gpu: u32,
    mem: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct KeyInfo {
    data_size: u32,
    data_type: u32,
    attributes: u8,
}

/// `SMCKeyData_t` (80 bytes, C layout) from `smctemp_types.h`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct KeyData {
    key: u32,
    vers: Vers,
    p_limit: PLimit,
    key_info: KeyInfo,
    result: u8,
    status: u8,
    data8: u8,
    data32: u32,
    bytes: [u8; SMC_MAX_BYTES],
}

const _: () = assert!(size_of::<KeyData>() == 80);

/// Opens `AppleSMC` over `IOKit`.
#[derive(Clone, Copy, Debug, Default)]
pub struct IoKitSmc;

impl SmcOpener for IoKitSmc {
    fn open(&self) -> Result<Box<dyn SmcConnection>, SystemReadError> {
        // SAFETY: `IOServiceMatching` returns a retained dictionary (or null)
        // that `IOServiceGetMatchingServices` consumes; the iterator and the
        // service are released here, and the connection is owned by
        // `IoKitConnection`, which closes it exactly once on drop.
        unsafe {
            let matching = IOServiceMatching(c"AppleSMC".as_ptr());
            if matching.is_null() {
                return Err(SystemReadError::new("smc matching dictionary unavailable"));
            }
            let mut iterator: MachPort = 0;
            let found = IOServiceGetMatchingServices(0, matching, &raw mut iterator);
            if found != 0 {
                return Err(SystemReadError::new(format!("smc lookup failed ({found:#x})")));
            }
            let service = IOIteratorNext(iterator);
            IOObjectRelease(iterator);
            if service == 0 {
                return Err(SystemReadError::new("no AppleSMC service"));
            }
            let mut connect: MachPort = 0;
            let opened = IOServiceOpen(service, mach_task_self_, 0, &raw mut connect);
            IOObjectRelease(service);
            if opened != 0 || connect == 0 {
                return Err(SystemReadError::new(format!("smc open failed ({opened:#x})")));
            }
            Ok(Box::new(IoKitConnection { connect, infos: HashMap::new() }))
        }
    }
}

struct IoKitConnection {
    connect: MachPort,
    /// Key info (size, type) per key, so each key costs one call after the
    /// first (the cache `smctemp` and smcFanControl keep).
    infos: HashMap<u32, KeyInfo>,
}

impl IoKitConnection {
    fn call(&self, input: &KeyData) -> Result<KeyData, SystemReadError> {
        let mut output = KeyData::default();
        let mut size = size_of::<KeyData>();
        // SAFETY: both pointers reference live `repr(C)` `KeyData` values of the
        // size passed, for the duration of the call; `self.connect` is an open
        // connection owned by `self`.
        let status = unsafe {
            IOConnectCallStructMethod(
                self.connect,
                KERNEL_INDEX_SMC,
                std::ptr::from_ref(input).cast(),
                size_of::<KeyData>(),
                (&raw mut output).cast(),
                &raw mut size,
            )
        };
        if status == 0 {
            Ok(output)
        } else {
            Err(SystemReadError::new(format!("smc call failed ({status:#x})")))
        }
    }
}

impl SmcConnection for IoKitConnection {
    fn read(&mut self, key: SmcKey) -> Result<Option<SmcRaw>, SystemReadError> {
        let code = key.code();
        let info = if let Some(info) = self.infos.get(&code) {
            *info
        } else {
            let output = self.call(&KeyData {
                key: code,
                data8: CMD_READ_KEY_INFO,
                ..KeyData::default()
            })?;
            if output.result != 0 || output.key_info.data_size == 0 {
                return Ok(None);
            }
            self.infos.insert(code, output.key_info);
            output.key_info
        };
        let size = info.data_size as usize;
        if size > SMC_MAX_BYTES {
            return Ok(None);
        }
        let output = self.call(&KeyData {
            key: code,
            key_info: KeyInfo {
                data_size: info.data_size,
                ..KeyInfo::default()
            },
            data8: CMD_READ_BYTES,
            ..KeyData::default()
        })?;
        if output.result != 0 {
            return Ok(None);
        }
        Ok(Some(SmcRaw {
            data_type: info.data_type.to_be_bytes(),
            bytes: output.bytes[..size].to_vec(),
        }))
    }
}

impl Drop for IoKitConnection {
    fn drop(&mut self) {
        // SAFETY: `connect` was opened by `IoKitSmc::open` and is closed once.
        unsafe { IOServiceClose(self.connect) };
    }
}

/// `machdep.cpu.brand_string`, or an empty string when it cannot be read.
#[must_use]
pub fn cpu_brand() -> String {
    let mut buffer = [0_u8; 256];
    let mut length = buffer.len();
    // SAFETY: the name is a NUL-terminated literal; `buffer` and `length`
    // describe the same writable 256 bytes.
    let status = unsafe {
        sysctlbyname(
            c"machdep.cpu.brand_string".as_ptr(),
            buffer.as_mut_ptr().cast(),
            &raw mut length,
            std::ptr::null_mut(),
            0,
        )
    };
    if status != 0 {
        return String::new();
    }
    CStr::from_bytes_until_nul(&buffer).map_or_default(|text| text.to_string_lossy().into_owned())
}
