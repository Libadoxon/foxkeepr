mod event;
mod hypr;
mod state;
mod tracker;

use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use clap::{Args, Parser, Subcommand};
use tracing::{debug, error, info};
use tracing_subscriber::EnvFilter;

use hypr::Hypr;
use state::State;
use tracker::{Action, Config, DEFAULT_CLASSES, Tracker};

#[derive(Parser)]
#[command(
    version,
    about = "Restore Firefox windows on the Hyprland workspaces they were on after restart"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    #[command(flatten)]
    opts: Opts,
}

#[derive(Subcommand)]
enum Command {
    /// Watch Hyprland and restore windows as they reappear (the default).
    Run,
    /// Print the layout currently on record.
    Status,
    /// Match the windows open right now against the saved layout and move them.
    Restore,
}

#[derive(Args, Clone)]
struct Opts {
    /// Where the layout is stored.
    #[arg(long, value_name = "PATH", global = true)]
    state: Option<PathBuf>,
    /// Window class to treat as the browser. Repeatable; replaces the defaults.
    #[arg(long = "class", value_name = "CLASS", global = true)]
    classes: Vec<String>,
    /// Seconds after the first restored window during which matching stays open.
    #[arg(long, default_value_t = 60, value_name = "SECS", global = true)]
    grace: u64,
    /// Seconds to let a window's title settle before matching it by position.
    #[arg(long, default_value_t = 8, value_name = "SECS", global = true)]
    settle: u64,
    /// Seconds the window set must hold still before the layout is persisted.
    #[arg(long, default_value_t = 5, value_name = "SECS", global = true)]
    stability: u64,
    /// Only ever match restored windows by exact title.
    #[arg(long, global = true)]
    no_order_fallback: bool,
    /// Log every event and decision. `RUST_LOG` overrides this.
    #[arg(long, short, global = true)]
    verbose: bool,
}

impl Opts {
    fn config(&self) -> Config {
        Config {
            classes: if self.classes.is_empty() {
                DEFAULT_CLASSES.iter().map(|s| s.to_string()).collect()
            } else {
                self.classes.clone()
            },
            grace: Duration::from_secs(self.grace),
            settle: Duration::from_secs(self.settle),
            stability: Duration::from_secs(self.stability),
            order_fallback: !self.no_order_fallback,
        }
    }

    fn state_path(&self) -> PathBuf {
        self.state.clone().unwrap_or_else(state::default_path)
    }
}

/// Init logging. Drop duplicate info in journal
fn init_logging(verbose: bool) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new(if verbose {
            "foxkeepr=debug"
        } else {
            "foxkeepr=info"
        })
    });
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(io::stderr);
    if std::env::var_os("JOURNAL_STREAM").is_some() {
        builder
            .without_time()
            .with_target(false)
            .with_ansi(false)
            .init();
    } else {
        builder.init();
    }
}

fn main() {
    let cli = Cli::parse();
    init_logging(cli.opts.verbose);
    let result = match cli.command.unwrap_or(Command::Run) {
        Command::Run => run(&cli.opts),
        Command::Status => status(&cli.opts),
        Command::Restore => restore(&cli.opts),
    };
    if let Err(e) = result {
        error!("{e}");
        std::process::exit(1);
    }
}

fn status(opts: &Opts) -> io::Result<()> {
    let path = opts.state_path();
    let state = State::load(&path)?;
    println!("{}", path.display());
    if state.session.is_empty() {
        println!("no layout recorded yet");
        return Ok(());
    }
    for record in &state.session {
        println!(
            "  workspace {:<14} {:<10} {}",
            record.workspace, record.class, record.title
        );
    }
    Ok(())
}

fn restore(opts: &Opts) -> io::Result<()> {
    let cfg = opts.config();
    let hypr = Hypr::from_env()?;
    let state = State::load(&opts.state_path())?;
    let clients = hypr.clients()?;
    let actions = tracker::plan_restore(&cfg, &state.session, &clients);
    if actions.is_empty() {
        info!("nothing to move");
    }
    for action in actions {
        apply(&hypr, &action);
    }
    Ok(())
}

fn run(opts: &Opts) -> io::Result<()> {
    let cfg = opts.config();
    let path = opts.state_path();
    let hypr = Hypr::from_env()?;
    let state = State::load(&path)?;

    let mut tracker = Tracker::new(cfg, state, Instant::now());
    // Subscribe before taking stock, so nothing slips through the gap.
    let mut events = hypr.events(Duration::from_millis(500))?;
    tracker.adopt(&hypr.clients()?, Instant::now());

    info!(
        armed = tracker.pending_len(),
        state = %path.display(),
        "watching Hyprland"
    );

    loop {
        let line = events.next_line()?;
        let now = Instant::now();
        let mut actions = Vec::new();

        if let Some(line) = line
            && let Some(ev) = event::parse(&line)
        {
            debug!(?ev, "event");
            actions.extend(tracker.on_event(ev, now));
        }
        actions.extend(tracker.tick(Instant::now()));

        for action in &actions {
            apply(&hypr, action);
        }
        if tracker.take_save()
            && let Err(e) = tracker.state().save(&path)
        {
            error!(state = %path.display(), "could not save layout: {e}");
        }
    }
}

fn apply(hypr: &Hypr, action: &Action) {
    let Action::Move { addr, workspace } = action;
    match hypr.move_window(addr, workspace) {
        Ok(()) => info!(%addr, %workspace, "moved window"),
        Err(e) => error!(%addr, %workspace, "could not move window: {e}"),
    }
}
