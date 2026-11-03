//! The read-only `proxyAudio.list` socket method.
//!
//! It serves the same listing service as `stache audio list` and never starts a
//! listener or changes a device. The method is available even when the module
//! is disabled or paused, because enumeration is read-only.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use specta::Type;
use stache_macos::audio::AudioBackend;
use stache_sdk::ipc::{HandlerError, HandlerResult, IpcHandler, decode_params, encode_response};

use crate::list::{DeviceFilter, list_devices};

/// The device direction requested by `proxyAudio.list`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum AudioListKind {
    /// Every device.
    #[default]
    All,
    /// Devices that support input.
    Input,
    /// Devices that support output.
    Output,
}

impl From<AudioListKind> for DeviceFilter {
    fn from(kind: AudioListKind) -> Self {
        match kind {
            AudioListKind::All => Self::All,
            AudioListKind::Input => Self::InputOnly,
            AudioListKind::Output => Self::OutputOnly,
        }
    }
}

/// Parameters for `proxyAudio.list`.
///
/// Omitting the parameters, or sending `null`, means [`AudioListKind::All`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(default, rename_all = "camelCase")]
pub struct AudioListParams {
    /// The devices to return.
    pub kind: AudioListKind,
}

/// The read-only audio socket handler.
pub struct AudioIpc {
    backend: Arc<dyn AudioBackend>,
}

impl AudioIpc {
    /// Serves listings from one backend.
    #[must_use]
    pub fn new(backend: Arc<dyn AudioBackend>) -> Self { Self { backend } }
}

impl IpcHandler for AudioIpc {
    fn handle(&self, method: &str, params: Value) -> HandlerResult<Value> {
        match method {
            "list" => {
                let params: Option<AudioListParams> = decode_params(params)?;
                let filter = params.unwrap_or_default().kind.into();
                encode_response(&list_devices(self.backend.as_ref(), filter))
            }
            _ => Err(HandlerError::unknown_method(method)),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use stache_macos::audio::{
        AudioBackend, AudioChangeSubscription, AudioDevice, AudioDirection, AudioError, AudioWakeup,
    };

    use super::*;

    struct Devices(Vec<AudioDevice>);

    impl AudioBackend for Devices {
        fn devices(&self) -> Vec<AudioDevice> { self.0.clone() }

        fn default_device(&self, _: AudioDirection) -> Option<AudioDevice> { None }

        fn set_default_device(&self, _: AudioDirection, _: u32) -> Result<(), AudioError> {
            Err(AudioError::new("set default audio device", -1))
        }

        fn is_hdmi(&self, _: u32) -> bool { false }

        fn subscribe(
            &self,
            _: AudioWakeup,
        ) -> Result<Box<dyn AudioChangeSubscription>, AudioError> {
            Err(AudioError::new("add audio property listener", -1))
        }
    }

    fn handler() -> AudioIpc {
        AudioIpc::new(Arc::new(Devices(vec![
            AudioDevice {
                id: 1,
                name: "Microphone".to_owned(),
                input: true,
                output: false,
            },
            AudioDevice {
                id: 2,
                name: "Speakers".to_owned(),
                input: false,
                output: true,
            },
        ])))
    }

    #[test]
    fn omitted_and_null_params_list_every_device() {
        let handler = handler();
        let omitted = handler.handle("list", Value::Null).expect("list");
        let empty = handler.handle("list", json!({})).expect("list");
        assert_eq!(omitted, empty);
        assert_eq!(omitted.as_array().map(Vec::len), Some(2));
    }

    #[test]
    fn the_kind_selects_one_direction() {
        let handler = handler();
        let input = handler.handle("list", json!({ "kind": "input" })).expect("list");
        let values = input.as_array().expect("array");
        assert_eq!(values.len(), 1);
        assert_eq!(values[0]["name"], "Microphone");
        let output = handler.handle("list", json!({ "kind": "output" })).expect("list");
        let values = output.as_array().expect("array");
        assert_eq!(values.len(), 1);
        assert_eq!(values[0]["name"], "Speakers");
    }

    #[test]
    fn unknown_methods_and_bad_params_are_typed() {
        let handler = handler();
        let unknown = handler.handle("set", Value::Null).expect_err("unknown method");
        assert_eq!(unknown.kind(), stache_sdk::ipc::HandlerErrorKind::UnknownMethod);
        let invalid =
            handler.handle("list", json!({ "kind": "sideways" })).expect_err("invalid kind");
        assert_eq!(invalid.kind(), stache_sdk::ipc::HandlerErrorKind::InvalidParams);
    }
}
