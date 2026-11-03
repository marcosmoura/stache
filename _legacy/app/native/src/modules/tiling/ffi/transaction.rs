//! Safe wrappers for SkyLight transaction APIs.
//!
#![allow(clippy::doc_markdown)] // Allow SkyLight, CGWindowID, etc. without backticks
//!
//! This module provides RAII wrappers for batching multiple window server operations
//! into atomic transactions. This reduces round-trips to the window server and ensures
//! all operations are applied together.
//!
//! # Overview
//!
//! SkyLight transactions allow batching operations like:
//! - Window z-order changes (ordering windows above/below others)
//! - Window alpha/opacity changes
//! - Window tags/flags changes
//!
//! Note: Position and size changes are NOT supported by transactions and must
//! continue using the Accessibility API.

use std::ffi::c_void;

use super::skylight::get_connection_id;

// ============================================================================
// FFI Declarations
// ============================================================================

type CFTypeRef = *mut c_void;

/// Place window above the reference window.
pub const K_WINDOW_ORDER_ABOVE: i32 = 1;
/// Place window below the reference window.
pub const K_WINDOW_ORDER_BELOW: i32 = -1;
/// Place window at the front of all windows at its level.
pub const K_WINDOW_ORDER_OUT: i32 = 0;

#[link(name = "SkyLight", kind = "framework")]
unsafe extern "C" {
    fn SLSTransactionCreate(cid: u32, options: u32) -> CFTypeRef;
    fn SLSTransactionCommit(transaction: CFTypeRef, synchronous: i32) -> i32;
    fn SLSTransactionSetWindowAlpha(transaction: CFTypeRef, wid: u32, alpha: f32) -> i32;
    fn SLSTransactionOrderWindow(
        transaction: CFTypeRef,
        wid: u32,
        order: i32,
        relative_to_wid: u32,
    ) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: *const c_void);
}

// ============================================================================
// Transaction Error
// ============================================================================

/// Error type for transaction operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionError {
    /// Failed to get connection ID.
    NoConnection,
    /// Failed to create transaction.
    CreateFailed,
    /// Failed to commit transaction.
    CommitFailed(i32),
    /// Failed to set window alpha.
    SetAlphaFailed(i32),
    /// Failed to order window.
    OrderWindowFailed(i32),
}

impl std::fmt::Display for TransactionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoConnection => write!(f, "failed to get window server connection"),
            Self::CreateFailed => write!(f, "failed to create transaction"),
            Self::CommitFailed(code) => write!(f, "failed to commit transaction (error {code})"),
            Self::SetAlphaFailed(code) => write!(f, "failed to set window alpha (error {code})"),
            Self::OrderWindowFailed(code) => write!(f, "failed to order window (error {code})"),
        }
    }
}

impl std::error::Error for TransactionError {}

/// Result type for transaction operations.
pub type TransactionResult<T> = Result<T, TransactionError>;

// ============================================================================
// Transaction Wrapper
// ============================================================================

/// RAII wrapper for SkyLight transactions.
///
/// Batches multiple window server operations into a single atomic commit.
#[derive(Debug)]
pub struct Transaction {
    /// Connection ID to the window server.
    #[allow(dead_code)]
    cid: u32,
    /// Transaction reference.
    handle: CFTypeRef,
    /// Whether the transaction has been committed.
    committed: bool,
}

impl Transaction {
    /// Creates a new transaction.
    ///
    /// # Errors
    ///
    /// Returns an error if the window server connection is unavailable
    /// or if transaction creation fails.
    pub fn new() -> TransactionResult<Self> {
        let cid = get_connection_id();
        if cid == 0 {
            return Err(TransactionError::NoConnection);
        }

        let handle = unsafe { SLSTransactionCreate(cid, 0) };
        if handle.is_null() {
            return Err(TransactionError::CreateFailed);
        }

        Ok(Self { cid, handle, committed: false })
    }

    /// Commits the transaction, applying all queued operations atomically.
    ///
    /// # Errors
    ///
    /// Returns an error if the window server fails to commit the transaction.
    pub fn commit(&mut self, synchronous: bool) -> TransactionResult<()> {
        if self.committed {
            return Ok(());
        }

        let sync_flag = i32::from(synchronous);
        let result = unsafe { SLSTransactionCommit(self.handle, sync_flag) };

        self.committed = true;

        if result == 0 {
            Ok(())
        } else {
            Err(TransactionError::CommitFailed(result))
        }
    }

    /// Commits the transaction synchronously.
    ///
    /// # Errors
    ///
    /// Returns an error if the window server fails to commit the transaction.
    pub fn commit_sync(&mut self) -> TransactionResult<()> { self.commit(true) }

    /// Commits the transaction asynchronously.
    ///
    /// # Errors
    ///
    /// Returns an error if the window server fails to commit the transaction.
    pub fn commit_async(&mut self) -> TransactionResult<()> { self.commit(false) }

    /// Sets the alpha (opacity) of a window.
    ///
    /// # Errors
    ///
    /// Returns an error if the window server fails to set the alpha.
    #[allow(clippy::cast_possible_truncation)]
    pub fn set_window_alpha(&mut self, window_id: u32, alpha: f64) -> TransactionResult<()> {
        let alpha_f32 = alpha.clamp(0.0, 1.0) as f32;
        let result = unsafe { SLSTransactionSetWindowAlpha(self.handle, window_id, alpha_f32) };

        if result == 0 {
            Ok(())
        } else {
            Err(TransactionError::SetAlphaFailed(result))
        }
    }

    /// Orders a window above another window.
    ///
    /// # Errors
    ///
    /// Returns an error if the window server fails to reorder the window.
    pub fn order_window_above(
        &mut self,
        window_id: u32,
        relative_to: u32,
    ) -> TransactionResult<()> {
        let result = unsafe {
            SLSTransactionOrderWindow(self.handle, window_id, K_WINDOW_ORDER_ABOVE, relative_to)
        };

        if result == 0 {
            Ok(())
        } else {
            Err(TransactionError::OrderWindowFailed(result))
        }
    }

    /// Orders a window below another window.
    ///
    /// # Errors
    ///
    /// Returns an error if the window server fails to reorder the window.
    pub fn order_window_below(
        &mut self,
        window_id: u32,
        relative_to: u32,
    ) -> TransactionResult<()> {
        let result = unsafe {
            SLSTransactionOrderWindow(self.handle, window_id, K_WINDOW_ORDER_BELOW, relative_to)
        };

        if result == 0 {
            Ok(())
        } else {
            Err(TransactionError::OrderWindowFailed(result))
        }
    }

    /// Brings a window to the front of all windows at its level.
    ///
    /// # Errors
    ///
    /// Returns an error if the window server fails to reorder the window.
    pub fn bring_to_front(&mut self, window_id: u32) -> TransactionResult<()> {
        let result =
            unsafe { SLSTransactionOrderWindow(self.handle, window_id, K_WINDOW_ORDER_ABOVE, 0) };

        if result == 0 {
            Ok(())
        } else {
            Err(TransactionError::OrderWindowFailed(result))
        }
    }

    /// Returns whether this transaction has been committed.
    #[must_use]
    pub const fn is_committed(&self) -> bool { self.committed }
}

impl Drop for Transaction {
    fn drop(&mut self) {
        // Auto-commit if not already committed
        if !self.committed {
            let _ = unsafe { SLSTransactionCommit(self.handle, 1) };
        }

        // Release the transaction handle
        if !self.handle.is_null() {
            unsafe { CFRelease(self.handle.cast_const()) };
        }
    }
}

// Transaction is Send because SkyLight APIs are thread-safe
unsafe impl Send for Transaction {}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transaction_error_display() {
        assert_eq!(
            TransactionError::NoConnection.to_string(),
            "failed to get window server connection"
        );
        assert_eq!(
            TransactionError::CreateFailed.to_string(),
            "failed to create transaction"
        );
        assert_eq!(
            TransactionError::CommitFailed(42).to_string(),
            "failed to commit transaction (error 42)"
        );
    }

    #[test]
    fn window_order_constants() {
        assert_eq!(K_WINDOW_ORDER_ABOVE, 1);
        assert_eq!(K_WINDOW_ORDER_BELOW, -1);
        assert_eq!(K_WINDOW_ORDER_OUT, 0);
    }

    #[test]
    fn transaction_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<Transaction>();
    }
}
