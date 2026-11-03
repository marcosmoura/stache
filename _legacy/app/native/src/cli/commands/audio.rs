//! Audio CLI commands.
//!
//! This module contains the audio subcommands for managing audio devices.

use clap::Subcommand;

use crate::audio;
use crate::error::StacheError;

/// Audio subcommands for listing and inspecting audio devices.
#[derive(Subcommand, Debug)]
#[command(next_display_order = None)]
pub enum AudioCommands {
    /// List all audio devices on the system.
    ///
    /// Shows audio input and output devices with their names and types.
    /// By default, displays a human-readable table format.
    #[command(after_long_help = r#"Examples:
  stache audio list              # List all devices in table format
  stache audio list --json       # List all devices in JSON format
  stache audio list --input      # List only input devices
  stache audio list --output     # List only output devices
  stache audio list -io --json   # List all devices in JSON (explicit)"#)]
    List {
        /// Output in JSON format instead of table format.
        #[arg(long, short = 'j')]
        json: bool,

        /// Show only input devices.
        #[arg(long, short = 'i')]
        input: bool,

        /// Show only output devices.
        #[arg(long, short = 'o')]
        output: bool,
    },
}

/// Execute audio subcommands.
pub fn execute(cmd: &AudioCommands) -> Result<(), StacheError> {
    match cmd {
        AudioCommands::List { json, input, output } => {
            let filter = match (input, output) {
                (true, false) => audio::DeviceFilter::InputOnly,
                (false, true) => audio::DeviceFilter::OutputOnly,
                _ => audio::DeviceFilter::All,
            };

            let devices = audio::list_devices(filter);

            if *json {
                let json_output = serde_json::to_string_pretty(&devices).map_err(|e| {
                    StacheError::AudioError(format!("JSON serialization error: {e}"))
                })?;
                println!("{json_output}");
            } else {
                let table = audio::format_devices_table(&devices);
                println!("{table}");
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[derive(Parser)]
    struct TestCli {
        #[command(subcommand)]
        command: AudioCommands,
    }

    #[test]
    fn test_audio_list_parse() {
        let cli = TestCli::try_parse_from(["test", "list"]).unwrap();
        match cli.command {
            AudioCommands::List { json, input, output } => {
                assert!(!json);
                assert!(!input);
                assert!(!output);
            }
        }
    }

    #[test]
    fn test_audio_list_json_parse() {
        let cli = TestCli::try_parse_from(["test", "list", "--json"]).unwrap();
        match cli.command {
            AudioCommands::List { json, .. } => {
                assert!(json);
            }
        }
    }

    #[test]
    fn test_audio_list_input_parse() {
        let cli = TestCli::try_parse_from(["test", "list", "--input"]).unwrap();
        match cli.command {
            AudioCommands::List { input, output, .. } => {
                assert!(input);
                assert!(!output);
            }
        }
    }

    #[test]
    fn test_audio_list_output_parse() {
        let cli = TestCli::try_parse_from(["test", "list", "--output"]).unwrap();
        match cli.command {
            AudioCommands::List { input, output, .. } => {
                assert!(!input);
                assert!(output);
            }
        }
    }
}
