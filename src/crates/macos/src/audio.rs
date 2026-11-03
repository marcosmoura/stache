//! Safe `CoreAudio` device access.
//!
//! Every `CoreAudio` C call used by the audio pilot lives in this module. The
//! [`AudioBackend`] trait is deliberately small: enumerate devices, read and set
//! the default device, ask whether a device uses the HDMI transport, and
//! subscribe to change notifications. Only
//! [`AudioBackend::set_default_device`] mutates the system state, and only when
//! a caller asks for it.
//!
//! Change notifications never block and never allocate in the callback: each
//! one only signals a bounded, coalescing [`AudioWakeup`]. The worker that owns
//! the subscription decides what to do next.
//!
//! `CoreAudio` calls here are thread-agnostic, so none of this needs the main
//! thread.

use std::fmt;
use std::ptr::{NonNull, null};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};

use core_foundation::base::TCFType;
use core_foundation::string::{CFString, CFStringRef};
use objc2_core_audio::{
    AudioObjectAddPropertyListener, AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize,
    AudioObjectID, AudioObjectPropertyAddress, AudioObjectPropertyScope,
    AudioObjectRemovePropertyListener, AudioObjectSetPropertyData,
    kAudioDevicePropertyStreamConfiguration, kAudioDevicePropertyTransportType,
    kAudioDeviceTransportTypeHDMI, kAudioHardwareNoError, kAudioHardwarePropertyDefaultInputDevice,
    kAudioHardwarePropertyDefaultOutputDevice, kAudioHardwarePropertyDevices,
    kAudioObjectPropertyElementMain, kAudioObjectPropertyElementWildcard, kAudioObjectPropertyName,
    kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyScopeInput,
    kAudioObjectPropertyScopeOutput, kAudioObjectSystemObject,
};
use objc2_core_audio_types::{AudioBuffer, AudioBufferList};

/// A `CoreAudio` device identifier.
///
/// Identifiers are ephemeral: they change when a device reconnects, so policy
/// matches names and uses an identifier only for the current session.
pub type AudioDeviceId = u32;

/// Which default device a call refers to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AudioDirection {
    /// The default input device.
    Input,
    /// The default output device.
    Output,
}

impl AudioDirection {
    /// The lowercase spelling used in logs and command output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Input => "input",
            Self::Output => "output",
        }
    }

    const fn default_selector(self) -> u32 {
        match self {
            Self::Input => kAudioHardwarePropertyDefaultInputDevice,
            Self::Output => kAudioHardwarePropertyDefaultOutputDevice,
        }
    }
}

/// One audio device as reported by `CoreAudio`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioDevice {
    /// The session-scoped device identifier.
    pub id: AudioDeviceId,
    /// The human-readable device name.
    pub name: String,
    /// Whether the device exposes at least one input stream.
    pub input: bool,
    /// Whether the device exposes at least one output stream.
    pub output: bool,
}

/// A failed `CoreAudio` call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioError {
    operation: &'static str,
    status: i32,
}

impl AudioError {
    /// Creates an error for one named operation.
    #[must_use]
    pub const fn new(operation: &'static str, status: i32) -> Self { Self { operation, status } }

    /// The operation that failed.
    #[must_use]
    pub const fn operation(&self) -> &'static str { self.operation }

    /// The raw `OSStatus` returned by `CoreAudio`.
    #[must_use]
    pub const fn status(&self) -> i32 { self.status }
}

impl fmt::Display for AudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} failed with OSStatus {}", self.operation, self.status)
    }
}

impl std::error::Error for AudioError {}

/// A bounded, coalescing wakeup for change notifications.
///
/// The underlying channel holds at most one pending signal, so a burst of
/// notifications cannot grow without bound. `signal` never blocks and is safe
/// to call from a `CoreAudio` callback thread. Clones share the same channel.
#[derive(Clone)]
pub struct AudioWakeup {
    sender: SyncSender<()>,
}

impl AudioWakeup {
    /// Creates a wakeup and the receiver its worker waits on.
    #[must_use]
    pub fn channel() -> (Self, Receiver<()>) {
        let (sender, receiver) = sync_channel(1);
        (Self { sender }, receiver)
    }

    /// Records one pending change, dropping the signal when one is already
    /// pending or the worker is gone.
    pub fn signal(&self) { let _ = self.sender.try_send(()); }
}

/// An owned change subscription.
///
/// Dropping the value unregisters the exact property addresses, callback and
/// client data that were used to register. The concrete implementation must
/// keep its callback data alive until that removal returns.
pub trait AudioChangeSubscription: Send {}

/// The native audio service used by the policy layer.
pub trait AudioBackend: Send + Sync {
    /// Every device the system currently reports.
    ///
    /// A failed enumeration returns an empty list, matching the legacy
    /// command's "no devices" behavior.
    fn devices(&self) -> Vec<AudioDevice>;

    /// The current default device, or `None` when `CoreAudio` reports none.
    fn default_device(&self, direction: AudioDirection) -> Option<AudioDevice>;

    /// Changes the default device.
    ///
    /// # Errors
    /// Returns the failing operation when `CoreAudio` rejects the change.
    fn set_default_device(
        &self,
        direction: AudioDirection,
        id: AudioDeviceId,
    ) -> Result<(), AudioError>;

    /// Whether the device reports the HDMI transport.
    fn is_hdmi(&self, id: AudioDeviceId) -> bool;

    /// Registers one listener generation for device and default-device changes.
    ///
    /// The returned subscription owns the listener registration; dropping it
    /// removes every address. A partial registration is rolled back before the
    /// error is returned.
    ///
    /// # Errors
    /// Returns the failing registration when `CoreAudio` refuses a listener.
    fn subscribe(
        &self,
        wakeup: AudioWakeup,
    ) -> Result<Box<dyn AudioChangeSubscription>, AudioError>;
}

/// The real `CoreAudio` backend.
#[derive(Clone, Copy, Debug, Default)]
pub struct CoreAudioBackend;

impl AudioBackend for CoreAudioBackend {
    fn devices(&self) -> Vec<AudioDevice> {
        let Ok(ids) = device_ids() else {
            return Vec::new();
        };
        ids.into_iter()
            .filter_map(|id| {
                let name = device_name(id).ok()?;
                Some(AudioDevice {
                    id,
                    name,
                    input: supports_scope(id, kAudioObjectPropertyScopeInput),
                    output: supports_scope(id, kAudioObjectPropertyScopeOutput),
                })
            })
            .collect()
    }

    fn default_device(&self, direction: AudioDirection) -> Option<AudioDevice> {
        let address = address(
            direction.default_selector(),
            kAudioObjectPropertyScopeGlobal,
            kAudioObjectPropertyElementMain,
        );
        let id = get_property_u32(system_object(), &address).ok()?;
        if id == 0 {
            return None;
        }
        let name = device_name(id).ok()?;
        Some(AudioDevice {
            id,
            name,
            input: supports_scope(id, kAudioObjectPropertyScopeInput),
            output: supports_scope(id, kAudioObjectPropertyScopeOutput),
        })
    }

    fn set_default_device(
        &self,
        direction: AudioDirection,
        id: AudioDeviceId,
    ) -> Result<(), AudioError> {
        let address = address(
            direction.default_selector(),
            kAudioObjectPropertyScopeGlobal,
            kAudioObjectPropertyElementMain,
        );
        let size = std::mem::size_of::<AudioDeviceId>();
        // SAFETY: the address and the identifier outlive the call, the size
        // matches the pointed-to value, and there is no qualifier data.
        let status = unsafe {
            AudioObjectSetPropertyData(
                system_object(),
                NonNull::from(&address),
                0,
                null(),
                u32::try_from(size).expect("device identifier fits in u32"),
                NonNull::from(&id).cast(),
            )
        };
        status_result("set default audio device", status)
    }

    fn is_hdmi(&self, id: AudioDeviceId) -> bool {
        let address = address(
            kAudioDevicePropertyTransportType,
            kAudioObjectPropertyScopeGlobal,
            kAudioObjectPropertyElementMain,
        );
        get_property_u32(id, &address)
            .is_ok_and(|transport| transport == kAudioDeviceTransportTypeHDMI)
    }

    fn subscribe(
        &self,
        wakeup: AudioWakeup,
    ) -> Result<Box<dyn AudioChangeSubscription>, AudioError> {
        let wakeup = Box::new(wakeup);
        let client_data = std::ptr::from_ref::<AudioWakeup>(wakeup.as_ref()).cast_mut().cast();
        let addresses = listener_addresses();
        register_listeners(
            &addresses,
            |address| {
                // SAFETY: the address is a live reference, the listener has the
                // required signature, and `client_data` points at the boxed
                // wakeup kept alive by the returned subscription.
                let status = unsafe {
                    AudioObjectAddPropertyListener(
                        system_object(),
                        NonNull::from(address),
                        Some(property_listener),
                        client_data,
                    )
                };
                status_result("add audio property listener", status)
            },
            |address| {
                // SAFETY: this removes the exact address/callback/data that
                // were registered and have not been removed yet.
                unsafe {
                    let _ = AudioObjectRemovePropertyListener(
                        system_object(),
                        NonNull::from(address),
                        Some(property_listener),
                        client_data,
                    );
                }
            },
        )?;
        Ok(Box::new(CoreAudioSubscription { addresses, wakeup }))
    }
}

const fn system_object() -> AudioObjectID { kAudioObjectSystemObject as AudioObjectID }

const fn address(
    selector: u32,
    scope: AudioObjectPropertyScope,
    element: u32,
) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: scope,
        mElement: element,
    }
}

const fn listener_addresses() -> [AudioObjectPropertyAddress; 3] {
    [
        address(
            kAudioHardwarePropertyDefaultOutputDevice,
            kAudioObjectPropertyScopeGlobal,
            kAudioObjectPropertyElementMain,
        ),
        address(
            kAudioHardwarePropertyDefaultInputDevice,
            kAudioObjectPropertyScopeGlobal,
            kAudioObjectPropertyElementMain,
        ),
        address(
            kAudioHardwarePropertyDevices,
            kAudioObjectPropertyScopeGlobal,
            kAudioObjectPropertyElementMain,
        ),
    ]
}

/// Registers every address, removing the already-added ones if one fails.
fn register_listeners<A, R>(
    addresses: &[AudioObjectPropertyAddress; 3],
    add: A,
    remove: R,
) -> Result<(), AudioError>
where
    A: Fn(&AudioObjectPropertyAddress) -> Result<(), AudioError>,
    R: Fn(&AudioObjectPropertyAddress),
{
    let mut registered: Vec<&AudioObjectPropertyAddress> = Vec::with_capacity(addresses.len());
    for address in addresses {
        match add(address) {
            Ok(()) => registered.push(address),
            Err(error) => {
                for added in registered {
                    remove(added);
                }
                return Err(error);
            }
        }
    }
    Ok(())
}

/// The live listener generation owned by one subscription.
struct CoreAudioSubscription {
    addresses: [AudioObjectPropertyAddress; 3],
    /// Keeps the callback data at a stable address until listeners are removed.
    wakeup: Box<AudioWakeup>,
}

impl AudioChangeSubscription for CoreAudioSubscription {}

impl Drop for CoreAudioSubscription {
    fn drop(&mut self) {
        let client_data = std::ptr::from_ref::<AudioWakeup>(self.wakeup.as_ref()).cast_mut().cast();
        for address in &self.addresses {
            // SAFETY: these are the exact addresses, callback and client data
            // used during registration. The boxed wakeup is still alive for the
            // whole removal loop.
            unsafe {
                let _ = AudioObjectRemovePropertyListener(
                    system_object(),
                    NonNull::from(address),
                    Some(property_listener),
                    client_data,
                );
            }
        }
    }
}

/// The `CoreAudio` callback.
///
/// It only signals the bounded wakeup; it never enumerates devices, takes a
/// lock or allocates.
///
/// # Safety
///
/// `client_data` must be a valid pointer to the `AudioWakeup` kept alive by the
/// subscription that registered this callback.
unsafe extern "C-unwind" fn property_listener(
    _object: AudioObjectID,
    _count: u32,
    _addresses: NonNull<AudioObjectPropertyAddress>,
    client_data: *mut std::ffi::c_void,
) -> i32 {
    if !client_data.is_null() {
        // SAFETY: the subscription owns the boxed wakeup until it has removed
        // this listener, so the pointer is valid here.
        let wakeup = unsafe { &*client_data.cast::<AudioWakeup>() };
        wakeup.signal();
    }
    kAudioHardwareNoError
}

const fn status_result(operation: &'static str, status: i32) -> Result<(), AudioError> {
    if status == kAudioHardwareNoError {
        Ok(())
    } else {
        Err(AudioError::new(operation, status))
    }
}

fn get_property_size(
    object: AudioObjectID,
    address: &AudioObjectPropertyAddress,
) -> Result<u32, AudioError> {
    let mut size = 0u32;
    // SAFETY: the address and size output are valid for the call and there is
    // no qualifier data.
    let status = unsafe {
        AudioObjectGetPropertyDataSize(
            object,
            NonNull::from(address),
            0,
            null(),
            NonNull::from(&mut size),
        )
    };
    status_result("read audio property size", status).map(|()| size)
}

fn get_property_data(
    object: AudioObjectID,
    address: &AudioObjectPropertyAddress,
    buffer: &mut [u8],
) -> Result<u32, AudioError> {
    let mut size =
        u32::try_from(buffer.len()).map_err(|_| AudioError::new("read audio property", -1))?;
    let Some(data) = NonNull::new(buffer.as_mut_ptr()) else {
        return Err(AudioError::new("read audio property", -1));
    };
    // SAFETY: the address is live, the buffer has the reported capacity, and
    // `size` is updated with the number of bytes written.
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            NonNull::from(address),
            0,
            null(),
            NonNull::from(&mut size),
            data.cast(),
        )
    };
    status_result("read audio property", status).map(|()| size)
}

fn get_property_u32(
    object: AudioObjectID,
    address: &AudioObjectPropertyAddress,
) -> Result<u32, AudioError> {
    let mut value = 0u32;
    let mut size = 4u32;
    // SAFETY: the address is live and the output points at a `u32` whose size
    // is passed exactly.
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            NonNull::from(address),
            0,
            null(),
            NonNull::from(&mut size),
            NonNull::from(&mut value).cast(),
        )
    };
    status_result("read audio property", status).map(|()| value)
}

fn device_ids() -> Result<Vec<AudioDeviceId>, AudioError> {
    let address = address(
        kAudioHardwarePropertyDevices,
        kAudioObjectPropertyScopeGlobal,
        kAudioObjectPropertyElementMain,
    );
    let mut size = get_property_size(system_object(), &address)?;
    let capacity = usize::try_from(size)
        .map_err(|_| AudioError::new("read audio device list", -1))?
        / std::mem::size_of::<AudioDeviceId>();
    let mut ids = vec![0u32; capacity];
    if ids.is_empty() {
        return Ok(ids);
    }
    // SAFETY: `ids` is initialized and large enough for `size` bytes, matching
    // what `CoreAudio` reported for this property.
    let status = unsafe {
        AudioObjectGetPropertyData(
            system_object(),
            NonNull::from(&address),
            0,
            null(),
            NonNull::from(&mut size),
            NonNull::new(ids.as_mut_ptr()).expect("non-empty").cast(),
        )
    };
    status_result("read audio device list", status)?;
    let written = usize::try_from(size)
        .map_err(|_| AudioError::new("read audio device list", -1))?
        / std::mem::size_of::<AudioDeviceId>();
    ids.truncate(written.min(ids.len()));
    Ok(ids)
}

fn device_name(id: AudioDeviceId) -> Result<String, AudioError> {
    let address = address(
        kAudioObjectPropertyName,
        kAudioObjectPropertyScopeGlobal,
        kAudioObjectPropertyElementMain,
    );
    let mut name: CFStringRef = null();
    let mut size = u32::try_from(std::mem::size_of::<CFStringRef>())
        .map_err(|_| AudioError::new("read audio device name", -1))?;
    // SAFETY: the property returns one retained `CFString` into `name`, whose
    // size is passed exactly.
    let status = unsafe {
        AudioObjectGetPropertyData(
            id,
            NonNull::from(&address),
            0,
            null(),
            NonNull::from(&mut size),
            NonNull::from(&mut name).cast(),
        )
    };
    status_result("read audio device name", status)?;
    let Some(name) = NonNull::new(name.cast_mut()) else {
        return Err(AudioError::new("read audio device name", -1));
    };
    // SAFETY: `CoreAudio` returns a +1 retained CFString, so wrapping under the
    // create rule balances that reference exactly once.
    let name = unsafe { CFString::wrap_under_create_rule(name.as_ptr()) };
    Ok(name.to_string())
}

fn supports_scope(id: AudioDeviceId, scope: AudioObjectPropertyScope) -> bool {
    let address = address(
        kAudioDevicePropertyStreamConfiguration,
        scope,
        kAudioObjectPropertyElementWildcard,
    );
    let Ok(size) = get_property_size(id, &address) else {
        return false;
    };
    let Ok(size) = usize::try_from(size) else {
        return false;
    };
    if size == 0 {
        return false;
    }
    // `u64` storage keeps the byte buffer aligned for the `AudioBufferList` the
    // system writes into it.
    let mut storage = vec![0u64; size.div_ceil(std::mem::size_of::<u64>())];
    // SAFETY: the storage is at least `size` bytes and every byte is
    // initialized, so exposing it as a mutable byte slice is in bounds.
    let bytes = unsafe { std::slice::from_raw_parts_mut(storage.as_mut_ptr().cast::<u8>(), size) };
    let Ok(written) = get_property_data(id, &address, bytes) else {
        return false;
    };
    // SAFETY: `CoreAudio` filled `written` bytes of the aligned storage with an
    // AudioBufferList; the helper validates the reported counts first.
    unsafe { buffer_list_has_channels(storage.as_ptr().cast::<u8>(), written) }
}

/// Whether any stream buffer in the list has at least one channel.
///
/// # Safety
///
/// `data` must point at `size` initialized bytes containing an
/// `AudioBufferList` as written by `CoreAudio`.
unsafe fn buffer_list_has_channels(data: *const u8, size: u32) -> bool {
    let size = usize::try_from(size).unwrap_or(0);
    if size < std::mem::size_of::<u32>() {
        return false;
    }
    // SAFETY: at least the buffer count is inside the written region. `data`
    // may be byte-aligned, so every field is read unaligned.
    let count = usize::try_from(unsafe { data.cast::<u32>().read_unaligned() }).unwrap_or(0);
    let offset = std::mem::offset_of!(AudioBufferList, mBuffers);
    let required = count.saturating_mul(std::mem::size_of::<AudioBuffer>());
    if offset.saturating_add(required) > size {
        return false;
    }
    // SAFETY: the bounds check above proves `offset` is at most `size`, so the
    // pointer stays within the written region (one past its end is allowed for
    // pointer arithmetic and the loop below only reads when buffers exist).
    let buffers = unsafe { data.add(offset) };
    for index in 0..count {
        // SAFETY: the bounds check above proves every reported buffer lies
        // inside the written region.
        let buffer = unsafe {
            buffers
                .add(index * std::mem::size_of::<AudioBuffer>())
                .cast::<AudioBuffer>()
                .read_unaligned()
        };
        if buffer.mNumberChannels > 0 {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    #[test]
    fn hdmi_transport_constant_is_the_hdmi_four_cc() {
        assert_eq!(kAudioDeviceTransportTypeHDMI, 0x6864_6d69);
    }

    #[test]
    #[allow(clippy::redundant_clone)] // The clone is the behavior under test.
    fn wakeup_channel_coalesces_without_blocking() {
        let (wakeup, receiver) = AudioWakeup::channel();
        wakeup.signal();
        wakeup.signal();
        wakeup.signal();
        assert_eq!(receiver.try_recv(), Ok(()));
        assert!(receiver.try_recv().is_err(), "a second signal is coalesced");
        wakeup.signal();
        assert_eq!(receiver.try_recv(), Ok(()));
        let clone = wakeup.clone();
        clone.signal();
        assert_eq!(receiver.try_recv(), Ok(()), "clones share the channel");
    }

    #[test]
    fn every_partial_registration_rolls_back_the_added_addresses() {
        let addresses = listener_addresses();
        for fail_at in 0..addresses.len() {
            let added = RefCell::new(Vec::new());
            let removed = RefCell::new(Vec::new());
            let result = register_listeners(
                &addresses,
                |address| {
                    if added.borrow().len() == fail_at {
                        return Err(AudioError::new("add audio property listener", -1));
                    }
                    added.borrow_mut().push(*address);
                    Ok(())
                },
                |address| removed.borrow_mut().push(*address),
            );
            assert!(result.is_err(), "registration must fail at index {fail_at}");
            assert_eq!(
                added.borrow().len(),
                fail_at,
                "only the addresses before the failure were added"
            );
            assert_eq!(
                *removed.borrow(),
                *added.borrow(),
                "every added address is removed exactly once"
            );
        }
    }

    #[test]
    fn a_full_registration_adds_each_address_once() {
        let addresses = listener_addresses();
        let added = RefCell::new(Vec::new());
        let removed = RefCell::new(Vec::new());
        register_listeners(
            &addresses,
            |address| {
                added.borrow_mut().push(*address);
                Ok(())
            },
            |address| removed.borrow_mut().push(*address),
        )
        .expect("full registration");
        assert_eq!(added.borrow().len(), addresses.len());
        assert!(removed.borrow().is_empty());
    }

    #[test]
    fn direction_spellings_are_stable() {
        assert_eq!(AudioDirection::Input.as_str(), "input");
        assert_eq!(AudioDirection::Output.as_str(), "output");
    }
}
