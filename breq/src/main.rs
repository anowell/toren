//! breq — the CLI for workspaces as places.
//!
//! Two orthogonal verb families:
//!
//! - **Place verbs** (`setup`, `do`, `sh`, `teardown`) manage the workspace. The only tracker
//!   side effect anywhere in them is `do <task-id>` claiming the task it was handed.
//! - **Task verbs** (`set <ws> task.status ...`, and the `breq-complete` / `breq-abort` scripts
//!   over it) update the tracker, and never touch the workspace.
//!
//! They're different axes, which is the point: shipping a piece of work and being finished with
//! the place you did it in are separate decisions. `breq list` shows when they've diverged.

use anyhow::{Context, Result};
use clap::{CommandFactory, Parser, Subcommand};
use colored::Colorize;
use std::io::IsTerminal;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use toren_lib::{
    AgentSpec, CollectOptions, Config, Family, Place, PlaceRegistry, PluginContext, PluginManager,
    Segment, Sets,
};
use toren_mirror::PaneRole;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::fmt::time::FormatTime;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::Layer;

mod mirror;
mod render;

/// Custom time formatter that displays only HH:MM:SS (UTC)
struct ShortTime;

impl FormatTime for ShortTime {
    fn format_time(&self, w: &mut tracing_subscriber::fmt::format::Writer<'_>) -> std::fmt::Result {
        use std::time::{SystemTime, UNIX_EPOCH};

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let secs_of_day = now % 86400;
        write!(
            w,
            "{:02}:{:02}:{:02}",
            secs_of_day / 3600,
            (secs_of_day % 3600) / 60,
            secs_of_day % 60
        )
    }
}

#[derive(Parser)]
#[command(name = "breq")]
#[command(about = "Composable workspace orchestration for coding agents")]
#[command(
    after_help = "Workflow verbs (breq-<name> scripts on PATH) are dispatched by name:\n  \
                        breq complete <ws>   ship: mark the workspace's tasks done\n  \
                        breq abort <ws>      hand the workspace's tasks back\n\
                        \nSee `breq doctor` to install the shipped ones."
)]
struct Cli {
    /// Increase verbosity (-v for DEBUG, -vv for TRACE)
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,

    /// Path to config file (default: auto-discovered ~/.toren/config.kdl)
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run a coding agent in a workspace
    ///
    /// Needs a task or a prompt. With neither, use `breq sh` to open a shell instead.
    Do {
        /// Task to work on (e.g. "tor-bau" or "runes:tor-bau"); claims it and adds its context
        task: Option<String>,

        /// Target workspace; defaults to the one you're standing in, else a new one
        #[arg(short = 'w', long)]
        workspace: Option<String>,

        /// Prompt for the agent
        #[arg(short, long)]
        prompt: Option<String>,

        /// Model override, passed through to the agent
        #[arg(short, long)]
        model: Option<String>,

        /// Agent to use (e.g. "claude", "codex"); defaults to the workspace's, then config
        #[arg(long)]
        agent: Option<String>,

        /// Resume the workspace's most recent agent session, or `--resume=<id>` for a specific
        /// one (`breq get <ws> agent.sessions` lists them)
        ///
        /// The id only attaches with `=`, so `breq do --resume <task>` still reads as a task.
        #[arg(long, num_args = 0..=1, require_equals = true, default_missing_value = "")]
        resume: Option<String>,

        /// Segment to use (defaults to the current directory's segment)
        #[arg(short, long)]
        segment: Option<String>,

        /// Exec the agent directly instead of running it inside an rmux session
        #[arg(long = "no-rmux")]
        no_rmux: bool,

        /// Replace an agent already running in this workspace instead of refusing
        #[arg(long)]
        force: bool,

        /// Additional arguments passed directly to the agent CLI
        #[arg(last = true)]
        passthrough: Vec<String>,
    },

    /// Open a shell in a workspace, or run a command there
    #[command(visible_alias = "sh")]
    Shell {
        /// Workspace name (e.g. "one", "two")
        workspace: Option<String>,

        /// Run a workspace hook (setup or destroy) from the current directory
        #[arg(long)]
        hook: Option<HookArg>,

        /// Mirror an existing window of the workspace's session instead of a shell
        /// (`agent`, `shell-2`, `cmd`, …)
        #[arg(long, conflicts_with = "cmd")]
        window: Option<String>,

        /// Segment to use
        #[arg(short, long)]
        segment: Option<String>,

        /// Exec directly instead of mirroring a pane of the workspace's rmux session
        #[arg(long = "no-rmux")]
        no_rmux: bool,

        /// Keep the pane after the process exits, waiting for a key
        #[arg(long, overrides_with = "no_hold")]
        hold: bool,

        /// Let the pane close with the process, the way a shell does
        #[arg(long = "no-hold", overrides_with = "hold")]
        no_hold: bool,

        /// Command to run in the workspace directory (after --)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        cmd: Vec<String>,
    },

    /// Create a workspace (no task, no agent)
    Setup {
        /// Workspace name (e.g. "one"); omit to take the next free slot
        workspace: Option<String>,

        /// Stack on another workspace instead of the segment tip
        #[arg(long)]
        from: Option<String>,

        /// Segment to use
        #[arg(short, long)]
        segment: Option<String>,
    },

    /// Destroy a workspace. Task-agnostic: no status changes, no push.
    Teardown {
        /// Workspace name
        workspace: String,

        /// Kill processes and live panes running in the workspace
        #[arg(long)]
        kill: bool,

        /// Keep the working copy and its VCS registration; drop only breq's state
        #[arg(long = "no-delete")]
        no_delete: bool,

        /// Segment to use
        #[arg(short, long)]
        segment: Option<String>,
    },

    /// One row per workspace: sessions, changes, delivery, tasks
    List {
        /// List every segment
        #[arg(short, long)]
        all: bool,

        /// List a specific segment
        #[arg(short, long, conflicts_with = "all")]
        segment: Option<String>,

        /// Refresh delivery status before rendering (the only path that hits the network)
        #[arg(long)]
        refresh: bool,

        /// Skip task lookups (no resolver calls at all)
        #[arg(long)]
        local: bool,
    },

    /// Read a workspace: everything, or one key
    Get {
        /// Workspace name, and/or a key. With one argument, a known workspace wins.
        #[arg(num_args = 0..=2)]
        args: Vec<String>,

        /// Read a specific linked task's fields
        #[arg(long)]
        task: Option<String>,

        /// Refresh delivery status first
        #[arg(long)]
        refresh: bool,

        /// Render as JSON
        #[arg(long)]
        json: bool,

        /// Segment to use
        #[arg(short, long)]
        segment: Option<String>,
    },

    /// Write a workspace state field, or a task field (pass-through to its source)
    ///
    /// List-valued keys take +/- prefixes: `breq set one +task runes:tor-456`.
    Set {
        /// Workspace name (optional inside a workspace), key, value
        ///
        /// `allow_hyphen_values` is what lets `-task` read as a list-removal key rather than
        /// an unknown flag.
        #[arg(num_args = 2..=3, allow_hyphen_values = true)]
        args: Vec<String>,

        /// Apply a task field write to one linked task rather than all of them
        #[arg(long)]
        task: Option<String>,

        /// Segment to use
        #[arg(short, long)]
        segment: Option<String>,
    },

    /// Remove leftovers: orphaned workspace dirs
    Cleanup {
        /// Segment to clean up
        #[arg(short, long)]
        segment: Option<String>,

        /// Clean up all segments
        #[arg(short, long, conflicts_with = "segment")]
        all: bool,
    },

    /// Initialize toren.kdl in the current repository
    Init {
        /// Add toren.kdl to .git/info/exclude instead of committing it
        #[arg(long)]
        stealth: bool,
    },

    /// Detect known-bad state and fix it. Nothing here ever runs implicitly.
    Doctor {
        /// Apply the repairs instead of only reporting them
        #[arg(long)]
        fix: bool,
    },

    /// Manage Rhai resolver plugins under ~/.toren/plugins
    Plugin {
        #[command(subcommand)]
        cmd: PluginCmd,
    },
}

#[derive(Subcommand)]
enum PluginCmd {
    /// List installed and available plugins
    List,

    /// Install a plugin from the contrib repo or a local path
    ///
    /// TARGET is `<family>/<name>` (tasks, agents, delivery) or a local .rhai path whose
    /// parent directory names the family.
    Install {
        /// `tasks/<name>`, `agents/<name>`, `delivery/<name>`, or a local .rhai path
        target: String,
    },
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum HookArg {
    Setup,
    Destroy,
}

/// Subcommands clap owns. Anything else is looked up as a `breq-<name>` script.
const BUILTIN_VERBS: &[&str] = &[
    "do", "shell", "sh", "setup", "teardown", "list", "get", "set", "cleanup", "init", "doctor",
    "plugin", "help",
];

fn main() -> Result<()> {
    let raw_args: Vec<String> = std::env::args().collect();

    if let Some(exit) = dispatch_external(&raw_args)? {
        std::process::exit(exit);
    }

    let cli = Cli::parse();

    let log_level = match cli.verbose {
        0 => tracing::Level::INFO,
        1 => tracing::Level::DEBUG,
        _ => tracing::Level::TRACE,
    };
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                // Stdout belongs to the pane mirror, which paints a raw-mode screen onto it.
                .with_writer(std::io::stderr)
                .with_target(false)
                .with_timer(ShortTime)
                .with_filter(LevelFilter::from_level(log_level)),
        )
        .with(toren_lib::logging::file_layer("breq"))
        .init();

    let config = Config::load_from(cli.config.as_deref())?;

    match cli.command {
        Commands::Do {
            task,
            workspace,
            prompt,
            model,
            agent,
            resume,
            segment,
            no_rmux,
            force,
            passthrough,
        } => cmd_do(
            &config,
            DoArgs {
                task,
                workspace,
                prompt,
                model,
                agent,
                resume,
                segment,
                no_rmux,
                force,
                passthrough,
            },
        ),
        Commands::Shell {
            workspace,
            hook,
            window,
            segment,
            no_rmux,
            hold,
            no_hold,
            cmd,
        } => cmd_shell(
            &config,
            ShellArgs {
                workspace,
                hook,
                window,
                segment,
                no_rmux,
                hold: resolve_hold(hold, no_hold),
                cmd,
            },
        ),
        Commands::Setup {
            workspace,
            from,
            segment,
        } => cmd_setup(&config, workspace, from, segment.as_deref()),
        Commands::Teardown {
            workspace,
            kill,
            no_delete,
            segment,
        } => cmd_teardown(&config, &workspace, kill, no_delete, segment.as_deref()),
        Commands::List {
            all,
            segment,
            refresh,
            local,
        } => cmd_list(&config, all, segment.as_deref(), refresh, local),
        Commands::Get {
            args,
            task,
            refresh,
            json,
            segment,
        } => cmd_get(&config, args, task, refresh, json, segment.as_deref()),
        Commands::Set {
            args,
            task,
            segment,
        } => cmd_set(&config, args, task, segment.as_deref()),
        Commands::Cleanup { segment, all } => cmd_cleanup(&config, all, segment.as_deref()),
        Commands::Init { stealth } => cmd_init(stealth),
        Commands::Doctor { fix } => cmd_doctor(&config, fix),
        Commands::Plugin { cmd } => cmd_plugin(cmd),
    }
}

// ─── external verbs ─────────────────────────────────────────────────────────

/// Hand an unknown subcommand to a `breq-<name>` script, git-style.
///
/// Returns the exit code if something external ran, `None` if clap should take over.
fn dispatch_external(raw_args: &[String]) -> Result<Option<i32>> {
    let mut idx = 1;
    while idx < raw_args.len() {
        let arg = &raw_args[idx];
        if arg == "-v" || arg == "-vv" || arg == "-vvv" || arg == "--verbose" {
            idx += 1;
        } else if arg == "--config" {
            idx += 2;
        } else {
            break;
        }
    }

    let Some(subcmd) = raw_args.get(idx) else {
        return Ok(None);
    };
    if subcmd.starts_with('-') || BUILTIN_VERBS.contains(&subcmd.as_str()) {
        return Ok(None);
    }

    let args = &raw_args[idx + 1..];

    if let Some(script) = toren_lib::scripts::find(subcmd) {
        let status = Command::new(&script)
            .args(args)
            .status()
            .with_context(|| format!("Failed to run {}", script.display()))?;
        return Ok(Some(status.code().unwrap_or(1)));
    }

    // Aliases stay as the last resort: one-line shell expansions in config.
    if let Ok(config) = Config::load() {
        if let Some(template) = config.aliases.get(subcmd.as_str()) {
            let expanded = toren_lib::alias::expand_alias(template, args);
            let code =
                toren_lib::alias::execute_alias(&expanded, &std::collections::HashMap::new())?;
            return Ok(Some(code));
        }
    }

    eprintln!("Unknown command '{}'.", subcmd);
    eprintln!(
        "Workflow verbs are scripts: create `breq-{}` on your PATH (or in {}).",
        subcmd,
        toren_lib::tilde_shorten(&toren_lib::scripts::bin_dir())
    );
    eprintln!();
    Cli::command().print_help().ok();
    Ok(Some(2))
}

// ─── shared plumbing ────────────────────────────────────────────────────────

fn plugins() -> Result<PluginManager> {
    PluginManager::new(&toren_lib::toren_root().join("plugins"))
}

/// The place a command should act on: `-w`, else where you're standing.
fn target_place(
    registry: &PlaceRegistry,
    workspace: Option<&str>,
    segment: Option<&str>,
) -> Result<Option<Place>> {
    if let Some(name) = workspace {
        let segment = registry.segment(segment)?;
        return registry.require(&segment, name).map(Some);
    }
    Ok(registry.resolve_from_env())
}

/// Resolve a place for read/write commands, insisting on one.
fn require_place(
    registry: &PlaceRegistry,
    workspace: Option<&str>,
    segment: Option<&str>,
) -> Result<Place> {
    target_place(registry, workspace, segment)?.with_context(|| {
        "No workspace given, and this directory isn't inside one.\n\
         Name it explicitly, or run from inside the workspace."
            .to_string()
    })
}

fn plugin_ctx(place: &Place) -> PluginContext {
    PluginContext::new(
        Some(place.segment_path.clone()),
        Some(place.segment.clone()),
    )
}

// ─── do ─────────────────────────────────────────────────────────────────────

struct DoArgs {
    task: Option<String>,
    workspace: Option<String>,
    prompt: Option<String>,
    model: Option<String>,
    agent: Option<String>,
    /// `None` — a fresh run; `Some("")` — the most recent session; `Some(id)` — that one.
    resume: Option<String>,
    segment: Option<String>,
    no_rmux: bool,
    force: bool,
    passthrough: Vec<String>,
}

fn cmd_do(config: &Config, args: DoArgs) -> Result<()> {
    let registry = PlaceRegistry::new(config)?;
    let plugins = plugins()?;

    // Only reach for stdin when nothing else said what to do. Reading it unconditionally
    // hangs `breq do <task>` any time stdin is an open pipe — which is exactly how scripts and
    // agents invoke it.
    let user_prompt = match (&args.prompt, &args.task) {
        (Some(prompt), _) => Some(prompt.clone()),
        (None, None) => read_piped_prompt()?,
        (None, Some(_)) => None,
    };

    if args.task.is_none() && user_prompt.is_none() && args.resume.is_none() {
        anyhow::bail!(
            "`breq do` needs a task or a prompt.\n  \
             breq do <task-id>              work a task\n  \
             breq do -p \"...\"               work from a prompt\n\
             \nTo open a shell or run a command in a workspace instead, use `breq sh`."
        );
    }

    // Where: the named workspace, the one we're standing in, or a fresh one.
    let mut place = match target_place(
        &registry,
        args.workspace.as_deref(),
        args.segment.as_deref(),
    )? {
        Some(place) => place,
        None => {
            let segment = registry.segment(args.segment.as_deref())?;
            let place =
                registry.create(&segment, None, None, config.ancillaries.max_per_segment)?;
            eprintln!("Workspace: {}", place.path.display());
            place
        }
    };

    // What: task context, if a task was named. Claiming it here is the only tracker side
    // effect in any place verb.
    // The prompt is kept apart from the title: the title is mutable and derived, the prompt is
    // what was actually asked for and is the fallback the chain lands on.
    if let Some(ref p) = user_prompt {
        if place.state.prompt.is_none() {
            place.state.prompt = Some(p.clone());
        }
    }

    let mut prompt = user_prompt.clone();
    if let Some(ref task_ref) = args.task {
        let task = resolve_task(&plugins, config, &place, task_ref)?;
        let link = toren_lib::format_link(&task.source, &task.id);

        let ctx = plugin_ctx(&place);
        if let Err(e) = plugins.resolve_claim(&task.source, &task.id, "claude", ctx) {
            eprintln!("warning: could not claim {}: {:#}", link, e);
        }

        // Resolving the task was a live tracker call; the cache gets it for free.
        toren_lib::sets::cache_task(&place, &task);
        place.state.add_task(&task.source, &task.id);
        // A tracker with no title to give falls through to the prompt rather than erasing the
        // title the workspace already carries.
        let title = Some(task.title.trim())
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .or_else(|| user_prompt.as_deref().map(|p| first_chars(p, TITLE_CHARS)));
        if let Some(title) = title {
            place.state.title = Some(title);
        }
        prompt = Some(compose_prompt(&task, user_prompt.as_deref()));
        eprintln!("Task: {} — {}", link, task.title);
    } else if let Some(ref p) = user_prompt {
        // A task-less workspace needs *something* to be legible in `breq list`.
        place.state.title = Some(first_chars(p, TITLE_CHARS));
    }

    let stored = place.agent();
    let agent = AgentSpec::resolve(
        &plugins,
        args.agent.as_deref(),
        stored.as_ref(),
        config.ancillaries.agent.as_deref(),
    )?;
    let agent = AgentSpec {
        name: agent.name,
        model: args.model.clone().or(agent.model),
    };
    place.state.set_agent(&agent.name, agent.model.as_deref());
    place.save()?;

    // Which session this run belongs to: named, most recent, or one the agent has yet to open.
    let session_id = match args.resume.as_deref() {
        Some("") => toren_lib::sessions::resume_target(&place, &plugins, &agent.name, None),
        Some(id) => toren_lib::sessions::resume_target(&place, &plugins, &agent.name, Some(id)),
        None => None,
    };

    let argv = if args.resume.is_some() {
        agent.resume_argv_for(&plugins, session_id.as_deref(), prompt.as_deref(), false)?
    } else {
        agent.argv(&plugins, prompt.as_deref(), false)?
    };

    toren_lib::sessions::record_start(&mut place, &plugins, &agent.name, session_id.as_deref())?;

    match &session_id {
        Some(id) => eprintln!(
            "Starting {} in {} (session {})\n",
            agent,
            place.path.display(),
            id
        ),
        None => eprintln!("Starting {} in {}\n", agent, place.path.display()),
    }
    launch(
        &place,
        &agent,
        &argv,
        args.no_rmux,
        args.force,
        &args.passthrough,
    )
}

/// Resolve a `task-id` or `source:task-id` through the task resolvers.
fn resolve_task(
    plugins: &PluginManager,
    config: &Config,
    place: &Place,
    task_ref: &str,
) -> Result<toren_lib::ResolvedTask> {
    let inferred = toren_lib::infer_task_fields(Some(task_ref), None, None, None);
    let id = inferred
        .task_id
        .clone()
        .with_context(|| format!("Could not read a task id out of '{}'", task_ref))?;

    let ctx = plugin_ctx(place);
    match inferred.task_source {
        Some(source) => plugins.resolve_info(&source, &id, ctx),
        None => {
            let sources = plugins.effective_sources(&config.tasks.sources);
            plugins.resolve_info_multi(&sources, &id, ctx)
        }
    }
}

/// Task context first, then whatever you asked for.
fn compose_prompt(task: &toren_lib::ResolvedTask, user_prompt: Option<&str>) -> String {
    let mut prompt = format!("{} {}: {}", task.source, task.id, task.title);
    if let Some(description) = task.description.as_deref().filter(|d| !d.trim().is_empty()) {
        prompt.push_str("\n\n");
        prompt.push_str(description);
    }
    if let Some(user) = user_prompt.filter(|p| !p.trim().is_empty()) {
        prompt.push_str("\n\n---\n\n");
        prompt.push_str(user);
    }
    prompt
}

/// How much of a prompt becomes the workspace title.
const TITLE_CHARS: usize = 80;

fn first_chars(s: &str, n: usize) -> String {
    let line = s.lines().find(|l| !l.trim().is_empty()).unwrap_or(s);
    line.chars().take(n).collect()
}

fn read_piped_prompt() -> Result<Option<String>> {
    if std::io::stdin().is_terminal() {
        return Ok(None);
    }
    use std::io::Read;
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf)?;
    let trimmed = buf.trim().to_string();
    Ok(if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    })
}

/// Hand the terminal to the agent.
///
/// Inside rmux the agent runs in the session's `agent` window and this terminal becomes a mirror
/// of that pane — no multiplexer chrome, but the agent survives closing the terminal and the
/// browser shows the same pane. Without a terminal to draw in there is nothing to mirror, so the
/// agent is left running and where to find it is reported instead.
fn launch(
    place: &Place,
    agent: &AgentSpec,
    argv: &[String],
    no_rmux: bool,
    force: bool,
    passthrough: &[String],
) -> Result<()> {
    let mut argv = argv.to_vec();
    argv.extend(passthrough.iter().cloned());

    if !no_rmux && toren_lib::rmux::is_available() {
        let session = place.session_name();

        // Sessions from a previous incarnation of this slot point at a directory that no
        // longer exists; never mirror one.
        let killed =
            toren_lib::rmux::reconcile(&place.segment, &place.name, place.uid().as_deref());
        if killed > 0 {
            eprintln!("Reconciled {} stale session(s) for this workspace", killed);
        }

        // Spawning replaces the agent window, SIGKILLing whatever was working there.
        if !force && toren_lib::rmux::agent_is_running(&session) {
            anyhow::bail!(
                "An agent is already running in workspace '{}'.\n  \
                 Watch it:    breq sh {} --window agent\n  \
                 Replace it:  breq do -w {} --force ...",
                place.name,
                place.name,
                place.name,
            );
        }

        toren_lib::rmux::ensure_session(&session, &place.path, &place.env())?;
        toren_lib::rmux::spawn_agent(&session, &place.path, &argv)?;

        if !mirror::owns_terminal() {
            eprintln!(
                "rmux session: {} (no terminal here to mirror it in)",
                session
            );
            return Ok(());
        }

        eprintln!(
            "rmux session: {} (closing this terminal leaves the agent running)\n",
            session
        );

        // `<ENTER>` on the held pane resumes the session the agent leaves behind rather than
        // starting it cold — the one place toren's held pane beats a blind re-run.
        let spec = agent.clone();
        let passthrough = passthrough.to_vec();
        let rerun: mirror::Rerun =
            Box::new(move |place: &Place| resume_agent(place, &spec, &passthrough));

        let code = mirror::run(
            place,
            mirror::Pane {
                window: toren_lib::rmux::AGENT_WINDOW.to_string(),
                role: PaneRole::Agent,
                hold: true,
            },
            rerun,
        )?;
        std::process::exit(code);
    }

    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]).current_dir(&place.path);
    for (key, value) in place.env() {
        cmd.env(key, value);
    }
    let err = cmd.exec();
    Err(err).context(format!("Failed to exec {}", argv[0]))
}

/// Start the agent again on the session that just ended, in the same window.
///
/// The held pane is what makes this reachable, and the session record is what makes it a *resume*
/// rather than a fresh start: the run that just finished is settled first, so the id it wrote is
/// the one continued.
fn resume_agent(place: &Place, agent: &AgentSpec, passthrough: &[String]) -> Result<()> {
    let plugins = plugins()?;
    let mut place = place.clone();
    toren_lib::sessions::settle_saved(&mut place, &plugins);

    let session_id = toren_lib::sessions::resume_target(&place, &plugins, &agent.name, None);
    let mut argv = agent.resume_argv_for(&plugins, session_id.as_deref(), None, false)?;
    argv.extend(passthrough.iter().cloned());

    toren_lib::sessions::record_start(&mut place, &plugins, &agent.name, session_id.as_deref())?;
    toren_lib::rmux::spawn_agent(&place.session_name(), &place.path, &argv)
}

// ─── shell ──────────────────────────────────────────────────────────────────

struct ShellArgs {
    workspace: Option<String>,
    hook: Option<HookArg>,
    /// An existing window to mirror, rather than a shell of one's own.
    window: Option<String>,
    segment: Option<String>,
    no_rmux: bool,
    /// `None` — decide by context; `Some` — what the flags said (D18).
    hold: Option<bool>,
    cmd: Vec<String>,
}

fn cmd_shell(config: &Config, args: ShellArgs) -> Result<()> {
    let registry = PlaceRegistry::new(config)?;
    let segment_name = args.segment.as_deref();

    if let Some(hook_type) = args.hook {
        return run_hook(&registry, hook_type);
    }

    // No workspace and no command: make one and drop into it.
    if args.workspace.is_none()
        && args.cmd.is_empty()
        && args.window.is_none()
        && registry.resolve_from_env().is_none()
    {
        let segment = registry.segment(segment_name)?;
        let place = registry.create(&segment, None, None, config.ancillaries.max_per_segment)?;
        eprintln!("Created workspace: {}", place.path.display());
        println!("{}", place.path.display());
        return launch_shell(&place, args.no_rmux, args.hold);
    }

    let place = require_place(&registry, args.workspace.as_deref(), segment_name)?;

    if let Some(window) = args.window {
        return watch_window(&place, &window, args.hold);
    }

    if args.cmd.is_empty() {
        println!("{}", place.path.display());
        return launch_shell(&place, args.no_rmux, args.hold);
    }

    launch_command(&place, &args.cmd, args.no_rmux, args.hold)
}

/// `--hold` / `--no-hold` as the overriding pair D18 describes: `None` is "decide by context",
/// which is what makes the context-sensitive default expressible instead of hidden.
fn resolve_hold(hold: bool, no_hold: bool) -> Option<bool> {
    match (hold, no_hold) {
        (true, false) => Some(true),
        (false, true) => Some(false),
        _ => None,
    }
}

fn run_hook(registry: &PlaceRegistry, hook: HookArg) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let place = registry
        .resolve_from_path(&cwd)
        .context("Not inside a breq-managed workspace")?;
    let num = toren_lib::word_to_number(&place.name).unwrap_or(0);

    match hook {
        HookArg::Setup => {
            eprintln!("Running setup for '{}'", place.name);
            registry.workspaces.run_setup(
                &place.segment_path,
                &place.path,
                &place.name,
                num,
                None,
            )?;
        }
        HookArg::Destroy => {
            eprintln!("Running destroy for '{}'", place.name);
            registry
                .workspaces
                .run_destroy(&place.segment_path, &place.path, &place.name)?;
        }
    }
    eprintln!("Done.");
    Ok(())
}

/// Open a fresh shell window in the workspace's session and mirror it.
///
/// Every invocation is its own shell: two terminals running `breq sh` on one workspace are two
/// shells side by side, not two mirrors of one pane. The shell still lives in the session, so it
/// sits alongside the agent and shows up in the browser's window list.
///
/// This is the "feels exactly like `zsh`" case: no chrome, and `exit` closes the pane and returns
/// you to the shell you came from. `--hold` opts into keeping the finished pane, which is only
/// worth doing when you want to see how the shell ended.
fn launch_shell(place: &Place, no_rmux: bool, hold: Option<bool>) -> Result<()> {
    if !no_rmux && toren_lib::rmux::is_available() && mirror::owns_terminal() {
        let session = place.session_name();
        toren_lib::rmux::reconcile(&place.segment, &place.name, place.uid().as_deref());
        toren_lib::rmux::ensure_session(&session, &place.path, &place.env())?;
        let window = toren_lib::rmux::open_shell(&session, &place.path)?;

        let hold = hold.unwrap_or(false);
        toren_lib::rmux::set_hold(&session, &window, hold)?;

        // A re-run restarts a shell in the same window rather than opening yet another one.
        let rerun_window = window.clone();
        let rerun: mirror::Rerun = Box::new(move |place: &Place| {
            let session = place.session_name();
            toren_lib::rmux::respawn_shell(&session, &rerun_window, &place.path)?;
            toren_lib::rmux::set_hold(&session, &rerun_window, hold)
        });
        let code = mirror::run(
            place,
            mirror::Pane {
                window,
                role: PaneRole::Shell,
                hold,
            },
            rerun,
        )?;
        std::process::exit(code);
    }

    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
    let mut cmd = Command::new(&shell);
    cmd.current_dir(&place.path);
    for (key, value) in place.env() {
        cmd.env(key, value);
    }
    let err = cmd.exec();
    Err(err).context("Failed to exec shell")
}

/// Mirror a window of the workspace's session that something else created.
///
/// How a running agent is reached from a terminal: `breq do` mirrors the agent it spawns, and this
/// mirrors the one already there — started from another terminal, or from the browser. The hold
/// policy is read from the window rather than guessed from its name, because it was decided when
/// the window was created and nothing here is entitled to a second opinion.
fn watch_window(place: &Place, window: &str, hold: Option<bool>) -> Result<()> {
    let session = place.session_name();
    if !toren_lib::rmux::is_available() || !toren_lib::rmux::window_exists(&session, window) {
        let open = toren_lib::rmux::list_windows(&session).unwrap_or_default();
        anyhow::bail!(
            "Workspace '{}' has no '{}' window running.{}",
            place.name,
            window,
            match open.is_empty() {
                true => String::new(),
                false => format!("\n  Open windows: {}", open.join(", ")),
            }
        );
    }
    if !mirror::owns_terminal() {
        anyhow::bail!("There is no terminal here to mirror '{}' in", window);
    }

    let role = match window == toren_lib::rmux::AGENT_WINDOW {
        true => PaneRole::Agent,
        false => PaneRole::Shell,
    };
    let rerun: mirror::Rerun = match place.agent().filter(|_| role == PaneRole::Agent) {
        // A held agent pane knows which session ran in it, so `<ENTER>` continues that one.
        Some(agent) => Box::new(move |place: &Place| resume_agent(place, &agent, &[])),
        // Anything else: rmux remembers what the pane was created with, which is the only record
        // of it that exists — breq did not spawn this window and has no argv of its own.
        None => {
            let window = window.to_string();
            Box::new(move |place: &Place| {
                toren_lib::rmux::respawn_window(&place.session_name(), &window, &place.path)
            })
        }
    };

    let code = mirror::run(
        place,
        mirror::Pane {
            window: window.to_string(),
            role,
            hold: hold.unwrap_or_else(|| toren_lib::rmux::holds(&session, window)),
        },
        rerun,
    )?;
    std::process::exit(code);
}

/// Run a command in the workspace, either as a pane of its own or as a direct child.
///
/// A command that finished is worth keeping on screen until it is dismissed, so with a terminal
/// to draw in it gets a held pane of its own (D10). `--no-hold` — and anything without a terminal,
/// which is every pipeline — runs it as a direct child instead, so `breq sh <ws> -- <cmd>` still
/// composes: real stdout, real exit code, nothing to dismiss.
fn launch_command(place: &Place, cmd: &[String], no_rmux: bool, hold: Option<bool>) -> Result<()> {
    let mirrored = !no_rmux
        && hold != Some(false)
        && toren_lib::rmux::is_available()
        && mirror::owns_terminal();

    if mirrored {
        let session = place.session_name();
        toren_lib::rmux::reconcile(&place.segment, &place.name, place.uid().as_deref());
        toren_lib::rmux::ensure_session(&session, &place.path, &place.env())?;
        let window = toren_lib::rmux::spawn_command(&session, &place.path, cmd, true)?;

        // The re-run mints a new pane rather than respawning this one, so a browser mirroring the
        // window is handed over to it — and the fresh window is told to hold, which it would
        // otherwise inherit from the session as `off`.
        let rerun: mirror::Rerun = {
            let window = window.clone();
            let cmd = cmd.to_vec();
            Box::new(move |place: &Place| {
                let session = place.session_name();
                toren_lib::rmux::run_in_window(&session, &window, &place.path, &cmd)?;
                toren_lib::rmux::set_hold(&session, &window, true)
            })
        };
        let code = mirror::run(
            place,
            mirror::Pane {
                window,
                role: PaneRole::Shell,
                hold: true,
            },
            rerun,
        )?;
        std::process::exit(code);
    }

    let (program, args) = (cmd[0].clone(), cmd[1..].to_vec());
    let mut command = Command::new(&program);
    command.args(&args).current_dir(&place.path);
    for (key, value) in place.env() {
        command.env(key, value);
    }
    let err = command.exec();
    Err(err).with_context(|| format!("Failed to exec: {}", program))
}

// ─── setup ──────────────────────────────────────────────────────────────────

fn cmd_setup(
    config: &Config,
    workspace: Option<String>,
    from: Option<String>,
    segment_name: Option<&str>,
) -> Result<()> {
    let registry = PlaceRegistry::new(config)?;
    let segment = registry.segment(segment_name)?;

    let parent = match from {
        Some(name) => Some(registry.require(&segment, &name)?),
        None => None,
    };

    // An existing working copy is adopted in place rather than recreated — that's how a
    // hand-made worktree, or one that outlived its state, becomes a place breq manages.
    if let Some(ref name) = workspace {
        let mut place = registry.get(&segment, name);
        if place.exists() {
            if place.is_decorated() {
                // Setup hooks are not generally idempotent (a `copy` onto an existing
                // directory fails), so an already-managed workspace is left alone. Re-run
                // the hooks deliberately with `breq sh <ws> --hook setup`.
                eprintln!("Workspace '{}' already exists.", place.name);
            } else {
                registry.adopt(&mut place)?;
                eprintln!(
                    "Adopted existing working copy '{}' ({})",
                    place.name,
                    place.uid().unwrap_or_default()
                );
            }
            println!("{}", place.path.display());
            return Ok(());
        }
    }

    let place = registry.create(
        &segment,
        workspace.as_deref(),
        parent.as_ref(),
        config.ancillaries.max_per_segment,
    )?;

    match parent {
        Some(parent) => eprintln!(
            "Created workspace '{}' stacked on '{}': {}",
            place.name,
            parent.name,
            place.path.display()
        ),
        None => eprintln!(
            "Created workspace '{}': {}",
            place.name,
            place.path.display()
        ),
    }
    println!("{}", place.path.display());
    Ok(())
}

// ─── teardown ───────────────────────────────────────────────────────────────

fn cmd_teardown(
    config: &Config,
    workspace: &str,
    kill: bool,
    no_delete: bool,
    segment_name: Option<&str>,
) -> Result<()> {
    let registry = PlaceRegistry::new(config)?;
    let segment = registry.segment(segment_name)?;
    let place = registry.get(&segment, workspace);

    if !place.exists() && !place.vcs_tracked {
        anyhow::bail!(
            "Workspace '{}' not found at {}",
            place.name,
            place.path.display()
        );
    }

    // Children stacked on this workspace lose their base; say so rather than surprising them.
    let children: Vec<String> = registry
        .list(&segment)
        .into_iter()
        .filter(|p| p.parent().as_deref() == Some(place.name.as_str()))
        .map(|p| p.name)
        .collect();
    if !children.is_empty() {
        eprintln!(
            "note: '{}' is the stack parent of {} — their commits survive, but they no longer \
             have a live parent workspace",
            place.name,
            children.join(", ")
        );
    }

    eprintln!("Tearing down '{}' ({})", place.name, place.path.display());

    let outcome = toren_lib::teardown(
        &place,
        &registry.workspaces,
        &plugins()?,
        toren_lib::TeardownOptions { kill, no_delete },
    )?;

    println!("{}", serde_json::to_string(&outcome)?);
    Ok(())
}

// ─── list ───────────────────────────────────────────────────────────────────

fn cmd_list(
    config: &Config,
    all_segments: bool,
    segment_name: Option<&str>,
    refresh: bool,
    local: bool,
) -> Result<()> {
    let registry = PlaceRegistry::new(config)?;
    let plugins = plugins()?;

    let (places, scope) = if all_segments {
        (registry.list_all(), "all segments".to_string())
    } else {
        let segment = registry.segment(segment_name)?;
        (registry.list(&segment), segment.name.clone())
    };

    if places.is_empty() {
        println!("No workspaces in {}.", scope);
        if !all_segments {
            println!("Create one with `breq setup`, or use --all to see every segment.");
        }
        return Ok(());
    }

    // The one command that never writes: `list` renders the cache, and `--refresh` is the
    // explicit act of paying for the round trips — for this render only, since a read across
    // every workspace has no business rewriting every workspace's cache.
    let opts = if local {
        CollectOptions::local()
    } else {
        CollectOptions::cached().with_refresh(refresh).read_only()
    };

    let rows: Vec<(Place, Sets)> = places
        .into_iter()
        .map(|place| {
            let sets = Sets::collect(&place, &registry.workspaces, &plugins, config, opts);
            (place, sets)
        })
        .collect();

    render::list(&rows, &plugins, all_segments);
    Ok(())
}

// ─── get ────────────────────────────────────────────────────────────────────

fn cmd_get(
    config: &Config,
    args: Vec<String>,
    task: Option<String>,
    refresh: bool,
    json: bool,
    segment_name: Option<&str>,
) -> Result<()> {
    let registry = PlaceRegistry::new(config)?;
    let plugins = plugins()?;
    let (mut place, key) = split_place_args(&registry, args, segment_name, 0)?;

    if let Some(key) = key {
        return get_key(
            config,
            &registry,
            &plugins,
            &place,
            &key,
            task.as_deref(),
            refresh,
        );
    }

    // Looking at one workspace is when a finished agent session gets its ending written down;
    // nothing else is watching the pane for it.
    toren_lib::sessions::settle_saved(&mut place, &plugins);

    // Rendering one workspace already pays for the calls, so it refreshes the cache on the way
    // past — which is what keeps `breq list` current for the workspaces you actually work in.
    let sets = Sets::collect(
        &place,
        &registry.workspaces,
        &plugins,
        config,
        CollectOptions::live(),
    );

    if json {
        println!("{}", render::detail_json(&place, &sets, &plugins)?);
    } else {
        render::detail(&place, &sets, &plugins);
    }
    Ok(())
}

/// A single value, for scripting.
fn get_key(
    config: &Config,
    registry: &PlaceRegistry,
    plugins: &PluginManager,
    place: &Place,
    key: &str,
    task_filter: Option<&str>,
    refresh: bool,
) -> Result<()> {
    // Task-source-owned fields are asked of the source every time. Breq holding a copy is
    // exactly how a workspace ends up claiming a status the tracker disagrees with.
    if let Some(field) = key.strip_prefix("task.") {
        for link in filtered_tasks(place, task_filter)? {
            let (source, id) = toren_lib::split_link(&link)
                .with_context(|| format!("Malformed task link '{}'", link))?;
            let task = plugins.resolve_info(&source, &id, plugin_ctx(place))?;
            toren_lib::sets::cache_task(place, &task);
            let value = match field {
                "id" => Some(task.id),
                "title" => Some(task.title),
                "status" => task.status,
                "assignee" => task.assignee,
                "url" => task.url,
                "kind" => task.kind,
                "description" => task.description,
                "source" => Some(task.source),
                other => anyhow::bail!(
                    "Unknown task field '{}'. Known: id, title, status, assignee, url, kind, \
                     description, source",
                    other
                ),
            };
            println!("{}", value.unwrap_or_default());
        }
        return Ok(());
    }

    // Derived and core workspace fields.
    match key {
        "workspace.path" | "path" => {
            println!("{}", place.path.display());
            return Ok(());
        }
        "session" => {
            println!("{}", place.session_name());
            return Ok(());
        }
        "changes" => {
            let sets = Sets::collect(
                place,
                &registry.workspaces,
                plugins,
                config,
                CollectOptions::local(),
            );
            for commit in &sets.changes {
                println!("{} {}", commit.id, commit.summary);
            }
            return Ok(());
        }
        "branches" => {
            for branch in registry
                .workspaces
                .remote_branches(&place.segment_path, &place.path)
            {
                println!("{}", branch);
            }
            return Ok(());
        }
        "prs" => {
            let sets = Sets::collect(
                place,
                &registry.workspaces,
                plugins,
                config,
                CollectOptions {
                    tasks: false,
                    ..CollectOptions::cached().with_refresh(refresh)
                },
            );
            for pr in &sets.prs {
                println!("{} {} {} {}", pr.id, pr.state, pr.ci, pr.url);
            }
            return Ok(());
        }
        _ => {}
    }

    // Cached reads are asked for by name. They used to answer as a silent fallback for any key
    // stored state did not know, which made durable and disposable data indistinguishable at the
    // call site — `cache.` is the caller saying which of the two they meant.
    if let Some(cached) = key.strip_prefix(toren_lib::state::CACHE_NAMESPACE) {
        if let Some(entry) = place.cache().get(cached) {
            println!("{}", entry.value);
            eprintln!("(cached {})", entry.age_label());
        }
        return Ok(());
    }

    // Stored state. List-valued fields print one per line so `for x in $(breq get ...)`
    // does the obvious thing.
    if let Some(values) = place.state.get_field(key) {
        for value in values {
            println!("{}", value);
        }
    }
    Ok(())
}

fn filtered_tasks(place: &Place, task_filter: Option<&str>) -> Result<Vec<String>> {
    let links = place.tasks();
    if links.is_empty() {
        anyhow::bail!(
            "No tasks linked to '{}'. Attach one with: breq set {} +task <source>:<id>",
            place.name,
            place.name
        );
    }
    let Some(filter) = task_filter else {
        return Ok(links);
    };
    let matched: Vec<String> = links
        .into_iter()
        .filter(|l| l == filter || l.ends_with(&format!(":{}", filter)))
        .collect();
    if matched.is_empty() {
        anyhow::bail!(
            "No task matching '{}' is linked to '{}'",
            filter,
            place.name
        );
    }
    Ok(matched)
}

// ─── set ────────────────────────────────────────────────────────────────────

fn cmd_set(
    config: &Config,
    args: Vec<String>,
    task: Option<String>,
    segment_name: Option<&str>,
) -> Result<()> {
    let registry = PlaceRegistry::new(config)?;
    let plugins = plugins()?;
    let (mut place, rest) = split_place_args_multi(&registry, args, segment_name, 2)?;

    let key = rest[0].clone();
    let value = rest[1].clone();

    // Writing a task-source-owned field *is* the tracker update. No local copy exists to
    // fall out of sync.
    if let Some(field) = key.strip_prefix("task.") {
        for link in filtered_tasks(&place, task.as_deref())? {
            let (source, id) = toren_lib::split_link(&link)
                .with_context(|| format!("Malformed task link '{}'", link))?;
            plugins.resolve_set_field(&source, &id, field, &value, plugin_ctx(&place))?;
            eprintln!("{} {} = {}", link, field, value);
        }
        return Ok(());
    }

    // `+key value` / `-key value` mutate a list without rewriting it.
    if let Some(list_key) = key.strip_prefix('+') {
        if place.state.add_to_field(list_key, &value)? {
            place.save()?;
            eprintln!("{}: +{} {}", place.name, list_key, value);
            // The link itself is local knowledge; its title and status live in the tracker, and
            // this is the only moment anything asks. Failing to reach the tracker leaves the
            // link — a workspace that knows what it is for beats one that knows nothing.
            if matches!(list_key, "task" | "tasks") {
                if let Err(e) = toren_lib::sets::refresh_task(&place, &plugins, &value) {
                    eprintln!("warning: could not read {}: {:#}", value, e);
                }
            }
        }
        return Ok(());
    }
    if let Some(list_key) = key.strip_prefix('-') {
        if place.state.remove_from_field(list_key, &value)? {
            place.save()?;
            eprintln!("{}: -{} {}", place.name, list_key, value);
        }
        return Ok(());
    }

    place
        .state
        .set_field(&key, toren_lib::state::parse_value(&value))?;
    place.save()?;
    eprintln!("{}: {} = {}", place.name, key, value);
    Ok(())
}

/// Split `[workspace] [key]` — with one argument, a known workspace wins over a key.
fn split_place_args(
    registry: &PlaceRegistry,
    args: Vec<String>,
    segment_name: Option<&str>,
    trailing: usize,
) -> Result<(Place, Option<String>)> {
    let (place, rest) = split_place_args_multi(registry, args, segment_name, trailing)?;
    Ok((place, rest.into_iter().next()))
}

/// Split `[workspace] <trailing...>`.
fn split_place_args_multi(
    registry: &PlaceRegistry,
    args: Vec<String>,
    segment_name: Option<&str>,
    trailing: usize,
) -> Result<(Place, Vec<String>)> {
    // More arguments than the trailing form takes means the first one names a workspace.
    if args.len() > trailing {
        let name = args[0].clone();
        let segment = registry.segment(segment_name)?;
        let place = registry.get(&segment, &name);
        if place.exists() {
            return Ok((place, args[1..].to_vec()));
        }
        // Unambiguously meant as a workspace, and it isn't one.
        if trailing > 0 || args.len() > trailing + 1 {
            anyhow::bail!("Workspace '{}' not found at {}", name, place.path.display());
        }
        // Otherwise it was the optional key, and the workspace comes from where we stand.
    }

    let place = require_place(registry, None, segment_name)?;
    Ok((place, args))
}

// ─── cleanup ────────────────────────────────────────────────────────────────

fn cmd_cleanup(config: &Config, all_segments: bool, segment_name: Option<&str>) -> Result<()> {
    let registry = PlaceRegistry::new(config)?;

    let segments: Vec<Segment> = if all_segments {
        registry.segments.list_all()
    } else {
        vec![registry.segment(segment_name)?]
    };

    let mut removed = 0;
    for segment in &segments {
        for place in registry.list(segment) {
            // A directory the VCS has forgotten is a leftover: no history can be lost.
            if place.exists() && !place.vcs_tracked && !place.is_decorated() {
                print!("  {}/{}...", segment.name, place.name);
                match std::fs::remove_dir_all(&place.path) {
                    Ok(()) => {
                        println!(" removed");
                        removed += 1;
                    }
                    Err(e) => println!(" failed: {}", e),
                }
            }
        }
    }

    if removed == 0 {
        println!("No orphaned workspace directories found.");
    }

    Ok(())
}

// ─── doctor ─────────────────────────────────────────────────────────────────

fn cmd_doctor(config: &Config, fix: bool) -> Result<()> {
    let plugins = plugins()?;
    let reports = toren_lib::doctor::run(config, &plugins, fix)?;

    let mut clean = true;
    for report in &reports {
        if report.is_clean() && report.fixed.is_empty() {
            println!("{} {}", "ok".green(), report.name);
            continue;
        }
        clean = false;
        println!("{} {}", "!!".yellow(), report.name);
        for finding in &report.findings {
            println!("   - {}", finding);
        }
        for fixed in &report.fixed {
            println!("   {} {}", "→".green(), fixed);
        }
        if let Some(advice) = &report.advice {
            println!("   {}", advice.dimmed());
        }
    }

    if clean {
        println!("\nNothing to fix.");
    }
    Ok(())
}

// ─── init ───────────────────────────────────────────────────────────────────

fn cmd_init(stealth: bool) -> Result<()> {
    let cwd = std::env::current_dir()?;

    let has_jj = cwd.join(".jj").exists();
    let has_git = cwd.join(".git").exists();

    if !has_jj && !has_git {
        anyhow::bail!(
            "Not a version-controlled repository. breq init must be run from a jj or git repo root."
        );
    }

    ensure_repo_root(&cwd, has_jj)?;

    let config_path = cwd.join("toren.kdl");
    if config_path.exists() || cwd.join(".toren.kdl").exists() {
        anyhow::bail!("toren.kdl already exists. Remove it first to re-initialize.");
    }

    let (share_entries, copy_entries) = discover_setup_entries(&cwd)?;

    let mut kdl = String::from("// var subdomain=\"{{ ws.name }}.{{ repo.name }}\"\n\nsetup {\n");
    for entry in &share_entries {
        kdl.push_str(&format!("    share src=\"{}\"\n", entry));
    }
    for entry in &copy_entries {
        kdl.push_str(&format!("    copy src=\"{}\"\n", entry));
    }
    kdl.push_str("}\n\n");
    kdl.push_str(
        "// Runs instead of `setup` for a workspace created with `breq setup --from <ws>`.\n\
         // {{ parent.path }} is the workspace being forked, so runtime state can be cloned\n\
         // rather than rebuilt:\n\
         // fork {\n\
         //     copy src=\"data\" from=\"{{ parent.path }}\"\n\
         // }\n\n",
    );
    kdl.push_str("destroy { }\n");

    std::fs::write(&config_path, &kdl).context("Failed to write toren.kdl")?;
    println!(
        "Created {} with {} setup entries",
        toren_lib::tilde_shorten(&config_path),
        share_entries.len() + copy_entries.len()
    );
    for entry in share_entries.iter().chain(copy_entries.iter()) {
        println!("  {}", entry);
    }

    if stealth {
        add_to_git_exclude(&cwd, "toren.kdl")?;
    }

    install_stack_scripts(&cwd)?;

    if std::io::stdin().is_terminal() {
        offer_segment_registration(&cwd)?;
    }

    Ok(())
}

/// Install the shipped workflow scripts, plus any that fit the detected stack.
///
/// Out-of-box feel comes from here: a github + task-resolver repo gets a working
/// `breq submit` without anyone writing one.
fn install_stack_scripts(repo: &Path) -> Result<()> {
    let mut installed = Vec::new();

    for name in toren_lib::scripts::missing(true) {
        if let Some(path) = toren_lib::scripts::install(name)? {
            installed.push(path);
        }
    }

    if detects_github(repo) {
        if let Some(path) = toren_lib::scripts::install("breq-submit")? {
            installed.push(path);
        }
    }

    if !installed.is_empty() {
        println!("\nInstalled workflow scripts:");
        for path in &installed {
            println!("  {}", toren_lib::tilde_shorten(path));
        }
        println!("  (edit them — they're yours; `breq <name>` finds them by name)");
    }
    Ok(())
}

fn detects_github(repo: &Path) -> bool {
    let remotes = Command::new("git")
        .args(["remote", "-v"])
        .current_dir(repo)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();
    remotes.contains("github.com") && which::which("gh").is_ok()
}

fn ensure_repo_root(cwd: &Path, has_jj: bool) -> Result<()> {
    let (program, args) = if has_jj {
        ("jj", vec!["workspace", "root"])
    } else {
        ("git", vec!["rev-parse", "--show-toplevel"])
    };

    let output = Command::new(program)
        .args(&args)
        .current_dir(cwd)
        .output()
        .with_context(|| format!("Failed to run {}", program))?;

    if !output.status.success() {
        anyhow::bail!("Failed to determine repository root");
    }

    let root = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim().to_string());
    if root != cwd {
        anyhow::bail!(
            "breq init must be run from the repo root: {}",
            root.display()
        );
    }
    Ok(())
}

/// Guess what a workspace needs copied or shared: build artifacts and untracked local state.
fn discover_setup_entries(cwd: &Path) -> Result<(Vec<String>, Vec<String>)> {
    let mut copy_entries: Vec<String> = Vec::new();
    let mut share_entries: Vec<String> = Vec::new();

    if cwd.join(".beads").exists() {
        let is_tracked = Command::new("git")
            .args(["ls-files", "--error-unmatch", ".beads"])
            .current_dir(cwd)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !is_tracked {
            share_entries.push(".beads".to_string());
        }
    }

    let well_known = [
        "target",
        "node_modules",
        "dist",
        "build",
        ".next",
        ".nuxt",
        ".output",
        ".svelte-kit",
        "vendor",
        "__pycache__",
    ];

    let gitignore_path = cwd.join(".gitignore");
    if gitignore_path.exists() {
        let gitignore = std::fs::read_to_string(&gitignore_path)?;
        for line in gitignore.lines() {
            let line = line.trim().trim_end_matches('/');
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            for artifact in &well_known {
                if (line == *artifact || line.ends_with(&format!("/{}", artifact)))
                    && cwd.join(line).is_dir()
                    && !copy_entries.contains(&line.to_string())
                {
                    copy_entries.push(line.to_string());
                }
            }
        }
    }

    for artifact in &well_known {
        if cwd.join(artifact).is_dir() && !copy_entries.contains(&artifact.to_string()) {
            copy_entries.push(artifact.to_string());
        }
    }

    Ok((share_entries, copy_entries))
}

fn add_to_git_exclude(cwd: &Path, entry: &str) -> Result<()> {
    let git_info_dir = cwd.join(".git").join("info");
    if !git_info_dir.exists() {
        println!("Warning: .git/info directory not found, --stealth had no effect");
        return Ok(());
    }

    let exclude_path = git_info_dir.join("exclude");
    let existing = std::fs::read_to_string(&exclude_path).unwrap_or_default();
    if existing.lines().any(|l| l.trim() == entry) {
        return Ok(());
    }

    let mut content = existing;
    if !content.ends_with('\n') && !content.is_empty() {
        content.push('\n');
    }
    content.push_str(entry);
    content.push('\n');
    std::fs::write(&exclude_path, content).context("Failed to update .git/info/exclude")?;
    println!("Added {} to .git/info/exclude", entry);
    Ok(())
}

fn offer_segment_registration(cwd: &Path) -> Result<()> {
    let Ok(config) = Config::load() else {
        return Ok(());
    };
    let segment_mgr = toren_lib::SegmentManager::new(&config)?;
    if segment_mgr.resolve_from_path(cwd).is_some() {
        return Ok(());
    }

    let repo_path = toren_lib::tilde_shorten(cwd);
    let parent_glob = cwd
        .parent()
        .map(|p| format!("{}/*", toren_lib::tilde_shorten(p)));

    let entry = if let Some(glob) = parent_glob {
        eprintln!("\nThis repo isn't covered by a segment in ~/.toren/config.kdl.");
        eprint!("Add parent glob '{}'? [Y/n] ", glob);
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        let ans = input.trim().to_ascii_lowercase();
        if ans.is_empty() || ans == "y" || ans == "yes" {
            glob
        } else {
            repo_path
        }
    } else {
        repo_path
    };

    add_segment_to_config(&toren_lib::default_config_path(), &entry)
}

/// Add a segment entry to ~/.toren/config.kdl, preserving comments.
fn add_segment_to_config(config_path: &Path, entry: &str) -> Result<()> {
    use kdl::{KdlDocument, KdlNode};

    let mut doc = if config_path.exists() {
        std::fs::read_to_string(config_path)?
            .parse::<KdlDocument>()
            .with_context(|| format!("Failed to parse {}", config_path.display()))?
    } else {
        KdlDocument::new()
    };

    if doc.get("ancillaries").is_none() {
        doc.nodes_mut().push(KdlNode::new("ancillaries"));
    }
    let ancillaries = doc
        .get_mut("ancillaries")
        .expect("just ensured")
        .ensure_children();

    match ancillaries.get_mut("segments") {
        Some(segments) => {
            if segments
                .entries()
                .iter()
                .any(|e| e.value().as_string() == Some(entry))
            {
                println!("'{}' already in config", entry);
                return Ok(());
            }
            segments.push(entry);
        }
        None => {
            let mut segments = KdlNode::new("segments");
            segments.push(entry);
            ancillaries.nodes_mut().push(segments);
        }
    }
    doc.autoformat();

    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    toren_lib::fsutil::write_atomic(config_path, doc.to_string())
        .context("Failed to write config file")?;
    println!("Added '{}' to ~/.toren/config.kdl", entry);
    Ok(())
}

// ─── plugin ─────────────────────────────────────────────────────────────────

const PLUGIN_REPO_RAW: &str =
    "https://raw.githubusercontent.com/anowell/toren/main/contrib/plugins";
const PLUGIN_REPO_API: &str = "https://api.github.com/repos/anowell/toren/contents/contrib/plugins";

fn cmd_plugin(cmd: PluginCmd) -> Result<()> {
    match cmd {
        PluginCmd::List => cmd_plugin_list(),
        PluginCmd::Install { target } => cmd_plugin_install(&target),
    }
}

fn cmd_plugin_list() -> Result<()> {
    let installed_root = toren_lib::toren_root().join("plugins");
    let mgr = PluginManager::new(&installed_root)?;

    for family in Family::all() {
        let remote = fetch_remote_plugin_names(family.dir()).unwrap_or_default();
        let local: Vec<&str> = mgr.list(*family);

        let mut all: Vec<&str> = remote
            .iter()
            .map(|s| s.as_str())
            .chain(local.iter().copied())
            .collect();
        all.sort();
        all.dedup();

        println!("{}/", family.dir());
        if all.is_empty() {
            println!("  (none)");
            continue;
        }
        let width = all.iter().map(|n| n.len()).max().unwrap_or(0);
        for name in all {
            let meta = mgr.get_meta(*family, name);
            let marker = match (meta, remote.iter().any(|n| n == name)) {
                (Some(m), _) if m.is_builtin() => "built-in",
                (Some(_), _) => "installed",
                (None, true) => "available",
                (None, false) => "unknown",
            };
            println!("  {:<width$}  {}", name, marker, width = width);
        }
    }
    Ok(())
}

fn cmd_plugin_install(target: &str) -> Result<()> {
    let installed_root = toren_lib::toren_root().join("plugins");

    let local_path = PathBuf::from(target);
    if local_path.is_file() {
        install_from_local(&local_path, &installed_root)
    } else {
        install_from_remote(target, &installed_root)
    }
}

fn install_from_local(src: &Path, installed_root: &Path) -> Result<()> {
    if src.extension().and_then(|s| s.to_str()) != Some("rhai") {
        anyhow::bail!("Local plugin must be a .rhai file: {}", src.display());
    }

    let family = src
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .and_then(Family::parse)
        .with_context(|| {
            format!(
                "Cannot infer plugin family from path: {} (parent dir must be tasks, agents, \
                 or delivery)",
                src.display()
            )
        })?;

    let file_name = src
        .file_name()
        .with_context(|| format!("Invalid plugin path: {}", src.display()))?;

    let dest_dir = installed_root.join(family.dir());
    std::fs::create_dir_all(&dest_dir)?;
    let dest = dest_dir.join(file_name);
    std::fs::copy(src, &dest)?;

    println!(
        "Installed {} -> {}",
        src.display(),
        toren_lib::tilde_shorten(&dest)
    );
    Ok(())
}

fn install_from_remote(target: &str, installed_root: &Path) -> Result<()> {
    let (family, name) = parse_remote_target(target)?;

    let url = format!("{}/{}/{}.rhai", PLUGIN_REPO_RAW, family.dir(), name);
    let agent = http_agent();

    let response = agent
        .get(&url)
        .call()
        .with_context(|| format!("Failed to fetch {}", url))?;

    let status: u16 = response.status().into();
    if status == 404 {
        anyhow::bail!(
            "Plugin '{}/{}' not found in contrib repo.\n\
             Run `breq plugin list` to see what's available.",
            family.dir(),
            name
        );
    }
    if !(200..300).contains(&status) {
        anyhow::bail!("Failed to fetch {} (HTTP {})", url, status);
    }

    let body = response.into_body().read_to_string()?;

    let dest_dir = installed_root.join(family.dir());
    std::fs::create_dir_all(&dest_dir)?;
    let dest = dest_dir.join(format!("{}.rhai", name));
    std::fs::write(&dest, body)?;

    println!(
        "Installed {}/{} -> {}",
        family.dir(),
        name,
        toren_lib::tilde_shorten(&dest)
    );
    Ok(())
}

fn parse_remote_target(target: &str) -> Result<(Family, String)> {
    let (family, name) = target.split_once('/').with_context(|| {
        format!(
            "Plugin target '{}' must be '<family>/<name>' (tasks, agents, delivery) or a local \
             .rhai path",
            target
        )
    })?;

    let family = Family::parse(family).with_context(|| {
        format!(
            "Unknown plugin family '{}' — must be tasks, agents, or delivery",
            family
        )
    })?;

    if name.is_empty() || name.contains('/') || name.contains("..") {
        anyhow::bail!("Invalid plugin name: '{}'", name);
    }

    Ok((
        family,
        name.strip_suffix(".rhai").unwrap_or(name).to_string(),
    ))
}

fn fetch_remote_plugin_names(family: &str) -> Result<Vec<String>> {
    let url = format!("{}/{}", PLUGIN_REPO_API, family);
    let response = http_agent()
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "breq-plugin-list")
        .call()
        .with_context(|| format!("Failed to fetch {}", url))?;

    let status: u16 = response.status().into();
    if !(200..300).contains(&status) {
        anyhow::bail!("GET {} returned HTTP {}", url, status);
    }

    let body = response.into_body().read_to_string()?;
    let entries: Vec<serde_json::Value> = serde_json::from_str(&body)?;

    let mut names: Vec<String> = entries
        .iter()
        .filter(|e| e.get("type").and_then(|v| v.as_str()) == Some("file"))
        .filter_map(|e| e.get("name").and_then(|v| v.as_str()))
        .filter_map(|n| n.strip_suffix(".rhai"))
        .map(String::from)
        .collect();
    names.sort();
    Ok(names)
}

fn http_agent() -> ureq::Agent {
    ureq::Agent::new_with_config(
        ureq::config::Config::builder()
            .timeout_global(Some(std::time::Duration::from_secs(30)))
            .http_status_as_error(false)
            .build(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hold_of(args: &[&str]) -> Option<bool> {
        match Cli::try_parse_from(args).expect("parses").command {
            Commands::Shell { hold, no_hold, .. } => resolve_hold(hold, no_hold),
            _ => panic!("expected `sh`"),
        }
    }

    #[test]
    fn hold_is_left_to_context_until_it_is_asked_for() {
        assert_eq!(hold_of(&["breq", "sh", "one"]), None);
        assert_eq!(hold_of(&["breq", "sh", "one", "--", "make", "test"]), None);
    }

    #[test]
    fn an_explicit_hold_flag_wins_in_both_directions() {
        assert_eq!(hold_of(&["breq", "sh", "one", "--hold"]), Some(true));
        assert_eq!(
            hold_of(&["breq", "sh", "one", "--no-hold", "--", "make", "test"]),
            Some(false)
        );
    }

    #[test]
    fn the_last_hold_flag_wins_rather_than_the_parse_failing() {
        assert_eq!(
            hold_of(&["breq", "sh", "one", "--hold", "--no-hold"]),
            Some(false)
        );
        assert_eq!(
            hold_of(&["breq", "sh", "one", "--no-hold", "--hold"]),
            Some(true)
        );
    }

    #[test]
    fn a_command_still_reaches_the_workspace_whole() {
        let Commands::Shell { workspace, cmd, .. } =
            Cli::try_parse_from(["breq", "sh", "one", "--", "grep", "-rn", "--color", "x"])
                .expect("parses")
                .command
        else {
            panic!("expected `sh`");
        };
        assert_eq!(workspace.as_deref(), Some("one"));
        assert_eq!(cmd, vec!["grep", "-rn", "--color", "x"]);
    }

    #[test]
    fn adding_a_segment_keeps_the_rest_of_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.kdl");
        std::fs::write(
            &config,
            "// hand-written\nserver {\n    port 8788\n}\n\nancillaries {\n    segments \"~/proj/*\"\n}\n",
        )
        .unwrap();

        add_segment_to_config(&config, "~/work/repo").unwrap();

        let written = std::fs::read_to_string(&config).unwrap();
        assert!(written.contains("// hand-written"), "{}", written);
        assert!(written.contains("~/proj/*"), "{}", written);
        assert!(written.contains("~/work/repo"), "{}", written);
        assert!(written.contains("port 8788"), "{}", written);
    }

    #[test]
    fn a_segment_is_only_added_once() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.kdl");

        add_segment_to_config(&config, "~/proj/*").unwrap();
        add_segment_to_config(&config, "~/proj/*").unwrap();

        let written = std::fs::read_to_string(&config).unwrap();
        assert_eq!(written.matches("~/proj/*").count(), 1, "{}", written);
        assert!(written.contains("ancillaries"), "{}", written);
    }
}
