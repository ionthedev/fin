//! Terminal entry points, with non-interactive help, completion, and snapshot modes.

use clap::{CommandFactory, Parser};
use std::ffi::OsString;
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "fin-tui",
    version,
    about = "A dark Mermaid workspace for your terminal"
)]
struct Arguments {
    /// Open a UTF-8 Mermaid file (starts with an example when omitted).
    file: Option<PathBuf>,
    /// Print a plain text workspace snapshot without entering terminal mode.
    #[arg(long, value_name = "WIDTHxHEIGHT", value_parser = parse_size, conflicts_with = "completions")]
    snapshot: Option<(u16, u16)>,
    /// Print a shell completion script to stdout.
    #[arg(long, value_enum, conflicts_with = "file")]
    completions: Option<clap_complete::Shell>,
}

fn parse_size(value: &str) -> Result<(u16, u16), String> {
    let (width, height) = value
        .split_once('x')
        .ok_or_else(|| "Use WIDTHxHEIGHT, for example 120x36.".to_owned())?;
    let width: u16 = width
        .parse()
        .map_err(|_| "Width must be a number.".to_owned())?;
    let height: u16 = height
        .parse()
        .map_err(|_| "Height must be a number.".to_owned())?;
    if !(40..=300).contains(&width) || !(12..=120).contains(&height) {
        return Err("Choose a width from 40–300 and a height from 12–120.".into());
    }
    Ok((width, height))
}

/// Launch the terminal interface or handle output-only flags before terminal setup.
pub fn run(args: impl IntoIterator<Item = OsString>) -> Result<(), Box<dyn std::error::Error>> {
    let arguments = match Arguments::try_parse_from(args) {
        Ok(arguments) => arguments,
        Err(error)
            if matches!(
                error.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            ) =>
        {
            error.print()?;
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    };
    if let Some(shell) = arguments.completions {
        clap_complete::generate(
            shell,
            &mut Arguments::command(),
            "fin-tui",
            &mut io::stdout(),
        );
        return Ok(());
    }
    if let Some((width, height)) = arguments.snapshot {
        let text = crate::tui::snapshot(arguments.file, width, height)?;
        io::stdout().lock().write_all(text.as_bytes())?;
        return Ok(());
    }
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err("Interactive mode needs a terminal. Use --snapshot 120x36 [diagram.mmd] for plain text output, or --help.".into());
    }
    crate::tui::run(arguments.file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_dimensions_are_bounded_and_options_are_unambiguous() {
        assert_eq!(parse_size("120x36"), Ok((120, 36)));
        for invalid in ["100", "0x0", "9999x9999", "80x0", "abcx36"] {
            assert!(parse_size(invalid).is_err());
        }
        assert!(Arguments::try_parse_from(["fin-tui", "--snapshot", "120x36", "a.mmd"]).is_ok());
        assert!(
            Arguments::try_parse_from(["fin-tui", "--snapshot", "120x36", "--completions", "bash"])
                .is_err()
        );
        assert!(Arguments::try_parse_from(["fin-tui", "--unknown"]).is_err());
    }
}
