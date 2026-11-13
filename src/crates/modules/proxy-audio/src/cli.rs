//! The `stache audio list` command.
//!
//! The command runs entirely in the CLI process: it enumerates through an
//! injected [`AudioBackend`], never starts the desktop host, and never registers
//! a listener. `--json` uses the CLI's terminal policy; styling is injected by
//! the composition root so this crate does not depend on the CLI crate.

use std::io::Write;
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Arg, ArgAction, ArgMatches, Command};
use stache_macos::audio::AudioBackend;
use stache_sdk::{CliContribution, CliExecutor};

use crate::list::{DeviceFilter, format_devices_table, list_devices};

/// Applies the CLI's JSON terminal styling.
pub type JsonHighlighter = Arc<dyn Fn(&str, bool) -> String + Send + Sync>;

/// Builds the `audio` contribution.
#[must_use]
pub fn contribution(backend: Arc<dyn AudioBackend>, highlight: JsonHighlighter) -> CliContribution {
    CliContribution::new(command, AudioCli { backend, highlight })
}

/// Builds the `audio` command tree.
#[must_use]
pub fn command() -> Command {
    Command::new("audio")
        .about("Inspect audio devices")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .subcommand(
            Command::new("list")
                .about("List audio devices")
                .arg(
                    Arg::new("json")
                        .long("json")
                        .short('j')
                        .help("Output JSON instead of a table")
                        .action(ArgAction::SetTrue),
                )
                .arg(
                    Arg::new("input")
                        .long("input")
                        .short('i')
                        .help("Show only input devices")
                        .action(ArgAction::SetTrue),
                )
                .arg(
                    Arg::new("output")
                        .long("output")
                        .short('o')
                        .help("Show only output devices")
                        .action(ArgAction::SetTrue),
                ),
        )
}

struct AudioCli {
    backend: Arc<dyn AudioBackend>,
    highlight: JsonHighlighter,
}

impl CliExecutor for AudioCli {
    fn execute(
        &self,
        matches: &ArgMatches,
        ansi: bool,
        out: &mut dyn Write,
        err: &mut dyn Write,
    ) -> ExitCode {
        let Some(("list", list)) = matches.subcommand() else {
            let _ = writeln!(err, "stache: audio requires the `list` subcommand");
            return ExitCode::FAILURE;
        };
        let filter = DeviceFilter::from_flags(list.get_flag("input"), list.get_flag("output"));
        let devices = list_devices(self.backend.as_ref(), filter);
        if list.get_flag("json") {
            match serde_json::to_string_pretty(&devices) {
                Ok(json) => {
                    let _ = writeln!(out, "{}", (self.highlight)(&json, ansi));
                }
                Err(error) => {
                    let _ = writeln!(err, "stache: audio JSON serialization error: {error}");
                    return ExitCode::FAILURE;
                }
            }
        } else {
            let _ = writeln!(out, "{}", format_devices_table(&devices));
        }
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use stache_macos::audio::{
        AudioChangeSubscription, AudioDevice, AudioDirection, AudioError, AudioWakeup,
    };

    use super::*;

    struct FakeBackend {
        devices: Vec<AudioDevice>,
        subscribes: AtomicUsize,
    }

    impl FakeBackend {
        fn new(devices: Vec<AudioDevice>) -> Arc<Self> {
            Arc::new(Self {
                devices,
                subscribes: AtomicUsize::new(0),
            })
        }
    }

    impl AudioBackend for FakeBackend {
        fn devices(&self) -> Vec<AudioDevice> { self.devices.clone() }

        fn default_device(&self, _: AudioDirection) -> Option<AudioDevice> { None }

        fn set_default_device(&self, _: AudioDirection, _: u32) -> Result<(), AudioError> {
            Err(AudioError::new("set default audio device", -1))
        }

        fn is_hdmi(&self, _: u32) -> bool { false }

        fn subscribe(
            &self,
            _: AudioWakeup,
        ) -> Result<Box<dyn AudioChangeSubscription>, AudioError> {
            self.subscribes.fetch_add(1, Ordering::SeqCst);
            Err(AudioError::new("add audio property listener", -1))
        }
    }

    fn device(id: u32, name: &str, input: bool, output: bool) -> AudioDevice {
        AudioDevice {
            id,
            name: name.to_owned(),
            input,
            output,
        }
    }

    fn plain(json: &str, _ansi: bool) -> String { json.to_owned() }

    fn run(
        backend: Arc<dyn AudioBackend>,
        highlight: JsonHighlighter,
        args: &[&str],
        ansi: bool,
    ) -> (ExitCode, String, String) {
        let matches = command()
            .try_get_matches_from(args)
            .unwrap_or_else(|error| panic!("{args:?}: {error}"));
        let contribution = contribution(backend, highlight);
        let executor = contribution.executor();
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = executor.execute(&matches, ansi, &mut out, &mut err);
        (
            code,
            String::from_utf8(out).expect("utf-8 stdout"),
            String::from_utf8(err).expect("utf-8 stderr"),
        )
    }

    fn two_devices() -> Arc<FakeBackend> {
        FakeBackend::new(vec![
            device(1, "Microphone", true, false),
            device(2, "AirPods Pro", false, true),
        ])
    }

    #[test]
    fn the_list_subcommand_requires_no_flags() {
        for args in [vec!["audio", "list"], vec![
            "audio", "list", "-io", "--json",
        ]] {
            let matches = command().try_get_matches_from(&args).expect("parse");
            let ("list", list) = matches.subcommand().expect("subcommand") else {
                panic!("{args:?}");
            };
            let _ = list;
        }
        assert!(command().try_get_matches_from(["audio"]).is_err());
    }

    #[test]
    fn each_flag_and_short_spelling_parses() {
        for (args, json, input, output) in [
            (vec!["audio", "list"], false, false, false),
            (vec!["audio", "list", "--json"], true, false, false),
            (vec!["audio", "list", "-j"], true, false, false),
            (vec!["audio", "list", "--input"], false, true, false),
            (vec!["audio", "list", "-i"], false, true, false),
            (vec!["audio", "list", "--output"], false, false, true),
            (vec!["audio", "list", "-o"], false, false, true),
            (vec!["audio", "list", "-io"], false, true, true),
            (vec!["audio", "list", "-io", "--json"], true, true, true),
        ] {
            let matches = command().try_get_matches_from(&args).expect("parse");
            let Some(("list", list)) = matches.subcommand() else {
                panic!("{args:?}");
            };
            assert_eq!(list.get_flag("json"), json, "{args:?}");
            assert_eq!(list.get_flag("input"), input, "{args:?}");
            assert_eq!(list.get_flag("output"), output, "{args:?}");
        }
    }

    #[test]
    fn the_table_matches_the_legacy_layout() {
        let backend = FakeBackend::new(vec![
            device(1, "Microphone", true, false),
            device(2, "AirPods Pro", false, true),
        ]);
        let (code, out, err) = run(backend, Arc::new(plain), &["audio", "list"], false);
        assert_eq!(code, ExitCode::SUCCESS, "{err}");
        let expected = concat!(
            "Name         Type        Input  Output\n",
            "-----------  ----------  -----  ------\n",
            "AirPods Pro  bluetooth   -      Y     \n",
            "Microphone   other       Y      -     \n",
            "\n",
        );
        assert_eq!(out, expected);
    }

    #[test]
    fn an_empty_table_matches_the_legacy_message() {
        let backend = FakeBackend::new(Vec::new());
        let (code, out, err) = run(backend, Arc::new(plain), &["audio", "list"], false);
        assert_eq!(code, ExitCode::SUCCESS, "{err}");
        assert_eq!(out, "No audio devices found.\n");
    }

    #[test]
    fn the_json_golden_preserves_keys_and_order() {
        let backend = FakeBackend::new(vec![
            device(1, "Microphone", true, false),
            device(2, "AirPods Pro", false, true),
        ]);
        let (code, out, err) = run(backend, Arc::new(plain), &["audio", "list", "--json"], false);
        assert_eq!(code, ExitCode::SUCCESS, "{err}");
        let expected = concat!(
            "[\n",
            "  {\n",
            "    \"name\": \"AirPods Pro\",\n",
            "    \"type\": \"bluetooth\",\n",
            "    \"input\": false,\n",
            "    \"output\": true\n",
            "  },\n",
            "  {\n",
            "    \"name\": \"Microphone\",\n",
            "    \"type\": \"other\",\n",
            "    \"input\": true,\n",
            "    \"output\": false\n",
            "  }\n",
            "]\n",
        );
        assert_eq!(out, expected);
    }

    #[test]
    fn the_direction_flags_filter_the_json() {
        let backend = two_devices();
        let (code, out, _) = run(
            backend,
            Arc::new(plain),
            &["audio", "list", "--input", "--json"],
            false,
        );
        assert_eq!(code, ExitCode::SUCCESS);
        let devices: Vec<crate::list::AudioDeviceInfo> = serde_json::from_str(&out).expect("json");
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].name, "Microphone");

        let (code, out, _) = run(
            two_devices(),
            Arc::new(plain),
            &["audio", "list", "-o", "-j"],
            false,
        );
        assert_eq!(code, ExitCode::SUCCESS);
        let devices: Vec<crate::list::AudioDeviceInfo> = serde_json::from_str(&out).expect("json");
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].name, "AirPods Pro");
    }

    #[test]
    fn the_terminal_flag_reaches_the_json_styler() {
        let seen = Arc::new(AtomicBool::new(false));
        let observed = Arc::clone(&seen);
        let highlight: JsonHighlighter = Arc::new(move |json, ansi| {
            observed.store(ansi, Ordering::SeqCst);
            json.to_owned()
        });
        let _ = run(
            two_devices(),
            Arc::clone(&highlight),
            &["audio", "list", "-j"],
            true,
        );
        assert!(seen.load(Ordering::SeqCst));
        let _ = run(two_devices(), highlight, &["audio", "list", "-j"], false);
        assert!(!seen.load(Ordering::SeqCst));
    }

    #[test]
    fn listing_never_registers_a_listener() {
        let backend = two_devices();
        let (code, _, err) = run(
            Arc::clone(&backend) as Arc<dyn AudioBackend>,
            Arc::new(plain),
            &["audio", "list"],
            false,
        );
        assert_eq!(code, ExitCode::SUCCESS, "{err}");
        assert_eq!(backend.subscribes.load(Ordering::SeqCst), 0);
    }
}
