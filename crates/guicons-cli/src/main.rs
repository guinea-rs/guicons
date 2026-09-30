use clap::{Parser, Subcommand};
use guicons_cli::{AddError, FetchSummary};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "icons")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Download every iconify/url icon into `.cache/guicons/` and lock it in `icons.lock`.
    Fetch {
        #[arg(long, default_value = "icons.gui.toml")]
        manifest: PathBuf,
        /// Re-download even if already cached, and re-lock.
        #[arg(long)]
        force: bool,
    },
    /// Like `fetch`, but always re-downloads and re-locks.
    Update {
        #[arg(long, default_value = "icons.gui.toml")]
        manifest: PathBuf,
    },
    /// Validate the manifest, reporting every error with a source snippet.
    Check {
        #[arg(long, default_value = "icons.gui.toml")]
        manifest: PathBuf,
    },
    /// Add an icon (iconify id or file path) to the manifest.
    Add {
        /// `set:name` iconify id, or a path to a local file.
        source: String,
        /// Manifest key to use. Defaults to the icon name or file stem.
        #[arg(long)]
        name: Option<String>,
        /// Comma-separated variant names, e.g. `filled,regular`.
        #[arg(long, value_delimiter = ',')]
        variants: Vec<String>,
        #[arg(long)]
        size: Option<u16>,
        #[arg(long, default_value = "icons.gui.toml")]
        manifest: PathBuf,
        /// Overwrite if the key already exists.
        #[arg(long)]
        force: bool,
    },
}

fn main() -> ExitCode {
    // Force the graphical (boxed, unicode, source-snippet) diagnostic
    // renderer regardless of TTY detection - `check`'s whole point is a
    // readable report, not a fallback that only appears in a real
    // terminal. Respects `NO_COLOR` for color, not for the layout itself.
    let _ = miette::set_hook(Box::new(|_| {
        Box::new(
            miette::MietteHandlerOpts::new()
                .force_graphical(true)
                .color(std::env::var_os("NO_COLOR").is_none())
                .build(),
        )
    }));

    let cli = Cli::parse();

    match cli.command {
        Command::Fetch { manifest, force } => run_fetch(&manifest, force),
        Command::Update { manifest } => run_fetch(&manifest, true),
        Command::Check { manifest } => run_check(&manifest),
        Command::Add { source, name, variants, size, manifest, force } => {
            run_add(&manifest, &source, name.as_deref(), &variants, size, force)
        }
    }
}

fn run_check(manifest: &PathBuf) -> ExitCode {
    let (entry_count, reports) = guicons_cli::check(manifest);
    if reports.is_empty() {
        println!("\u{2713} {} - {entry_count} icon{} OK", manifest.display(), if entry_count == 1 { "" } else { "s" });
        return ExitCode::SUCCESS;
    }

    // Advice-level notes (e.g. an iconify id that isn't cached locally
    // yet, so `check` can't confirm it resolves without a network fetch)
    // are printed but don't fail the command - only real errors do.
    let (errors, notes): (Vec<_>, Vec<_>) =
        reports.into_iter().partition(|report| report.severity() == Some(miette::Severity::Error));

    for report in errors.iter().chain(&notes) {
        eprintln!("{report:?}");
    }

    if !notes.is_empty() {
        eprintln!("{} note{} found", notes.len(), if notes.len() == 1 { "" } else { "s" });
    }
    if errors.is_empty() {
        println!("\u{2713} {} - {entry_count} icon{} OK", manifest.display(), if entry_count == 1 { "" } else { "s" });
        return ExitCode::SUCCESS;
    }
    eprintln!("{} error{} found", errors.len(), if errors.len() == 1 { "" } else { "s" });
    ExitCode::FAILURE
}

fn run_add(
    manifest: &PathBuf,
    source: &str,
    name: Option<&str>,
    variants: &[String],
    size: Option<u16>,
    force: bool,
) -> ExitCode {
    match guicons_cli::add(manifest, source, name, variants, size, force) {
        Ok(keys) => {
            for key in keys {
                println!("added {key}");
            }
            ExitCode::SUCCESS
        }
        Err(AddError::Manifest(errors)) => {
            eprintln!("existing manifest has errors:");
            for error in errors {
                eprintln!("  {error}");
            }
            ExitCode::FAILURE
        }
        Err(AddError::AlreadyExists(keys)) => {
            eprintln!("already in manifest (use --force to overwrite): {}", keys.join(", "));
            ExitCode::FAILURE
        }
        Err(AddError::Plan(message)) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
        Err(AddError::Io(message)) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
        Err(AddError::InvalidResult(errors)) => {
            eprintln!("refusing to write - result would not parse:");
            for error in errors {
                eprintln!("  {error}");
            }
            ExitCode::FAILURE
        }
    }
}

fn run_fetch(manifest: &PathBuf, force: bool) -> ExitCode {
    match guicons_cli::fetch(manifest, force) {
        Ok(summary) => {
            print_summary(&summary);
            if summary.is_success() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(errors) => {
            for error in errors {
                eprintln!("{error}");
            }
            ExitCode::FAILURE
        }
    }
}

fn print_summary(summary: &FetchSummary) {
    for id in &summary.fetched {
        println!("fetched   {id}");
    }
    for id in &summary.skipped {
        println!("cached    {id}");
    }
    for (id, error) in &summary.failed {
        eprintln!("failed    {id}: {error}");
    }
    println!(
        "{} fetched, {} already cached, {} failed",
        summary.fetched.len(),
        summary.skipped.len(),
        summary.failed.len()
    );
}
