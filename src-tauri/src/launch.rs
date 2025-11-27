use crate::cli;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum LaunchMode {
    Ui,
    CliEvent,
    CliInvalid,
}

impl LaunchMode {
    fn from_args(args: &[String]) -> Self {
        match cli::preview_cli_event(args) {
            Ok(Some(_)) | Err(cli::CliParseError::Help(_)) => Self::CliEvent,
            Ok(None) => Self::Ui,
            Err(cli::CliParseError::MissingArguments)
                if cli::should_render_help_on_empty_invocation() =>
            {
                Self::CliEvent
            }
            Err(err) => {
                eprintln!("barba: ignoring CLI invocation: {err}");
                Self::CliInvalid
            }
        }
    }

    const fn should_launch_ui(self) -> bool { matches!(self, Self::Ui) }

    const fn exit_code(self) -> i32 {
        match self {
            Self::CliInvalid => 1,
            _ => 0,
        }
    }
}

pub fn get_launch_mode() -> (bool, i32) {
    let cli_args: Vec<String> = std::env::args().collect();
    let launch_mode = LaunchMode::from_args(&cli_args);

    if matches!(launch_mode, LaunchMode::CliInvalid) {
        std::process::exit(launch_mode.exit_code());
    }

    (launch_mode.should_launch_ui(), launch_mode.exit_code())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(std::string::ToString::to_string).collect()
    }

    #[test]
    fn launch_mode_switches_to_cli_for_events() {
        let mode = LaunchMode::from_args(&args(&["barba", "focus-changed"]));
        assert_eq!(mode, LaunchMode::CliEvent);
    }

    #[test]
    fn launch_mode_defaults_to_ui_without_payload() {
        let mode = LaunchMode::from_args(&args(&["barba"]));
        assert_eq!(mode, LaunchMode::Ui);
    }
}
