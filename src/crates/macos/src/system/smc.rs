//! The native SMC CPU temperature, `smctemp` style.
//!
//! No subprocess and no third-party SMC crate: [`smc_io`](super::smc_io) opens
//! `AppleSMC` over public `IOKit` and reads keys with the standard
//! `SMCKeyData` struct protocol (`IOConnectCallStructMethod`, selector 2, read
//! key info then read bytes) used by smcFanControl, `smctemp` and Stats. This
//! module is the safe, pure part: key lists per chip, value decoding, the
//! averaging and the retry / last-valid policy, all over the fakeable
//! [`SmcOpener`] / [`SmcConnection`] traits.
//!
//! # Key lists
//!
//! Sources (read on 2026-11-10):
//!
//! * `narugit/smctemp`, `smctemp.cc` `SmcTemp::GetCpuTemp()` (Intel
//!   `TC0D/TC0E/TC0F/TC0P`; Apple Silicon per chip family, M1 auxiliary
//!   `Tc0a/Tc0b/Tc0x/Tc0z`) and `smctemp.h` for the protocol constants;
//! * `exelban/stats`, `Modules/Sensors/values.swift` (CPU temperature sensors
//!   per platform generation, including the M3/M4 `Te*`/`Tf*` efficiency and
//!   performance keys and the M5 `Tp*` super/performance keys);
//! * legacy `cpu.rs` key families `TC`, `Tp`, `Te`, `Tf`.
//!
//! The two upstream lists disagree for M2 to M4 (they differ in which
//! per-generation key names exist), so the list of a generation is the **union**
//! of both, deduplicated. A key the machine does not have is skipped: the SMC
//! answers a key-info read for it with a non-zero result. Averaging only the
//! keys that exist and read valid is the same behaviour `smctemp` has.
//!
//! The chip is detected from `machdep.cpu.brand_string`; an Apple Silicon
//! generation this build does not know uses the union of every Apple Silicon
//! list. Nothing here has been verified on hardware yet.

use std::sync::{Mutex, PoisonError};

use super::SystemReadError;

/// The most bytes an SMC value carries.
pub const SMC_MAX_BYTES: usize = 32;

/// A valid CPU temperature is `> TEMP_MIN` and `< TEMP_MAX` degrees Celsius.
pub const TEMP_MIN: f64 = 0.0;
/// See [`TEMP_MIN`].
pub const TEMP_MAX: f64 = 150.0;

/// Extra whole-group reads after the first when no key reads valid (the
/// `smctemp` M2 instability handling).
pub const DEFAULT_RETRIES: u32 = 3;

/// How many consecutive samples without a valid temperature still report the
/// last valid one.
pub const DEFAULT_STALE_SAMPLES: u32 = 3;

/// Whether `temp` is a plausible CPU temperature.
#[must_use]
pub fn is_valid_temp(temp: f64) -> bool { temp.is_finite() && temp > TEMP_MIN && temp < TEMP_MAX }

/// A four-character SMC key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SmcKey([u8; 4]);

impl SmcKey {
    /// A key from exactly four ASCII bytes.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let bytes: [u8; 4] = text.as_bytes().try_into().ok()?;
        bytes.iter().all(u8::is_ascii_graphic).then_some(Self(bytes))
    }

    /// The key as the big-endian `u32` the SMC protocol uses.
    #[must_use]
    pub const fn code(self) -> u32 { u32::from_be_bytes(self.0) }

    /// The key text.
    #[must_use]
    pub fn as_str(&self) -> &str { std::str::from_utf8(&self.0).unwrap_or("????") }
}

/// One raw SMC value: its four-character type and its bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SmcRaw {
    /// The type code, for example `*b"flt "`.
    pub data_type: [u8; 4],
    /// The value bytes, at most [`SMC_MAX_BYTES`].
    pub bytes: Vec<u8>,
}

impl SmcRaw {
    /// A value of `data_type` with `bytes`.
    #[must_use]
    pub fn new(data_type: &[u8; 4], bytes: &[u8]) -> Self {
        Self {
            data_type: *data_type,
            bytes: bytes.to_vec(),
        }
    }
}

/// The divisor of the 16-bit fixed-point types `fpXY` / `spXY` (`smctemp`'s
/// `ReadValue`): `fp1f` 32768, `fp4c` 4096, `fp5b` 2048, `fp6a` 1024, `fp79`
/// 512, `fp88` 256, `fpa6` 64, `fpc4` 16, `fpe2` 4; `sp1e` 16384, `sp3c` 4096,
/// `sp4b` 2048, `sp5a` 1024, `sp69` 512, `sp78` 256, `sp87` 128, `sp96` 64,
/// `spb4` 16, `spf0` 1.
const FIXED_POINT: [(&[u8; 4], f64, bool); 18] = [
    (b"fp1f", 32768.0, false),
    (b"fp4c", 4096.0, false),
    (b"fp5b", 2048.0, false),
    (b"fp6a", 1024.0, false),
    (b"fp79", 512.0, false),
    (b"fp88", 256.0, false),
    (b"fpa6", 64.0, false),
    (b"fpc4", 16.0, false),
    (b"fpe2", 4.0, false),
    (b"sp1e", 16384.0, true),
    (b"sp3c", 4096.0, true),
    (b"sp4b", 2048.0, true),
    (b"sp5a", 1024.0, true),
    (b"sp69", 512.0, true),
    (b"sp78", 256.0, true),
    (b"sp87", 128.0, true),
    (b"sp96", 64.0, true),
    (b"spb4", 16.0, true),
];

/// Decodes a raw value into a number.
///
/// `None` for an unknown type, a wrong size or a non-finite result. The
/// multi-byte integers and fixed-point values
/// are big-endian; `flt ` is a little-endian IEEE 754 float (the native order
/// of every supported Mac).
#[must_use]
#[allow(clippy::cast_precision_loss)] // SMC values are tiny
pub fn decode(raw: &SmcRaw) -> Option<f64> {
    let bytes = raw.bytes.as_slice();
    let value = match &raw.data_type {
        b"flt " => f64::from(f32::from_le_bytes(bytes.try_into().ok()?)),
        b"ui8 " if bytes.len() == 1 => f64::from(bytes[0]),
        b"ui16" if bytes.len() == 2 => f64::from(u16::from_be_bytes(bytes.try_into().ok()?)),
        b"ui32" if bytes.len() == 4 => f64::from(u32::from_be_bytes(bytes.try_into().ok()?)),
        b"ui64" if bytes.len() == 8 => u64::from_be_bytes(bytes.try_into().ok()?) as f64,
        b"si8 " if bytes.len() == 1 => f64::from(i8::from_be_bytes([bytes[0]])),
        b"si16" if bytes.len() == 2 => f64::from(i16::from_be_bytes(bytes.try_into().ok()?)),
        other => {
            let (_, divisor, signed) = FIXED_POINT.iter().find(|(name, _, _)| *name == other)?;
            let word: [u8; 2] = bytes.try_into().ok()?;
            if *signed {
                f64::from(i16::from_be_bytes(word)) / divisor
            } else {
                f64::from(u16::from_be_bytes(word)) / divisor
            }
        }
    };
    value.is_finite().then_some(value)
}

/// The CPU family whose keys are read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChipFamily {
    /// An Intel Mac.
    Intel,
    /// Apple M1 (any variant).
    M1,
    /// Apple M2.
    M2,
    /// Apple M3.
    M3,
    /// Apple M4.
    M4,
    /// Apple M5.
    M5,
    /// An Apple Silicon generation this build does not know.
    OtherApple,
}

impl ChipFamily {
    /// The family of a `machdep.cpu.brand_string`, for example
    /// `Apple M3 Pro` or `Intel(R) Core(TM) i7`.
    #[must_use]
    pub fn from_brand(brand: &str) -> Self {
        let lower = brand.to_ascii_lowercase();
        let Some(rest) = lower.strip_prefix("apple") else {
            return Self::Intel;
        };
        let generation = rest.split_whitespace().next().unwrap_or("");
        match generation {
            "m1" => Self::M1,
            "m2" => Self::M2,
            "m3" => Self::M3,
            "m4" => Self::M4,
            "m5" => Self::M5,
            _ => Self::OtherApple,
        }
    }

    /// The keys averaged first, and the keys averaged only when none of the
    /// first reads valid.
    #[must_use]
    pub fn plan(self) -> KeyPlan {
        match self {
            Self::Intel => KeyPlan::new(INTEL_CORES.to_vec(), INTEL_PACKAGE.to_vec()),
            Self::M1 => KeyPlan::new(M1.to_vec(), M1_AUX.to_vec()),
            Self::M2 => KeyPlan::new(M2.to_vec(), Vec::new()),
            Self::M3 => KeyPlan::new(M3.to_vec(), Vec::new()),
            Self::M4 => KeyPlan::new(M4.to_vec(), Vec::new()),
            Self::M5 => KeyPlan::new(M5.to_vec(), Vec::new()),
            Self::OtherApple => {
                let mut all: Vec<&str> = Vec::new();
                let lists: [&[&str]; 5] = [&M1, &M2, &M3, &M4, &M5];
                for list in lists {
                    for key in list {
                        if !all.contains(key) {
                            all.push(key);
                        }
                    }
                }
                KeyPlan::new(all, M1_AUX.to_vec())
            }
        }
    }
}

/// Intel per-core die keys (`TC0C`, `TC1C`, ...; the keys the legacy `smc`
/// crate averaged).
const INTEL_CORES: [&str; 10] = [
    "TC0C", "TC1C", "TC2C", "TC3C", "TC4C", "TC5C", "TC6C", "TC7C", "TC8C", "TC9C",
];
/// Intel package keys (`smctemp`: die, PECI die, filtered, proximity; Stats:
/// `TCAD` package), averaged only when no core key reads valid.
const INTEL_PACKAGE: [&str; 5] = ["TC0D", "TC0E", "TC0F", "TC0P", "TCAD"];
/// `smctemp` M1 plus Stats M1 (identical): 2 efficiency, 8 performance.
const M1: [&str; 10] = [
    "Tp01", "Tp05", "Tp0D", "Tp0H", "Tp0L", "Tp0P", "Tp0X", "Tp0b", "Tp09", "Tp0T",
];
/// `smctemp` M1 auxiliary sensors.
const M1_AUX: [&str; 4] = ["Tc0a", "Tc0b", "Tc0x", "Tc0z"];
/// `smctemp` M2 (4 efficiency + 8 cores) united with Stats M2.
const M2: [&str; 14] = [
    "Tp1h", "Tp1t", "Tp1p", "Tp1l", "Tp01", "Tp09", "Tp0f", "Tp0n", "Tp05", "Tp0D", "Tp0j", "Tp0r",
    "Tp0X", "Tp0b",
];
/// `smctemp` M3 (same `Tp*` list as its M2) united with Stats M3 (`Te*`
/// efficiency, `Tf*` performance).
const M3: [&str; 24] = [
    "Tp01", "Tp09", "Tp0f", "Tp0n", "Tp05", "Tp0D", "Tp0j", "Tp0r", "Te05", "Te0L", "Te0P", "Te0S",
    "Tf04", "Tf09", "Tf0A", "Tf0B", "Tf0D", "Tf0E", "Tf44", "Tf49", "Tf4A", "Tf4B", "Tf4D", "Tf4E",
];
/// `smctemp` M4 united with Stats M4.
const M4: [&str; 13] = [
    "Tp01", "Tp09", "Tp0f", "Tp05", "Tp0D", "Te05", "Te0S", "Te09", "Te0H", "Tp0V", "Tp0Y", "Tp0b",
    "Tp0e",
];
/// `smctemp` M5 (identical to Stats M5): 6 super + 12 performance cores.
const M5: [&str; 18] = [
    "Tp00", "Tp04", "Tp08", "Tp0C", "Tp0G", "Tp0K", "Tp0O", "Tp0R", "Tp0U", "Tp0X", "Tp0a", "Tp0d",
    "Tp0g", "Tp0j", "Tp0m", "Tp0p", "Tp0u", "Tp0y",
];

/// The keys one chip's temperature is averaged from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyPlan {
    /// Averaged first.
    pub primary: Vec<SmcKey>,
    /// Averaged only when no primary key reads valid.
    pub fallback: Vec<SmcKey>,
}

impl KeyPlan {
    fn new(primary: Vec<&str>, fallback: Vec<&str>) -> Self {
        let keys = |list: Vec<&str>| list.into_iter().filter_map(SmcKey::parse).collect();
        Self {
            primary: keys(primary),
            fallback: keys(fallback),
        }
    }
}

/// An open SMC connection. Closed when dropped.
pub trait SmcConnection: Send {
    /// Reads `key`: `Ok(None)` when the machine has no such key.
    ///
    /// # Errors
    /// Why the call into the SMC failed (the connection is then discarded).
    fn read(&mut self, key: SmcKey) -> Result<Option<SmcRaw>, SystemReadError>;
}

/// Opens SMC connections.
pub trait SmcOpener: Send + Sync {
    /// Opens a connection.
    ///
    /// # Errors
    /// Why `AppleSMC` could not be found or opened.
    fn open(&self) -> Result<Box<dyn SmcConnection>, SystemReadError>;
}

/// Reads the CPU temperature.
pub trait SmcBackend: Send + Sync {
    /// One CPU temperature sample in degrees Celsius: `Ok(None)` when there is
    /// no valid reading (and none recent enough to repeat).
    ///
    /// # Errors
    /// Why the SMC could not be opened or read and no recent valid reading can
    /// stand in.
    fn cpu_temperature(&self) -> Result<Option<f64>, SystemReadError>;
}

/// The retry and last-valid bounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SmcPolicy {
    /// Extra whole-group reads when none of the keys reads valid.
    pub retries: u32,
    /// How many consecutive samples without a valid reading repeat the last
    /// valid one.
    pub stale_samples: u32,
}

impl Default for SmcPolicy {
    fn default() -> Self {
        Self {
            retries: DEFAULT_RETRIES,
            stale_samples: DEFAULT_STALE_SAMPLES,
        }
    }
}

#[derive(Default)]
struct State {
    connection: Option<Box<dyn SmcConnection>>,
    /// The last valid temperature and the failed samples since.
    last: Option<(f64, u32)>,
}

/// The native CPU temperature source: the key plan, the decoding, the average
/// and the retry / last-valid policy over an [`SmcOpener`].
pub struct SmcCpuTemperature {
    opener: Box<dyn SmcOpener>,
    plan: KeyPlan,
    policy: SmcPolicy,
    state: Mutex<State>,
}

impl SmcCpuTemperature {
    /// A source over `opener` for `chip`.
    #[must_use]
    pub fn new(opener: Box<dyn SmcOpener>, chip: ChipFamily, policy: SmcPolicy) -> Self {
        Self {
            opener,
            plan: chip.plan(),
            policy,
            state: Mutex::new(State::default()),
        }
    }

    /// The production source: public `IOKit`, the detected chip, the default
    /// bounds. Opens nothing until the first sample.
    #[must_use]
    pub fn system() -> Self {
        Self::new(
            Box::new(super::smc_io::IoKitSmc),
            ChipFamily::from_brand(&super::smc_io::cpu_brand()),
            SmcPolicy::default(),
        )
    }

    /// The averaged valid temperature of `keys`.
    fn average(
        connection: &mut dyn SmcConnection,
        keys: &[SmcKey],
    ) -> Result<Option<f64>, SystemReadError> {
        let mut sum = 0.0;
        let mut count = 0_u32;
        for key in keys {
            let Some(raw) = connection.read(*key)? else {
                continue;
            };
            if let Some(temp) = decode(&raw).filter(|temp| is_valid_temp(*temp)) {
                sum += temp;
                count += 1;
            }
        }
        Ok((count > 0).then(|| sum / f64::from(count)))
    }

    fn read_group(
        &self,
        connection: &mut dyn SmcConnection,
    ) -> Result<Option<f64>, SystemReadError> {
        if let Some(temp) = Self::average(connection, &self.plan.primary)? {
            return Ok(Some(temp));
        }
        Self::average(connection, &self.plan.fallback)
    }

    fn sample(&self, state: &mut State) -> Result<Option<f64>, SystemReadError> {
        if state.connection.is_none() {
            state.connection = Some(self.opener.open()?);
        }
        let Some(connection) = state.connection.as_mut() else {
            return Ok(None);
        };
        for _ in 0..=self.policy.retries {
            match self.read_group(connection.as_mut()) {
                Ok(Some(temp)) => return Ok(Some(temp)),
                Ok(None) => {}
                Err(error) => {
                    state.connection = None;
                    return Err(error);
                }
            }
        }
        Ok(None)
    }
}

impl SmcBackend for SmcCpuTemperature {
    fn cpu_temperature(&self) -> Result<Option<f64>, SystemReadError> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let outcome = self.sample(&mut state);
        if let Ok(Some(temp)) = outcome {
            state.last = Some((temp, 0));
            return Ok(Some(temp));
        }
        // No valid reading: repeat the last valid one for a bounded number of
        // samples, then report unknown. Never fabricated.
        if let Some((temp, age)) = state.last {
            if age < self.policy.stale_samples {
                state.last = Some((temp, age + 1));
                return Ok(Some(temp));
            }
            state.last = None;
        }
        outcome
    }
}

#[cfg(test)]
mod tests;
