// Release builds on Windows are window apps, so no console window opens behind the player.
// Started from a terminal, they print to that terminal instead (see `attach_console`).
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod cli_player;
mod gui;
mod lastfm;
mod log_capture;
mod settings;
mod update;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "freya", version, about = "A skinnable music player")]
struct Args {
    #[command(subcommand)]
    command: Option<Command>,

    /// Audio files or folders to play.
    files: Vec<PathBuf>,

    /// Skin to use: a `.slint` file, a skin folder or a `.freyaskin` package.
    #[arg(long)]
    skin: Option<PathBuf>,

    /// Renderer: femtovg (GPU), skia (GPU) or software. Defaults to software on macOS and
    /// femtovg elsewhere.
    #[arg(long)]
    renderer: Option<String>,
}

#[derive(Subcommand)]
enum Command {
    /// Play files or folders in the terminal, without a window.
    Play {
        /// Audio files or folders (searched recursively).
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },
    /// Tools for skin authors.
    #[command(subcommand)]
    Skin(SkinCommand),
}

#[derive(Subcommand)]
enum SkinCommand {
    /// Compile a skin and report errors and warnings.
    Check {
        /// A `.slint` file, a skin folder or a `.freyaskin` package.
        path: PathBuf,
    },
    /// Pack a skin folder into a `.freyaskin` package, after checking it.
    Pack {
        /// Folder with `skin.slint` and optionally `skin.toml`, images and fonts.
        folder: PathBuf,
        /// Output file; defaults to the folder's name with `.freyaskin`.
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
}

fn main() -> anyhow::Result<()> {
    attach_console();
    log_capture::init(settings::log_path().as_deref());
    let args = Args::parse();
    match args.command {
        Some(Command::Play { paths }) => cli_player::run(&paths),
        Some(Command::Skin(command)) => skin_command(command),
        None => gui::run(gui::Options {
            files: args.files,
            skin: args.skin,
            renderer: args.renderer,
        }),
    }
}

fn skin_command(command: SkinCommand) -> anyhow::Result<()> {
    use freya_skin::{SkinInfo, SkinSource};

    let report = |warnings: &[String]| {
        for warning in warnings {
            eprintln!("warning: {warning}");
        }
    };
    match command {
        SkinCommand::Check { path } => {
            let source = SkinSource::from_path(&path);
            let info = SkinInfo::read(&source)?;
            let warnings = freya_skin::check(&source)?;
            report(&warnings);
            println!(
                "{}: OK ({} warning{})",
                info.name,
                warnings.len(),
                if warnings.len() == 1 { "" } else { "s" }
            );
        }
        SkinCommand::Pack { folder, output } => {
            let output = match output {
                Some(output) => output,
                None => default_package(&folder)?,
            };
            let warnings = freya_skin::pack(&folder, &output)?;
            report(&warnings);
            println!("wrote {}", output.display());
        }
    }
    Ok(())
}

/// The package for `folder` when no output is named: next to it, with its whole name, so a
/// folder `night.v2` makes `night.v2.freyaskin` and not `night.freyaskin`, another skin's.
fn default_package(folder: &std::path::Path) -> anyhow::Result<PathBuf> {
    let folder = folder.canonicalize()?;
    let name = folder
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("name the package with --output"))?;
    let mut file = name.to_os_string();
    file.push(".");
    file.push(freya_skin::PACKAGE_EXTENSION);
    Ok(folder.with_file_name(file))
}

/// On Windows, a window app has no console; when started from a terminal, this connects it to
/// that terminal so `freya play` and `freya skin` can print.
#[cfg(windows)]
#[allow(unsafe_code, reason = "a plain Windows API call without pointers")]
fn attach_console() {
    use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
    // Fails, harmlessly, when there is no terminal: started from the Start menu or Explorer.
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

#[cfg(not(windows))]
fn attach_console() {}
