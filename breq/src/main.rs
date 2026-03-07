use anyhow::{Context, Result};
use clap::{CommandFactory, Parser, Subcommand};
use colored::Colorize;
use std::io::IsTerminal;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use toren_lib::{
    AssignmentManager, AssignmentRef, AssignmentSource, Config, Segment, SegmentManager,
    WorkspaceManager,
};
use tracing::info;
use tracing_subscriber::fmt::time::FormatTime;

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
        let hours = secs_of_day / 3600;
        let minutes = (secs_of_day % 3600) / 60;
        let seconds = secs_of_day % 60;

        write!(w, "{:02}:{:02}:{:02}", hours, minutes, seconds)
    }
}

#[derive(Parser)]
#[command(name = "breq")]
#[command(about = "Composable workspace orchestration for Claude ancillaries")]
struct Cli {
    /// Increase verbosity (-v for DEBUG, -vv for TRACE)
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,

    /// Path to config file (default: auto-discovered toren.toml)
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Assign work to a coding agent in a workspace
    Do {
        /// Assign to an existing workspace (e.g. "one", "three"); omit to create a new one
        workspace: Option<String>,

        /// Prompt for the agent session
        #[arg(short, long)]
        prompt: Option<String>,

        /// Intent template to use (e.g., "act", "plan", "review")
        #[arg(short, long)]
        intent: Option<String>,

        /// Tag assignment with a task identifier (e.g., bead ID)
        #[arg(long = "task-id", alias = "id")]
        task_id: Option<String>,

        /// Task title
        #[arg(long = "task-title")]
        task_title: Option<String>,

        /// Task URL
        #[arg(long = "task-url")]
        task_url: Option<String>,

        /// Segment to use (defaults to current directory's segment)
        #[arg(short, long)]
        segment: Option<String>,

        /// Agent to use (e.g., "claude", "codex:o3"). Overrides config; auto-detects if unset.
        #[arg(long)]
        agent: Option<String>,

        /// Additional arguments passed directly to the agent CLI
        #[arg(last = true)]
        passthrough: Vec<String>,
    },

    /// Open a shell in a workspace, optionally running a command
    #[command(visible_alias = "sh")]
    Shell {
        /// Workspace name (e.g. "one", "two")
        workspace: Option<String>,

        /// Run a specific hook (setup or destroy)
        #[arg(long)]
        hook: Option<HookArg>,

        /// Tag assignment with a task identifier
        #[arg(long = "task-id")]
        task_id: Option<String>,

        /// Task title
        #[arg(long = "task-title")]
        task_title: Option<String>,

        /// Task URL
        #[arg(long = "task-url")]
        task_url: Option<String>,

        /// Segment to use
        #[arg(short, long)]
        segment: Option<String>,

        /// Command to run in the workspace directory (after --)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        cmd: Vec<String>,
    },

    /// List active assignments
    List {
        /// Workspace or external ID to show detail for
        reference: Option<String>,

        /// List from all segments
        #[arg(short, long)]
        all: bool,

        /// List from a specific segment
        #[arg(short, long, conflicts_with = "all")]
        segment: Option<String>,

        /// Show detailed assignment info
        #[arg(long)]
        detail: bool,
    },

    /// Teardown a workspace (bead-free), output JSON to stdout
    Clean {
        /// Workspace name (e.g. "one", "three")
        workspace: String,

        /// Kill processes running in the workspace
        #[arg(long)]
        kill: bool,

        /// Push changes before cleanup
        #[arg(long)]
        push: bool,

        /// Segment to use
        #[arg(short, long)]
        segment: Option<String>,
    },

    /// Remove orphaned workspace directories
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

    /// Show a specific field from an assignment (for scripting)
    Show {
        /// Workspace name (e.g. "one", "two")
        workspace: String,

        /// Field path to show (e.g., "task.id", "task.title", "task.url", "task.source",
        /// "workspace.path", "segment", "ancillary_id", "session_id")
        #[arg(long)]
        field: String,

        /// Segment to use
        #[arg(short, long)]
        segment: Option<String>,
    },

    /// Remove assignment record without workspace cleanup
    Dismiss {
        /// Workspace or task ID reference
        reference: String,
    },
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum HookArg {
    Setup,
    Destroy,
}

fn main() -> Result<()> {
    let raw_args: Vec<String> = std::env::args().collect();

    // Pre-parse: check for alias match before clap gets involved
    if raw_args.len() > 1 {
        // Skip global flags to find the subcommand
        let mut subcmd_idx = 1;
        while subcmd_idx < raw_args.len() {
            let arg = &raw_args[subcmd_idx];
            if arg == "-v" || arg == "-vv" || arg == "-vvv" || arg == "--verbose" {
                subcmd_idx += 1;
                continue;
            }
            if arg == "--config" {
                subcmd_idx += 2; // skip --config <path>
                continue;
            }
            break;
        }

        if subcmd_idx < raw_args.len() {
            let subcmd = &raw_args[subcmd_idx];
            let plugin_args: Vec<String> = raw_args[subcmd_idx + 1..].to_vec();

            // Top-level help: inject plugin descriptions
            if subcmd == "--help" || subcmd == "-h" {
                if let Ok(config) = Config::load() {
                    if let Ok(plugin_mgr) = toren_lib::PluginManager::new(&config.plugins) {
                        let plugins = plugin_mgr.list_with_descriptions();
                        if !plugins.is_empty() {
                            let mut section = String::from("Plugins:");
                            for (name, desc) in &plugins {
                                match desc {
                                    Some(d) => section.push_str(&format!("\n  {:<16}{}", name, d)),
                                    None => section.push_str(&format!("\n  {}", name)),
                                }
                            }
                            Cli::command()
                                .after_help(section)
                                .print_help()
                                .ok();
                            std::process::exit(0);
                        }
                    }
                }
                // Fall through to normal clap --help if no plugins
            }

            // Try loading config for plugin/alias check (silently ignore config errors)
            if let Ok(config) = Config::load() {
                // Set up logging helper (shared by plugin and alias paths)
                let verbose_count: usize = raw_args[1..subcmd_idx]
                    .iter()
                    .map(|a| match a.as_str() {
                        "-vvv" => 3,
                        "-vv" => 2,
                        "-v" | "--verbose" => 1,
                        _ => 0,
                    })
                    .sum();
                let init_logging = |count: usize| {
                    if count > 0 {
                        let log_level = match count {
                            1 => tracing::Level::DEBUG,
                            _ => tracing::Level::TRACE,
                        };
                        tracing_subscriber::fmt()
                            .with_max_level(log_level)
                            .with_target(false)
                            .with_timer(ShortTime)
                            .init();
                    }
                };

                // 1. Plugin dispatch (highest priority)
                if let Ok(plugin_mgr) = toren_lib::PluginManager::new(&config.plugins) {
                    if plugin_mgr.has(subcmd) {
                        // Per-plugin help
                        if plugin_args.iter().any(|a| a == "--help" || a == "-h") {
                            if let Some(usage) = plugin_mgr.usage(subcmd) {
                                println!("{}", usage);
                            } else {
                                println!("Plugin '{}' (no help available)", subcmd);
                            }
                            std::process::exit(0);
                        }

                        init_logging(verbose_count);
                        info!("Plugin '{}'", subcmd);

                        // Resolve segment from CWD for plugin context
                        let (seg_path, seg_name) = resolve_segment_for_plugin(&config);

                        let mut ctx = toren_lib::PluginContext::new(seg_path, seg_name);
                        ctx.task_sources = config.tasks.sources.clone();

                        match plugin_mgr.run(subcmd, &plugin_args, ctx) {
                            Ok(toren_lib::PluginResult::Ok) => std::process::exit(0),
                            Ok(toren_lib::PluginResult::Action(action)) => {
                                match execute_deferred_action(&config, action) {
                                    Ok(()) => std::process::exit(0),
                                    Err(e) => {
                                        eprintln!("Error: {:#}", e);
                                        std::process::exit(1);
                                    }
                                }
                            }
                            Err(e) => {
                                eprintln!("Plugin '{}' error: {:#}", subcmd, e);
                                std::process::exit(1);
                            }
                        }
                    }
                }

                // 2. Alias dispatch (shell fallback)
                if let Some(template) = config.aliases.get(subcmd) {
                    init_logging(verbose_count);

                    let expanded = toren_lib::alias::expand_alias(template, &plugin_args);
                    info!("Alias '{}' -> {}", subcmd, expanded);
                    let code = toren_lib::alias::execute_alias(
                        &expanded,
                        &std::collections::HashMap::new(),
                    )?;
                    std::process::exit(code);
                }
            }
        }
    }

    let cli = Cli::parse();

    let log_level = match cli.verbose {
        0 => tracing::Level::INFO,
        1 => tracing::Level::DEBUG,
        _ => tracing::Level::TRACE,
    };

    tracing_subscriber::fmt()
        .with_max_level(log_level)
        .with_target(false)
        .with_timer(ShortTime)
        .init();

    // Load config once, shared across all commands
    let config = Config::load_from(cli.config.as_deref())?;

    match cli.command {
        Commands::Do {
            workspace,
            prompt,
            intent,
            task_id,
            task_title,
            task_url,
            segment,
            agent,
            passthrough,
        } => cmd_do(
            &config,
            workspace,
            prompt,
            intent,
            task_id,
            task_title,
            task_url,
            segment.as_deref(),
            agent,
            passthrough,
        ),
        Commands::Shell {
            workspace,
            hook,
            task_id,
            task_title,
            task_url,
            segment,
            cmd,
        } => cmd_shell(&config, workspace, hook, task_id, task_title, task_url, segment.as_deref(), cmd),
        Commands::List {
            reference,
            all,
            segment,
            detail,
        } => cmd_list(&config, reference, all, segment, detail),
        Commands::Clean {
            workspace,
            kill,
            push,
            segment,
        } => cmd_clean(&config, &workspace, kill, push, segment.as_deref()),
        Commands::Cleanup { segment, all } => cmd_cleanup(&config, all, segment),
        Commands::Init { stealth } => cmd_init(stealth),
        Commands::Show {
            workspace,
            field,
            segment,
        } => cmd_show(&config, &workspace, &field, segment.as_deref()),
        Commands::Dismiss { reference } => cmd_dismiss(&config, &reference),
    }
}

/// Helper to find segment from current directory or specified name.
fn resolve_segment(segment_mgr: &SegmentManager, segment_name: Option<&str>) -> Result<Segment> {
    if let Some(name) = segment_name {
        segment_mgr
            .find_by_name(name)
            .with_context(|| format!("Segment '{}' not found in any segment root", name))
    } else {
        let cwd = std::env::current_dir()?;
        segment_mgr.resolve_from_path(&cwd).with_context(|| {
            "Current directory is not under any configured segment.\n\
             Configure segments in ~/.toren/config.toml:\n\
             [ancillaries]\n\
             segments = [\"~/proj/*\"]"
        })
    }
}

/// Generate workspace name from ancillary number word.
fn workspace_name_for_number(n: u32) -> String {
    toren_lib::number_to_word(n).to_lowercase()
}

/// Resolve segment path and name from CWD for plugin context (best-effort).
fn resolve_segment_for_plugin(config: &Config) -> (Option<PathBuf>, Option<String>) {
    if let Ok(segment_mgr) = SegmentManager::new(config) {
        if let Ok(cwd) = std::env::current_dir() {
            if let Some(segment) = segment_mgr.resolve_from_path(&cwd) {
                return (Some(segment.path), Some(segment.name));
            }
        }
    }
    (None, None)
}

/// Execute a deferred action returned by a plugin script.
fn execute_deferred_action(config: &Config, action: toren_lib::DeferredAction) -> Result<()> {
    match action {
        toren_lib::DeferredAction::Do {
            task_id,
            task_title,
            task_url,
            prompt,
            intent,
        } => {
            cmd_do(
                config,
                None,       // workspace (auto-create)
                prompt,
                intent,
                task_id,
                task_title,
                task_url,
                None,       // segment (resolve from CWD)
                None,       // agent (use config/auto-detect)
                Vec::new(), // passthrough
            )
        }
    }
}

// ─── do ─────────────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn cmd_do(
    config: &Config,
    workspace: Option<String>,
    prompt: Option<String>,
    intent: Option<String>,
    task_id_arg: Option<String>,
    task_title_arg: Option<String>,
    task_url_arg: Option<String>,
    segment_name: Option<&str>,
    agent_str: Option<String>,
    passthrough: Vec<String>,
) -> Result<()> {
    let agent = config.resolve_agent(agent_str.as_deref())?;
    let workspace_root = config.ancillaries.workspace_root.clone();

    let workspace_mgr = WorkspaceManager::new(workspace_root, Some(config.proxy.domain.clone()));
    let segment_mgr = SegmentManager::new(config)?;
    let mut assignment_mgr = AssignmentManager::new()?;

    let segment = resolve_segment(&segment_mgr, segment_name)?;

    // Infer task fields from CLI args
    let inferred = toren_lib::infer_task_fields(
        task_id_arg.as_deref(),
        task_title_arg.as_deref(),
        task_url_arg.as_deref(),
        None, // prompt not known yet
    );

    // 1. System prompt from intent (optional, rendered as --append-system-prompt)
    let system_prompt = if let Some(ref intent_name) = intent {
        let template = config
            .intents
            .get(intent_name)
            .with_context(|| format!("Unknown intent: {}", intent_name))?;

        // Fetch task description if we have a task_id
        let task_description = inferred.task_id.as_ref().and_then(|id| {
            let plugin_mgr = toren_lib::PluginManager::new(&config.plugins).ok()?;
            let ctx = toren_lib::PluginContext::new(Some(segment.path.clone()), Some(segment.name.clone()));
            if let Some(source) = inferred.task_source.as_deref() {
                // Source is known (e.g., "beads:foo-123") — direct lookup
                plugin_mgr.resolve_info(source, id, ctx).ok()
            } else {
                // Source unknown — search across all task plugins
                let sources = plugin_mgr.effective_sources(&config.tasks.sources);
                plugin_mgr.resolve_info_multi(&sources, id, ctx).ok()
            }.and_then(|t| t.description)
        });

        // Build task context for template rendering
        let task_id = inferred.task_id.clone().unwrap_or_default();
        let task_title = inferred.task_title.clone().unwrap_or_else(|| task_id.clone());
        let ctx = toren_lib::WorkspaceContext {
            ws: toren_lib::WorkspaceInfo {
                name: String::new(),
                num: 0,
                path: String::new(),
            },
            repo: toren_lib::RepoInfo {
                root: segment.path.display().to_string(),
                name: segment.name.clone(),
            },
            task: Some(toren_lib::TaskInfo {
                id: task_id,
                title: task_title,
                description: task_description,
                url: inferred.task_url.clone(),
                source: inferred.task_source.clone(),
            }),
            vars: std::collections::HashMap::new(),
        };
        Some(toren_lib::render_template(template, &ctx)?)
    } else {
        None
    };

    // 2. User message: provided prompt > stdin > $EDITOR
    let user_message = if let Some(ref p) = prompt {
        p.clone()
    } else if !std::io::stdin().is_terminal() {
        // Read from stdin (piped input)
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        let trimmed = buf.trim().to_string();
        if trimmed.is_empty() {
            anyhow::bail!("Empty input from stdin. Provide -p or pipe a prompt.");
        }
        trimmed
    } else if system_prompt.is_some() {
        // Intent provides system prompt; open editor for user message
        let text = edit::edit("")
            .context("Editor returned an error")?;
        let trimmed = text.trim().to_string();
        if trimmed.is_empty() {
            anyhow::bail!("Empty prompt from editor. Provide -p or pipe a prompt.");
        }
        trimmed
    } else {
        // No intent, no prompt, no stdin — open editor
        let text = edit::edit("")
            .context("Editor returned an error")?;
        let trimmed = text.trim().to_string();
        if trimmed.is_empty() {
            anyhow::bail!("Empty prompt from editor. Provide -p, -i, or pipe a prompt.");
        }
        trimmed
    };

    // Determine workspace: reuse existing or create new
    if let Some(ref ws_name) = workspace {
        let ws_name_lower = ws_name.to_lowercase();
        let ws_path = workspace_mgr.workspace_path(&segment.name, &ws_name_lower);

        if !ws_path.exists() {
            anyhow::bail!("Workspace '{}' not found at {}", ws_name_lower, ws_path.display());
        }

        // Reuse workspace — start agent session
        eprintln!("Starting {} session in {}\n", agent, ws_path.display());
        let mut cmd = agent.build_command(&user_message, &ws_path, system_prompt.as_deref());
        cmd.args(&passthrough);

        let err = cmd.exec();
        Err(err).context(format!("Failed to exec {}", agent.kind.binary_name()))
    } else {
        // Create new workspace
        let existing_workspaces = workspace_mgr
            .list_workspaces(&segment.path)
            .unwrap_or_default();
        let ancillary_id_str = assignment_mgr.next_available_ancillary(
            &segment.name,
            config.ancillaries.max_per_segment,
            &existing_workspaces,
        );
        let ancillary_num = toren_lib::ancillary_number(&ancillary_id_str).unwrap_or(1);
        eprintln!("Ancillary: {}", ancillary_id_str);

        let base_branch = workspace_mgr.active_branch(&segment.path);
        let ws_name = workspace_name_for_number(ancillary_num);

        let (ws_path, _setup_result) = workspace_mgr.create_workspace_with_setup(
            &segment.path,
            &segment.name,
            &ws_name,
            ancillary_num,
        )?;
        eprintln!("Workspace: {}", ws_path.display());

        // Record assignment
        let source = if inferred.task_id.is_some() {
            AssignmentSource::Reference
        } else {
            AssignmentSource::Prompt {
                original_prompt: user_message.clone(),
            }
        };

        // Use inferred title, falling back to first 80 chars of user message
        let title: Option<String> = inferred.task_title.clone().or_else(|| Some(
            user_message
                .lines()
                .next()
                .unwrap_or(&user_message)
                .chars()
                .take(80)
                .collect(),
        ));

        assignment_mgr.create(
            &ancillary_id_str,
            inferred.task_id.as_deref(),
            source,
            &segment.name,
            ws_path.clone(),
            title,
            base_branch,
            inferred.task_url.as_deref(),
            inferred.task_source.as_deref(),
        )?;

        // Exec into agent
        eprintln!("Starting {} session in {}\n", agent, ws_path.display());
        let mut cmd = agent.build_command(&user_message, &ws_path, system_prompt.as_deref());
        cmd.args(&passthrough);

        let err = cmd.exec();
        Err(err).context(format!("Failed to exec {}", agent.kind.binary_name()))
    }
}

// ─── shell ──────────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn cmd_shell(
    config: &Config,
    workspace: Option<String>,
    hook: Option<HookArg>,
    task_id_arg: Option<String>,
    task_title_arg: Option<String>,
    task_url_arg: Option<String>,
    segment_name: Option<&str>,
    cmd: Vec<String>,
) -> Result<()> {
    // Hook mode: run setup/destroy from cwd
    if let Some(hook_type) = hook {
        let workspace_root = config.ancillaries.workspace_root.clone();
        let workspace_mgr = WorkspaceManager::new(workspace_root, Some(config.proxy.domain.clone()));

        let (segment_path, workspace_path, workspace_name) = detect_workspace_context()?;
        let ancillary_num = toren_lib::word_to_number(&workspace_name);

        match hook_type {
            HookArg::Setup => {
                eprintln!(
                    "Running setup for workspace '{}' in {}",
                    workspace_name,
                    workspace_path.display()
                );
                workspace_mgr.run_setup(
                    &segment_path,
                    &workspace_path,
                    &workspace_name,
                    ancillary_num.unwrap_or(0),
                )?;
                eprintln!("Setup complete.");
            }
            HookArg::Destroy => {
                eprintln!(
                    "Running destroy for workspace '{}' in {}",
                    workspace_name,
                    workspace_path.display()
                );
                workspace_mgr.run_destroy(
                    &segment_path,
                    &workspace_path,
                    &workspace_name,
                )?;
                eprintln!("Destroy complete.");
            }
        }
        return Ok(());
    }

    let workspace_root = config.ancillaries.workspace_root.clone();

    let workspace_mgr = WorkspaceManager::new(workspace_root, Some(config.proxy.domain.clone()));
    let segment_mgr = SegmentManager::new(config)?;
    let segment = resolve_segment(&segment_mgr, segment_name)?;

    if let Some(ref ws_name) = workspace {
        let ws_name_lower = ws_name.to_lowercase();
        let ws_path = workspace_mgr.workspace_path(&segment.name, &ws_name_lower);

        if !ws_path.exists() {
            anyhow::bail!("Workspace '{}' not found at {}", ws_name_lower, ws_path.display());
        }

        let (program, args): (String, Vec<String>) = if cmd.is_empty() {
            let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
            (shell, vec![])
        } else {
            (cmd[0].clone(), cmd[1..].to_vec())
        };

        println!("{}", ws_path.display());
        let err = Command::new(&program)
            .args(&args)
            .current_dir(&ws_path)
            .exec();
        Err(err).with_context(|| format!("Failed to exec: {}", program))
    } else if !cmd.is_empty() {
        // No workspace, command given — run in cwd
        let (program, args) = (cmd[0].clone(), cmd[1..].to_vec());
        let err = Command::new(&program).args(&args).exec();
        Err(err).with_context(|| format!("Failed to exec: {}", program))
    } else {
        // No workspace, no command — create new workspace and drop into shell
        let mut assignment_mgr = AssignmentManager::new()?;
        let existing_workspaces = workspace_mgr
            .list_workspaces(&segment.path)
            .unwrap_or_default();
        let ancillary_id_str = assignment_mgr.next_available_ancillary(
            &segment.name,
            config.ancillaries.max_per_segment,
            &existing_workspaces,
        );
        let ancillary_num = toren_lib::ancillary_number(&ancillary_id_str).unwrap_or(1);

        let base_branch = workspace_mgr.active_branch(&segment.path);
        let ws_name = workspace_name_for_number(ancillary_num);

        let (ws_path, _) = workspace_mgr.create_workspace_with_setup(
            &segment.path,
            &segment.name,
            &ws_name,
            ancillary_num,
        )?;

        // Infer task fields from CLI args
        let inferred = toren_lib::infer_task_fields(
            task_id_arg.as_deref(),
            task_title_arg.as_deref(),
            task_url_arg.as_deref(),
            None,
        );

        let source = if inferred.task_id.is_some() {
            AssignmentSource::Reference
        } else {
            AssignmentSource::Prompt {
                original_prompt: "(interactive shell)".to_string(),
            }
        };

        assignment_mgr.create(
            &ancillary_id_str,
            inferred.task_id.as_deref(),
            source,
            &segment.name,
            ws_path.clone(),
            inferred.task_title,
            base_branch,
            inferred.task_url.as_deref(),
            inferred.task_source.as_deref(),
        )?;

        eprintln!("Created workspace: {}", ws_path.display());
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        println!("{}", ws_path.display());
        let err = Command::new(&shell).current_dir(&ws_path).exec();
        Err(err).context("Failed to exec shell")
    }
}

// ─── list ───────────────────────────────────────────────────────────────────

fn cmd_list(
    config: &Config,
    reference: Option<String>,
    all_segments: bool,
    segment_name: Option<String>,
    detail: bool,
) -> Result<()> {
    let workspace_root = config.ancillaries.workspace_root.clone();
    let segment_mgr = SegmentManager::new(config)?;
    let mut assignment_mgr = AssignmentManager::new()?;

    // If a specific reference is given and --detail, show detailed info
    if let Some(ref reference) = reference {
        if detail {
            return cmd_list_detail(config, &segment_mgr, &mut assignment_mgr, reference);
        }
    }

    // Determine which segment(s) to list
    let (assignments, segments, scope_label): (Vec<_>, Vec<Segment>, String) = if all_segments {
        let assignments = assignment_mgr.list_active().into_iter().collect::<Vec<_>>();
        let segments = segment_mgr.list_all();
        (assignments, segments, "all segments".to_string())
    } else if let Some(ref name) = segment_name {
        let assignments = assignment_mgr
            .list_active_segment(name)
            .into_iter()
            .collect::<Vec<_>>();
        let segments = segment_mgr
            .find_by_name(name)
            .map(|s| vec![s])
            .unwrap_or_default();
        (assignments, segments, name.clone())
    } else {
        let segment = resolve_segment(&segment_mgr, None)?;
        let name = segment.name.clone();
        let assignments = assignment_mgr
            .list_active_segment(&name)
            .into_iter()
            .collect::<Vec<_>>();
        (assignments, vec![segment], name)
    };

    let has_assignments = !assignments.is_empty();

    let term_width = terminal_size::terminal_size()
        .map(|(w, _)| w.0 as usize)
        .unwrap_or(80);

    // Compute workspace column width from the longest name we'll display
    let ws_col_width = assignments
        .iter()
        .map(|a| {
            let name = if all_segments {
                a.ancillary_id.as_str()
            } else {
                a.ancillary_id.split_whitespace().last().unwrap_or(&a.ancillary_id)
            };
            // Account for " *" dirty suffix
            name.len() + 2
        })
        .max()
        .unwrap_or(10);

    // Fixed column widths: workspace(dynamic) + ext_id(15) + activity(6) + spaces(3)
    let fixed_width: usize = ws_col_width + 15 + 6 + 3;

    for assignment in &assignments {
        // Agent activity
        let agent_activity = toren_lib::composite_status::detect_agent_activity(
            &assignment.workspace_path,
        );

        // Has changes
        let has_changes = toren_lib::composite_status::workspace_has_changes(
            &assignment.workspace_path,
            assignment.base_branch.as_deref(),
        );

        // Workspace name — extract short name, mark dirty with *
        // In --all mode, use full ancillary ID for disambiguation
        let ancillary_name = if all_segments {
            assignment.ancillary_id.as_str()
        } else {
            assignment
                .ancillary_id
                .split_whitespace()
                .last()
                .unwrap_or(&assignment.ancillary_id)
        };

        let ws_text = if has_changes {
            format!("{:<width$}", format!("{} *", ancillary_name), width = ws_col_width)
        } else {
            format!("{:<width$}", ancillary_name, width = ws_col_width)
        };
        let ws_colored = if has_changes {
            ws_text.yellow()
        } else {
            ws_text.normal()
        };

        // Task ID (if any)
        let task_id_display = assignment
            .task_id
            .as_deref()
            .unwrap_or("-");

        let activity_text = format!("{:<6}", agent_activity);
        let activity_colored = if agent_activity == "busy" {
            activity_text.yellow()
        } else {
            activity_text.green()
        };

        // Always show title; in detail mode show all task fields
        let title_max = term_width.saturating_sub(fixed_width);
        let title = assignment
            .task_title
            .as_deref()
            .map(|t| truncate_title(t, title_max))
            .unwrap_or_else(|| "-".to_string());

        println!(
            "{} {:<15} {} {}",
            ws_colored, task_id_display, activity_colored, title
        );
    }

    // Detect orphaned workspace directories
    {
        let ws_mgr = WorkspaceManager::new(workspace_root, Some(config.proxy.domain.clone()));
        let orphans = find_orphaned_workspaces(&ws_mgr, &segments, &assignments);

        if !orphans.is_empty() {
            for (segment_name, ws_name, path) in &orphans {
                tracing::debug!("orphaned workspace dir: {}/{} ({})", segment_name, ws_name, path.display());
            }
            tracing::debug!(
                "{} orphaned workspace dir(s) (will be reclaimed on next assign, or run `breq cleanup`)",
                orphans.len()
            );
        }
    }

    if !has_assignments && !all_segments {
        println!("No active assignments in {}.", scope_label);
        println!("Use --all to see assignments across all segments.");
    } else if !has_assignments {
        println!("No active assignments in {}.", scope_label);
    }

    Ok(())
}

fn cmd_list_detail(
    config: &Config,
    segment_mgr: &SegmentManager,
    assignment_mgr: &mut AssignmentManager,
    reference: &str,
) -> Result<()> {
    let segment = resolve_segment(segment_mgr, None)?;
    let ref_ = AssignmentRef::parse(reference, &segment.name);
    let assignments = assignment_mgr.resolve(&ref_);

    if assignments.is_empty() {
        anyhow::bail!("No assignment found for: {}", reference);
    }

    for assignment in assignments {
        println!("Assignment: {}", assignment.id);
        println!("  Ancillary:    {}", assignment.ancillary_id);
        if let Some(ref task_id) = assignment.task_id {
            println!("  Task ID:      {}", task_id);
        }
        if let Some(ref task_title) = assignment.task_title {
            println!("  Task Title:   {}", task_title);
        }
        if let Some(ref task_url) = assignment.task_url {
            println!("  Task URL:     {}", task_url);
        }
        if let Some(ref task_source) = assignment.task_source {
            println!("  Task Source:   {}", task_source);
        }
        println!("  Segment:      {}", assignment.segment);
        println!("  Status:       {:?}", assignment.status);
        println!("  Source:       {:?}", assignment.source);
        println!("  Workspace:    {}", assignment.workspace_path.display());
        if let Some(ref branch) = assignment.base_branch {
            println!("  Base:         {}", branch);
        }
        if let Some(ref sid) = assignment.session_id {
            println!("  Session:      {}", sid);
        }
        println!("  Created:      {}", assignment.created_at);
        println!("  Updated:      {}", assignment.updated_at);

        // Show workspace info if exists
        if assignment.workspace_path.exists() {
            println!("\nRecent changes:");
            if assignment.workspace_path.join(".jj").exists() {
                let _ = Command::new("jj")
                    .args(["log", "-n", "5"])
                    .current_dir(&assignment.workspace_path)
                    .status();
            } else if assignment.workspace_path.join(".git").exists() {
                let base = assignment.base_branch.as_deref().unwrap_or("main");
                let range = format!("{}..HEAD", base);
                let _ = Command::new("git")
                    .args(["log", "--oneline", &range])
                    .current_dir(&assignment.workspace_path)
                    .status();
            }
        } else {
            println!("\n(Workspace not found)");
        }

        // Show task details if task_id present
        if let Some(ref task_id) = assignment.task_id {
            let seg_path = segment_mgr
                .find_by_name(&assignment.segment)
                .map(|s| s.path);
            if let Some(seg_path) = seg_path {
                println!("\nTask details:");
                let _ = Command::new("bd")
                    .args(["show", task_id])
                    .current_dir(&seg_path)
                    .status();
            }
        }

        println!();
    }

    let _ = config; // suppress unused warning
    Ok(())
}

// ─── clean ──────────────────────────────────────────────────────────────────

fn cmd_clean(
    config: &Config,
    workspace: &str,
    kill: bool,
    push: bool,
    segment_name: Option<&str>,
) -> Result<()> {
    let workspace_root = config.ancillaries.workspace_root.clone();

    let segment_mgr = SegmentManager::new(config)?;
    let workspace_mgr = WorkspaceManager::new(workspace_root, Some(config.proxy.domain.clone()));
    let mut assignment_mgr = AssignmentManager::new()?;

    let segment = resolve_segment(&segment_mgr, segment_name)?;

    // Resolve workspace name to assignment
    let ws_name = workspace.to_lowercase();
    let ancillary_num = toren_lib::word_to_number(&ws_name).unwrap_or(0);
    let ancillary_id_str = toren_lib::ancillary_id(&segment.name, ancillary_num);

    let assignment = assignment_mgr
        .get_active_for_ancillary(&ancillary_id_str)
        .cloned()
        .with_context(|| format!("No assignment found for workspace '{}'", ws_name))?;

    // Render auto-commit message
    let auto_commit_message = toren_lib::render_auto_commit_message(
        toren_lib::DEFAULT_AUTO_COMMIT_MESSAGE,
        &assignment,
        &segment.name,
        &segment.path,
    );

    eprintln!(
        "Cleaning workspace: {} ({})",
        ws_name,
        assignment.workspace_path.display()
    );

    let opts = toren_lib::CleanOptions {
        push,
        segment_path: &segment.path,
        kill,
        auto_commit_message,
    };

    let result =
        toren_lib::clean_assignment(&assignment, &mut assignment_mgr, &workspace_mgr, &opts)?;

    // Structured JSON to stdout
    let json = serde_json::to_string(&result)?;
    println!("{}", json);

    Ok(())
}

// ─── cleanup ────────────────────────────────────────────────────────────────

fn cmd_cleanup(config: &Config, all_segments: bool, segment_name: Option<String>) -> Result<()> {
    let workspace_root = config.ancillaries.workspace_root.clone();

    let segment_mgr = SegmentManager::new(config)?;
    let mut assignment_mgr = AssignmentManager::new()?;
    let ws_mgr = WorkspaceManager::new(workspace_root, Some(config.proxy.domain.clone()));

    let (assignments, segments): (Vec<_>, Vec<Segment>) = if all_segments {
        let assignments = assignment_mgr.list_active().into_iter().collect();
        let segments = segment_mgr.list_all();
        (assignments, segments)
    } else if let Some(ref name) = segment_name {
        let assignments = assignment_mgr
            .list_active_segment(name)
            .into_iter()
            .collect();
        let segments = segment_mgr
            .find_by_name(name)
            .map(|s| vec![s])
            .unwrap_or_default();
        (assignments, segments)
    } else {
        let segment = resolve_segment(&segment_mgr, None)?;
        let name = segment.name.clone();
        let assignments = assignment_mgr
            .list_active_segment(&name)
            .into_iter()
            .collect();
        (assignments, vec![segment])
    };

    let orphans = find_orphaned_workspaces(&ws_mgr, &segments, &assignments);

    if orphans.is_empty() {
        println!("No orphaned workspace directories found.");
        return Ok(());
    }

    println!("Removing {} orphaned workspace dir(s):", orphans.len());
    for (segment_name, ws_name, path) in &orphans {
        print!("  {}/{}...", segment_name, ws_name);
        match std::fs::remove_dir_all(path) {
            Ok(()) => println!(" removed"),
            Err(e) => println!(" failed: {}", e),
        }
    }

    Ok(())
}

// ─── dismiss ────────────────────────────────────────────────────────────────

fn cmd_dismiss(config: &Config, reference: &str) -> Result<()> {
    let segment_mgr = SegmentManager::new(config)?;
    let mut assignment_mgr = AssignmentManager::new()?;
    let segment = resolve_segment(&segment_mgr, None)?;

    let ref_ = AssignmentRef::parse(reference, &segment.name);
    let assignments: Vec<_> = assignment_mgr
        .resolve(&ref_)
        .iter()
        .map(|a| (*a).clone())
        .collect();

    if assignments.is_empty() {
        anyhow::bail!("No assignment found for: {}", reference);
    }

    for assignment in &assignments {
        assignment_mgr.remove(&assignment.id)?;
        println!(
            "Dismissed: {} ({})",
            assignment.ancillary_id,
            assignment.task_id.as_deref().unwrap_or("-")
        );
    }

    Ok(())
}

// ─── show ────────────────────────────────────────────────────────────────────

fn cmd_show(
    config: &Config,
    workspace: &str,
    field: &str,
    segment_name: Option<&str>,
) -> Result<()> {
    let segment_mgr = SegmentManager::new(config)?;
    let mut assignment_mgr = AssignmentManager::new()?;
    let segment = resolve_segment(&segment_mgr, segment_name)?;

    let ws_name = workspace.to_lowercase();
    let ancillary_num = toren_lib::word_to_number(&ws_name).unwrap_or(0);
    let ancillary_id_str = toren_lib::ancillary_id(&segment.name, ancillary_num);

    let assignment = assignment_mgr
        .get_active_for_ancillary(&ancillary_id_str)
        .with_context(|| format!("No assignment found for workspace '{}'", ws_name))?;

    let value = match field {
        "task.id" => assignment.task_id.as_deref().unwrap_or("").to_string(),
        "task.title" => assignment.task_title.as_deref().unwrap_or("").to_string(),
        "task.url" => assignment.task_url.as_deref().unwrap_or("").to_string(),
        "task.source" => assignment.task_source.as_deref().unwrap_or("").to_string(),
        "workspace.path" => assignment.workspace_path.display().to_string(),
        "segment" => assignment.segment.clone(),
        "ancillary_id" => assignment.ancillary_id.clone(),
        "session_id" => assignment.session_id.as_deref().unwrap_or("").to_string(),
        _ => anyhow::bail!(
            "Unknown field: {}. Supported: task.id, task.title, task.url, task.source, workspace.path, segment, ancillary_id, session_id",
            field
        ),
    };

    println!("{}", value);
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

    // Must be at the workspace/repo root
    if has_jj {
        let output = Command::new("jj")
            .args(["workspace", "root"])
            .current_dir(&cwd)
            .output()
            .context("Failed to run jj workspace root")?;

        if !output.status.success() {
            anyhow::bail!("Failed to determine jj workspace root");
        }

        let jj_root =
            std::path::PathBuf::from(String::from_utf8_lossy(&output.stdout).trim().to_string());

        if jj_root != cwd {
            anyhow::bail!(
                "breq init must be run from the workspace root: {}",
                jj_root.display()
            );
        }
    } else {
        let output = Command::new("git")
            .args(["rev-parse", "--show-toplevel"])
            .current_dir(&cwd)
            .output()
            .context("Failed to run git rev-parse")?;

        if !output.status.success() {
            anyhow::bail!("Failed to determine git repo root");
        }

        let git_root =
            std::path::PathBuf::from(String::from_utf8_lossy(&output.stdout).trim().to_string());

        if git_root != cwd {
            anyhow::bail!(
                "breq init must be run from the repo root: {}",
                git_root.display()
            );
        }
    }

    let config_path = cwd.join("toren.kdl");
    if config_path.exists() || cwd.join(".toren.kdl").exists() {
        anyhow::bail!("toren.kdl already exists. Remove it first to re-initialize.");
    }

    // Collect setup actions
    let mut copy_entries: Vec<String> = Vec::new();
    let mut share_entries: Vec<String> = Vec::new();

    if cwd.join(".beads").exists() {
        let is_tracked = std::process::Command::new("git")
            .args(["ls-files", "--error-unmatch", ".beads"])
            .current_dir(&cwd)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !is_tracked {
            share_entries.push(".beads".to_string());
        }
    }

    let well_known_artifacts = [
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
        let gitignore =
            std::fs::read_to_string(&gitignore_path).context("Failed to read .gitignore")?;

        for line in gitignore.lines() {
            let line = line.trim().trim_end_matches('/');
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            for artifact in &well_known_artifacts {
                if line == *artifact || line.ends_with(&format!("/{}", artifact)) {
                    let artifact_path = cwd.join(line);
                    if artifact_path.is_dir() {
                        let entry = line.to_string();
                        if !copy_entries.contains(&entry) {
                            copy_entries.push(entry);
                        }
                    }
                }
            }
        }
    }

    for artifact in &well_known_artifacts {
        let artifact_path = cwd.join(artifact);
        if artifact_path.is_dir() {
            let entry = artifact.to_string();
            if !copy_entries.contains(&entry) {
                copy_entries.push(entry);
            }
        }
    }

    let mut kdl = String::from("setup {\n");
    for entry in &share_entries {
        kdl.push_str(&format!("    share src=\"{}\"\n", entry));
    }
    for entry in &copy_entries {
        kdl.push_str(&format!("    copy src=\"{}\"\n", entry));
    }
    kdl.push_str("}\n\ndestroy { }\n");

    let total = share_entries.len() + copy_entries.len();
    std::fs::write(&config_path, &kdl).context("Failed to write toren.kdl")?;
    println!("Created toren.kdl with {} setup entries", total);

    for entry in &share_entries {
        println!("  share src=\"{}\"", entry);
    }
    for entry in &copy_entries {
        println!("  copy src=\"{}\"", entry);
    }

    if stealth {
        let git_info_dir = cwd.join(".git").join("info");
        if git_info_dir.exists() {
            let exclude_path = git_info_dir.join("exclude");
            let existing = std::fs::read_to_string(&exclude_path).unwrap_or_default();
            if !existing.lines().any(|l| l.trim() == "toren.kdl") {
                let mut content = existing;
                if !content.ends_with('\n') && !content.is_empty() {
                    content.push('\n');
                }
                content.push_str("toren.kdl\n");
                std::fs::write(&exclude_path, content)
                    .context("Failed to update .git/info/exclude")?;
                println!("Added toren.kdl to .git/info/exclude");
            }
        } else {
            println!("Warning: .git/info directory not found, --stealth had no effect");
        }
    }

    // Segment onboarding: check if this repo is discoverable as a segment
    if let Ok(config) = Config::load() {
        let segment_mgr = SegmentManager::new(&config)?;
        if segment_mgr.resolve_from_path(&cwd).is_none() {
            // Repo not discoverable — offer to add it
            if let Some(parent) = cwd.parent() {
                let repo_path = toren_lib::tilde_shorten(&cwd);
                let parent_glob = format!("{}/*", toren_lib::tilde_shorten(parent));

                if std::io::stdin().is_terminal() {
                    eprintln!("\nThis repo is not discoverable as a segment.");
                    eprintln!("Add it to ~/.toren/config.toml?");
                    eprintln!("  1) Add parent glob: {}", parent_glob);
                    eprintln!("  2) Add repo path:   {}", repo_path);
                    eprintln!("  3) Skip");
                    eprint!("Choice [1/2/3]: ");

                    let mut input = String::new();
                    std::io::stdin().read_line(&mut input)?;
                    let choice = input.trim();

                    let new_entry = match choice {
                        "1" => Some(parent_glob),
                        "2" => Some(repo_path),
                        _ => None,
                    };

                    if let Some(entry) = new_entry {
                        let config_path = dirs::home_dir()
                            .context("Could not determine home directory")?
                            .join(".toren/config.toml");

                        add_segment_to_config(&config_path, &entry)?;
                    }
                }
            }
        }
    }

    Ok(())
}

// ─── helpers ────────────────────────────────────────────────────────────────

/// Add a segment entry to ~/.toren/config.toml using toml_edit for
/// targeted insertion (preserves comments, doesn't expand defaults).
fn add_segment_to_config(config_path: &std::path::Path, entry: &str) -> Result<()> {
    use toml_edit::{value, Array, DocumentMut};

    let content = if config_path.exists() {
        std::fs::read_to_string(config_path)?
    } else {
        String::new()
    };

    let mut doc: DocumentMut = content.parse().unwrap_or_default();

    // Ensure [ancillaries] table exists
    if !doc.contains_table("ancillaries") {
        doc["ancillaries"] = toml_edit::Item::Table(toml_edit::Table::new());
    }

    let ancillaries = doc["ancillaries"].as_table_mut().unwrap();

    // Get or create the segments array
    if !ancillaries.contains_key("segments") {
        let mut arr = Array::new();
        arr.push(entry);
        ancillaries["segments"] = value(arr);
        write_config(config_path, &doc)?;
        println!("Added '{}' to ~/.toren/config.toml", entry);
        return Ok(());
    }

    if let Some(arr) = ancillaries["segments"].as_array_mut() {
        // Check for duplicates
        let already_present = arr.iter().any(|v| v.as_str() == Some(entry));
        if already_present {
            println!("'{}' already in config", entry);
            return Ok(());
        }
        arr.push(entry);
    }

    write_config(config_path, &doc)?;
    println!("Added '{}' to ~/.toren/config.toml", entry);
    Ok(())
}

fn write_config(path: &std::path::Path, doc: &toml_edit::DocumentMut) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).context("Failed to create config directory")?;
    }
    std::fs::write(path, doc.to_string()).context("Failed to write config file")
}

/// Find workspace directories that exist on disk but are not tracked by VCS
/// and have no assignment record.
fn find_orphaned_workspaces(
    ws_mgr: &WorkspaceManager,
    segments: &[Segment],
    assignments: &[&toren_lib::Assignment],
) -> Vec<(String, String, std::path::PathBuf)> {
    let mut orphans = Vec::new();

    for segment in segments {
        let tracked_workspaces = ws_mgr.list_workspaces(&segment.path).unwrap_or_default();

        let assigned_paths: std::collections::HashSet<_> = assignments
            .iter()
            .filter(|a| a.segment.to_lowercase() == segment.name.to_lowercase())
            .map(|a| a.workspace_path.clone())
            .collect();

        let segment_ws_dir = ws_mgr.workspace_path(&segment.name, "");
        if let Ok(entries) = std::fs::read_dir(&segment_ws_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                let ws_name = match path.file_name().and_then(|n| n.to_str()) {
                    Some(n) => n.to_string(),
                    None => continue,
                };

                if tracked_workspaces.contains(&ws_name) {
                    continue;
                }
                if assigned_paths.contains(&path) {
                    continue;
                }

                orphans.push((segment.name.clone(), ws_name, path));
            }
        }
    }

    orphans
}

fn truncate_title(title: &str, max_len: usize) -> String {
    if title.chars().count() <= max_len {
        title.to_string()
    } else if max_len > 3 {
        let end: String = title.chars().take(max_len - 3).collect();
        format!("{}...", end)
    } else {
        title.chars().take(max_len).collect()
    }
}

/// Detect workspace context from current directory.
fn detect_workspace_context() -> Result<(std::path::PathBuf, std::path::PathBuf, String)> {
    let cwd = std::env::current_dir()?;

    if cwd.join(".jj").exists() {
        return detect_jj_workspace_context(&cwd);
    }

    let git_entry = cwd.join(".git");
    if git_entry.exists() && git_entry.is_file() {
        return detect_git_worktree_context(&cwd);
    }

    anyhow::bail!(
        "Not in a VCS workspace. Run this command from within a jj workspace or git worktree."
    );
}

fn detect_jj_workspace_context(
    cwd: &std::path::Path,
) -> Result<(std::path::PathBuf, std::path::PathBuf, String)> {
    let output = Command::new("jj")
        .args(["workspace", "root"])
        .current_dir(cwd)
        .output()
        .context("Failed to run jj workspace root")?;

    if !output.status.success() {
        anyhow::bail!("Failed to determine workspace root");
    }

    let workspace_path =
        std::path::PathBuf::from(String::from_utf8_lossy(&output.stdout).trim().to_string());

    let workspace_name = workspace_path
        .file_name()
        .and_then(|n| n.to_str())
        .context("Invalid workspace path")?
        .to_string();

    let mut segment_path = None;
    let mut check_path = workspace_path.parent();
    while let Some(parent) = check_path {
        if parent.join("toren.kdl").exists() || parent.join(".toren.kdl").exists() {
            segment_path = Some(parent.to_path_buf());
            break;
        }
        if parent.join(".jj").exists() && parent != workspace_path {
            segment_path = Some(parent.to_path_buf());
            break;
        }
        check_path = parent.parent();
    }

    let segment_path = segment_path
        .context("Could not find segment root. Ensure you're in a breq-managed workspace.")?;

    Ok((segment_path, workspace_path, workspace_name))
}

fn detect_git_worktree_context(
    cwd: &std::path::Path,
) -> Result<(std::path::PathBuf, std::path::PathBuf, String)> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(cwd)
        .output()
        .context("Failed to run git rev-parse --show-toplevel")?;

    if !output.status.success() {
        anyhow::bail!("Failed to determine git worktree root");
    }

    let workspace_path =
        std::path::PathBuf::from(String::from_utf8_lossy(&output.stdout).trim().to_string());

    let workspace_name = workspace_path
        .file_name()
        .and_then(|n| n.to_str())
        .context("Invalid workspace path")?
        .to_string();

    let mut segment_path = None;
    let mut check_path = workspace_path.parent();
    while let Some(parent) = check_path {
        if parent.join("toren.kdl").exists() || parent.join(".toren.kdl").exists() {
            segment_path = Some(parent.to_path_buf());
            break;
        }
        if parent.join(".git").is_dir() && parent != workspace_path {
            segment_path = Some(parent.to_path_buf());
            break;
        }
        check_path = parent.parent();
    }

    let segment_path = segment_path
        .context("Could not find segment root. Ensure you're in a breq-managed workspace.")?;

    Ok((segment_path, workspace_path, workspace_name))
}
