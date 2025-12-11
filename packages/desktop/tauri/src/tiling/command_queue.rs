//! Command queue for batched tiling operations.
//!
//! This module provides a lock-free command queue that decouples event reception
//! from processing. Events from the observer are queued and processed in batches,
//! reducing lock contention on the `TilingManager`.

use std::sync::OnceLock;

use parking_lot::Mutex;

// ============================================================================
// Command Types
// ============================================================================

/// Commands that can be queued for batch processing.
#[derive(Debug, Clone)]
#[allow(clippy::enum_variant_names)]
pub enum TilingCommand {
    /// A new window was created.
    WindowCreated { window_id: u64 },

    /// A window was destroyed.
    WindowDestroyed { window_id: u64 },

    /// A window was moved by the user.
    WindowMoved { window_id: u64 },

    /// A window was resized by the user.
    WindowResized {
        window_id: u64,
        width: u32,
        height: u32,
    },
}

// ============================================================================
// Command Queue
// ============================================================================

/// A thread-safe command queue for batching tiling operations.
#[derive(Default)]
pub struct CommandQueue {
    /// Pending commands to be processed.
    commands: Mutex<Vec<TilingCommand>>,
}

impl CommandQueue {
    /// Creates a new empty command queue.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            commands: Mutex::new(Vec::new()),
        }
    }

    /// Pushes a command onto the queue.
    pub fn push(&self, command: TilingCommand) { self.commands.lock().push(command); }

    /// Takes all pending commands from the queue for processing.
    ///
    /// Returns an empty vector if no commands are pending.
    #[must_use]
    pub fn take_all(&self) -> Vec<TilingCommand> { std::mem::take(&mut *self.commands.lock()) }
}

// ============================================================================
// Global Queue
// ============================================================================

/// Global command queue instance.
static COMMAND_QUEUE: OnceLock<CommandQueue> = OnceLock::new();

/// Gets the global command queue.
#[must_use]
pub fn get_queue() -> &'static CommandQueue { COMMAND_QUEUE.get_or_init(CommandQueue::new) }

/// Pushes a command to the global queue.
pub fn queue_command(command: TilingCommand) { get_queue().push(command); }

// ============================================================================
// Batch Processing
// ============================================================================

/// Processes all pending commands in the queue.
///
/// Commands are deduplicated and processed in an optimal order:
/// 1. Window destroyed (remove stale references first)
/// 2. Window created (add new windows)
/// 3. Move/resize operations
///
/// This function acquires the `TilingManager` lock once and processes all commands.
pub fn flush_commands() {
    let Some(manager) = super::try_get_manager() else {
        return;
    };

    let commands = get_queue().take_all();
    if commands.is_empty() {
        return;
    }

    // Deduplicate and categorize commands
    let processed = deduplicate_commands(commands);

    // Process with a single lock acquisition
    let mut guard = manager.write();

    // Process in optimal order: destroyed first to clean up state
    for window_id in processed.destroyed {
        guard.handle_window_destroyed(window_id);
    }

    // Then created windows
    for window_id in processed.created {
        guard.handle_new_window(window_id);
    }

    // Then resizes (latest size per window)
    for (window_id, width, height) in processed.resized {
        let _ = guard.handle_user_resize(window_id, width, height);
    }

    // Finally moves
    for window_id in processed.moved {
        let _ = guard.handle_window_moved(window_id);
    }
}

/// Deduplicated and categorized commands ready for processing.
struct ProcessedCommands {
    destroyed: Vec<u64>,
    created: Vec<u64>,
    resized: Vec<(u64, u32, u32)>,
    moved: Vec<u64>,
}

/// Deduplicates commands and extracts them into categorized vectors.
fn deduplicate_commands(commands: Vec<TilingCommand>) -> ProcessedCommands {
    use std::collections::HashSet;

    let mut destroyed = Vec::new();
    let mut destroyed_set = HashSet::new();

    let mut created = Vec::new();
    let mut created_set = HashSet::new();

    let mut resized = Vec::new();
    let mut resized_set = HashSet::new();

    let mut moved = Vec::new();
    let mut moved_set = HashSet::new();

    for command in commands {
        match command {
            TilingCommand::WindowDestroyed { window_id } => {
                if destroyed_set.insert(window_id) {
                    destroyed.push(window_id);
                    // If window was created and then destroyed, remove from created
                    created.retain(|&id| id != window_id);
                    created_set.remove(&window_id);
                }
            }
            TilingCommand::WindowCreated { window_id } => {
                // Don't add if already destroyed
                if !destroyed_set.contains(&window_id) && created_set.insert(window_id) {
                    created.push(window_id);
                }
            }
            TilingCommand::WindowResized { window_id, width, height } => {
                // Keep the latest resize for each window
                if resized_set.insert(window_id) {
                    resized.push((window_id, width, height));
                } else {
                    // Update existing resize
                    if let Some(entry) = resized.iter_mut().find(|(id, _, _)| *id == window_id) {
                        entry.1 = width;
                        entry.2 = height;
                    }
                }
            }
            TilingCommand::WindowMoved { window_id } => {
                if moved_set.insert(window_id) {
                    moved.push(window_id);
                }
            }
        }
    }

    ProcessedCommands {
        destroyed,
        created,
        resized,
        moved,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_command_queue_basic() {
        let queue = CommandQueue::new();

        queue.push(TilingCommand::WindowCreated { window_id: 1 });

        let commands = queue.take_all();
        assert_eq!(commands.len(), 1);

        // Queue should be empty after take_all
        let commands = queue.take_all();
        assert!(commands.is_empty());
    }

    #[test]
    fn test_deduplicate_created_destroyed() {
        let commands = vec![
            TilingCommand::WindowCreated { window_id: 1 },
            TilingCommand::WindowCreated { window_id: 2 },
            TilingCommand::WindowDestroyed { window_id: 1 },
        ];

        let processed = deduplicate_commands(commands);

        // Window 1 should be removed from created since it was destroyed
        assert!(!processed.created.contains(&1));
        assert!(processed.created.contains(&2));
        assert!(processed.destroyed.contains(&1));
    }

    #[test]
    fn test_deduplicate_moves() {
        let commands = vec![
            TilingCommand::WindowMoved { window_id: 1 },
            TilingCommand::WindowMoved { window_id: 2 },
            TilingCommand::WindowMoved { window_id: 1 }, // Duplicate
        ];

        let processed = deduplicate_commands(commands);

        // Should have both windows (deduplicated)
        assert_eq!(processed.moved.len(), 2);
    }

    #[test]
    fn test_deduplicate_resize_keeps_latest() {
        let commands = vec![
            TilingCommand::WindowResized {
                window_id: 1,
                width: 100,
                height: 100,
            },
            TilingCommand::WindowResized {
                window_id: 1,
                width: 200,
                height: 200,
            },
        ];

        let processed = deduplicate_commands(commands);

        assert_eq!(processed.resized.len(), 1);
        assert_eq!(processed.resized[0], (1, 200, 200));
    }
}
