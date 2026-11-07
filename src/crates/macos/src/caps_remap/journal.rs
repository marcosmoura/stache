//! The versioned write-ahead journal format.
//!
//! The journal proves only that **this application intended** to own the Caps
//! Lock to F18 mapping. It never proves an external writer did not write the
//! same mapping independently. It is written before the mapping changes and
//! deleted only after absence of the owned mapping is verified.
//!
//! Phases:
//!
//! * `apply_pending` written before `hidutil` sets the mapping; the mapping may
//!   or may not exist.
//! * `applied` the mapping was set and verified.
//! * `restore_pending` written before `hidutil` removes the mapping.
//! * `ownership_lost` Caps Lock was found mapped elsewhere; nothing of ours was
//!   touched, and the record stays until the foreign mapping is gone.

use serde::{Deserialize, Serialize};
use stache_sdk::IDENTITY;

use super::mapping::OWNED;
use super::storage::JOURNAL_MAX_BYTES;

/// The journal format version.
pub const VERSION: u32 = 1;

/// Where an apply or restore stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Intent to apply is durable; the mapping may or may not exist.
    ApplyPending,
    /// The mapping was applied and verified.
    Applied,
    /// Intent to restore is durable.
    RestorePending,
    /// Caps Lock now maps somewhere this application did not put it.
    OwnershipLost,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Transition {
    src: u64,
    dst: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    version: u32,
    identity: String,
    revision: u64,
    phase: Phase,
    transition: Transition,
    /// Acquisition refuses any existing Caps mapping, so the prior relevant
    /// state is always the absence of one.
    prior_caps: String,
}

/// A decoded journal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Journal {
    /// Increases with every durable update; diagnostics only.
    pub revision: u64,
    /// The pending phase.
    pub phase: Phase,
}

/// Why journal bytes were refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalError(pub String);

impl std::fmt::Display for JournalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the Caps journal is not trusted: {}", self.0)
    }
}

impl std::error::Error for JournalError {}

impl Journal {
    /// A first journal for a phase.
    #[must_use]
    pub const fn new(phase: Phase) -> Self { Self { revision: 1, phase } }

    /// The next durable state in `phase`.
    #[must_use]
    pub const fn next(self, phase: Phase) -> Self {
        Self {
            revision: self.revision.saturating_add(1),
            phase,
        }
    }

    /// Serializes to the journal bytes.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let wire = Wire {
            version: VERSION,
            identity: IDENTITY.to_owned(),
            revision: self.revision,
            phase: self.phase,
            transition: Transition { src: OWNED.src, dst: OWNED.dst },
            prior_caps: "absent".to_owned(),
        };
        serde_json::to_vec(&wire).unwrap_or_else(|_| unreachable!("plain data serializes"))
    }

    /// Strictly decodes journal bytes.
    ///
    /// # Errors
    /// Returns a [`JournalError`] for malformed JSON, unknown fields, another
    /// version, another identity or another transition. A journal this
    /// application did not write is never adopted.
    pub fn decode(bytes: &[u8]) -> Result<Self, JournalError> {
        if bytes.len() > JOURNAL_MAX_BYTES {
            return Err(JournalError("it is too large".to_owned()));
        }
        let wire: Wire = serde_json::from_slice(bytes)
            .map_err(|error| JournalError(format!("it does not parse: {error}")))?;
        if wire.version != VERSION {
            return Err(JournalError(format!("unsupported version {}", wire.version)));
        }
        if wire.identity != IDENTITY {
            return Err(JournalError("it belongs to another identity".to_owned()));
        }
        if wire.transition.src != OWNED.src || wire.transition.dst != OWNED.dst {
            return Err(JournalError("it records another transition".to_owned()));
        }
        if wire.prior_caps != "absent" {
            return Err(JournalError("it records a prior Caps mapping".to_owned()));
        }
        Ok(Self {
            revision: wire.revision,
            phase: wire.phase,
        })
    }
}
