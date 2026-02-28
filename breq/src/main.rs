use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
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
    /// Start a Claude session in a workspace (create workspace if needed)
    Cmd {
        /// Reuse existing workspace (by name, e.g. "one", "three")
        workspace: Option<String>,

        /// Prompt for the Claude session
        #[arg(short, long)]
        prompt: Option<String>,

        /// Intent template to use (e.g., "act", "plan", "review")
        #[arg(short, long)]
        intent: Option<String>,

        /// Tag assignment with an external identifier (e.g., bead ID)
        #[arg(long)]
        id: Option<String>,

        /// Resume previous Claude session (uses --resume if session_id exists)
        #[arg(long)]
        resume: bool,

        /// Segment to use (defaults to current directory's segment)
        #[arg(short, long)]
        segment: Option<String>,

        /// Skip permission prompts (passes --dangerously-skip-permissions to claude)
        #[arg(long)]
        danger: bool,
    },

    /// Navigate to a workspace or run a command in it
    #[command(visible_alias = "g")]
    Run {
        /// Workspace name (e.g. "one", "two")
        workspace: Option<String>,

        /// Run a specific hook (setup or destroy)
        #[arg(long)]
        hook: Option<HookArg>,

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

    /// Initialize .toren.kdl in the current repository
    Init {
        /// Add .toren.kdl to .git/info/exclude instead of committing it
        #[arg(long)]
        stealth: bool,
    },

    /// Remove assignment record without workspace cleanup
    Dismiss {
        /// Workspace or external ID reference
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
            // Try loading config for alias check (silently ignore config errors)
            if let Ok(config) = Config::load() {
                if let Some(template) = config.aliases.get(subcmd) {
                    let alias_args = &raw_args[subcmd_idx + 1..];
                    let expanded = toren_lib::alias::expand_alias(template, alias_args);

                    // Set up logging if -v flags were used
                    let verbose_count: usize = raw_args[1..subcmd_idx]
                        .iter()
                        .map(|a| match a.as_str() {
                            "-vvv" => 3,
                            "-vv" => 2,
                            "-v" | "--verbose" => 1,
                            _ => 0,
                        })
                        .sum();
                    if verbose_count > 0 {
                        let log_level = match verbose_count {
                            1 => tracing::Level::DEBUG,
                            _ => tracing::Level::TRACE,
                        };
                        tracing_subscriber::fmt()
                            .with_max_level(log_level)
                            .with_target(false)
                            .with_timer(ShortTime)
                            .init();
                    }

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
        Commands::Cmd {
            workspace,
            prompt,
            intent,
            id,
            resume,
            segment,
            danger,
        } => cmd_cmd(
            &config,
            workspace,
            prompt,
            intent,
            id,
            resume,
            segment.as_deref(),
            danger,
        ),
        Commands::Run {
            workspace,
            hook,
            segment,
            cmd,
        } => cmd_run(&config, workspace, hook, segment.as_deref(), cmd),
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
            "Current directory is not under any segment root.\n\
             Configure segment roots in toren.toml:\n\
             [segments]\n\
             roots = [\"~/proj\"]"
        })
    }
}

/// Generate workspace name from ancillary number word.
fn workspace_name_for_number(n: u32) -> String {
    toren_lib::number_to_word(n).to_lowercase()
}

// ─── cmd ────────────────────────────────────────────────────────────────────

fn cmd_cmd(
    config: &Config,
    workspace: Option<String>,
    prompt: Option<String>,
    intent: Option<String>,
    external_id: Option<String>,
    resume: bool,
    segment_name: Option<&str>,
    danger: bool,
) -> Result<()> {
    let workspace_root = config
        .ancillary
        .workspace_root
        .clone()
        .context("workspace_root not configured in toren.toml")?;

    let workspace_mgr = WorkspaceManager::new(workspace_root, Some(config.local_domain.clone()));
    let segment_mgr = SegmentManager::new(config)?;
    let mut assignment_mgr = AssignmentManager::new()?;

    let segment = resolve_segment(&segment_mgr, segment_name)?;

    // Determine prompt text: -p flag > -i intent template > stdin (if piped) > $EDITOR
    let prompt_text = if let Some(ref p) = prompt {
        p.clone()
    } else if let Some(ref intent_name) = intent {
        let template = config
            .intents
            .get(intent_name)
            .with_context(|| format!("Unknown intent: {}", intent_name))?;

        // Build task context for template rendering
        let task_id = external_id.clone().unwrap_or_default();
        let task_title = external_id.clone().unwrap_or_default();
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
            }),
            vars: std::collections::HashMap::new(),
        };
        toren_lib::render_template(template, &ctx)?
    } else if !std::io::stdin().is_terminal() {
        // Read from stdin (piped input)
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        let trimmed = buf.trim().to_string();
        if trimmed.is_empty() {
            anyhow::bail!("Empty input from stdin. Provide -p, -i, or pipe a prompt.");
        }
        trimmed
    } else {
        // Open $EDITOR
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

        // Look up existing assignment for this workspace
        let ancillary_num = toren_lib::word_to_number(&ws_name_lower).unwrap_or(0);
        let ancillary_id_str = toren_lib::ancillary_id(&segment.name, ancillary_num);

        // Check for existing assignment
        let existing = assignment_mgr.get_active_for_ancillary(&ancillary_id_str).cloned();

        if resume {
            if let Some(ref assignment) = existing {
                if let Some(ref sid) = assignment.session_id {
                    eprintln!("Resuming session in {}", ws_path.display());
                    let mut cmd = Command::new("claude");
                    if danger {
                        cmd.arg("--dangerously-skip-permissions");
                    }
                    cmd.arg("--resume").arg(sid).current_dir(&ws_path);
                    let err = cmd.exec();
                    return Err(err).context("Failed to exec claude");
                }
            }
            eprintln!("No session to resume, starting new session");
        }

        // Reuse workspace — start claude
        eprintln!("Starting Claude session in {}\n", ws_path.display());
        let mut cmd = Command::new("claude");
        if danger {
            cmd.arg("--dangerously-skip-permissions");
        }
        cmd.arg(&prompt_text).current_dir(&ws_path);

        let err = cmd.exec();
        Err(err).context("Failed to exec claude")
    } else {
        // Create new workspace
        let existing_workspaces = workspace_mgr
            .list_workspaces(&segment.path)
            .unwrap_or_default();
        let ancillary_id_str = assignment_mgr.next_available_ancillary(
            &segment.name,
            config.ancillary.pool_size,
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
        let source = if external_id.is_some() {
            AssignmentSource::Reference
        } else {
            AssignmentSource::Prompt {
                original_prompt: prompt_text.clone(),
            }
        };

        let title: Option<String> = Some(
            prompt_text
                .lines()
                .next()
                .unwrap_or(&prompt_text)
                .chars()
                .take(80)
                .collect(),
        );

        assignment_mgr.create(
            &ancillary_id_str,
            external_id.as_deref(),
            source,
            &segment.name,
            ws_path.clone(),
            title,
            base_branch,
        )?;

        // Exec into claude
        eprintln!("Starting Claude session in {}\n", ws_path.display());
        let mut cmd = Command::new("claude");
        if danger {
            cmd.arg("--dangerously-skip-permissions");
        }
        cmd.arg(&prompt_text).current_dir(&ws_path);

        let err = cmd.exec();
        Err(err).context("Failed to exec claude")
    }
}

// ─── run ────────────────────────────────────────────────────────────────────

fn cmd_run(
    config: &Config,
    workspace: Option<String>,
    hook: Option<HookArg>,
    segment_name: Option<&str>,
    cmd: Vec<String>,
) -> Result<()> {
    // Hook mode: run setup/destroy from cwd
    if let Some(hook_type) = hook {
        let workspace_root = config
            .ancillary
            .workspace_root
            .clone()
            .context("workspace_root not configured")?;
        let workspace_mgr = WorkspaceManager::new(workspace_root, Some(config.local_domain.clone()));

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

    let workspace_root = config
        .ancillary
        .workspace_root
        .clone()
        .context("workspace_root not configured in toren.toml")?;

    let workspace_mgr = WorkspaceManager::new(workspace_root, Some(config.local_domain.clone()));
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
            config.ancillary.pool_size,
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

        assignment_mgr.create(
            &ancillary_id_str,
            None,
            AssignmentSource::Prompt {
                original_prompt: "(interactive shell)".to_string(),
            },
            &segment.name,
            ws_path.clone(),
            None,
            base_branch,
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
    let workspace_root = config.ancillary.workspace_root.clone();
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

    // Fixed column widths: workspace(10) + ext_id(15) + activity(6) + spaces(3)
    let fixed_width: usize = 10 + 15 + 6 + 3;

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
        let ancillary_name = assignment
            .ancillary_id
            .split_whitespace()
            .last()
            .unwrap_or(&assignment.ancillary_id);

        let ws_text = if has_changes {
            format!("{:<10}", format!("{} *", ancillary_name))
        } else {
            format!("{:<10}", ancillary_name)
        };
        let ws_colored = if has_changes {
            ws_text.yellow()
        } else {
            ws_text.normal()
        };

        // External ID (if any)
        let ext_id_display = assignment
            .external_id
            .as_deref()
            .unwrap_or("-");

        let activity_text = format!("{:<6}", agent_activity);
        let activity_colored = if agent_activity == "busy" {
            activity_text.yellow()
        } else {
            activity_text.green()
        };

        if detail {
            // In detail mode, show title too
            let title_max = term_width.saturating_sub(fixed_width);
            let title = assignment
                .title
                .as_deref()
                .map(|t| truncate_title(t, title_max))
                .unwrap_or_else(|| "-".to_string());

            println!(
                "{} {:<15} {} {}",
                ws_colored, ext_id_display, activity_colored, title
            );
        } else {
            println!(
                "{} {:<15} {}",
                ws_colored, ext_id_display, activity_colored
            );
        }
    }

    // Detect orphaned workspace directories
    if let Some(ref ws_root) = workspace_root {
        let ws_mgr = WorkspaceManager::new(ws_root.clone(), Some(config.local_domain.clone()));
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
        if let Some(ref ext_id) = assignment.external_id {
            println!("  External ID:  {}", ext_id);
        }
        println!("  Segment:      {}", assignment.segment);
        println!("  Status:       {:?}", assignment.status);
        println!("  Source:       {:?}", assignment.source);
        println!("  Workspace:    {}", assignment.workspace_path.display());
        if let Some(ref title) = assignment.title {
            println!("  Title:        {}", title);
        }
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

        // Show bead info if external_id present
        if let Some(ref ext_id) = assignment.external_id {
            let seg_path = segment_mgr
                .find_by_name(&assignment.segment)
                .map(|s| s.path);
            if let Some(seg_path) = seg_path {
                println!("\nTask details:");
                let _ = Command::new("bd")
                    .args(["show", ext_id])
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
    let workspace_root = config
        .ancillary
        .workspace_root
        .clone()
        .context("workspace_root not configured")?;

    let segment_mgr = SegmentManager::new(config)?;
    let workspace_mgr = WorkspaceManager::new(workspace_root, Some(config.local_domain.clone()));
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
        &config.ancillary,
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
    let workspace_root = config
        .ancillary
        .workspace_root
        .clone()
        .context("workspace_root not configured in toren.toml")?;

    let segment_mgr = SegmentManager::new(config)?;
    let mut assignment_mgr = AssignmentManager::new()?;
    let ws_mgr = WorkspaceManager::new(workspace_root, Some(config.local_domain.clone()));

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
            assignment.external_id.as_deref().unwrap_or("-")
        );
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

    let config_path = cwd.join(".toren.kdl");
    if config_path.exists() {
        anyhow::bail!(".toren.kdl already exists. Remove it first to re-initialize.");
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
    std::fs::write(&config_path, &kdl).context("Failed to write .toren.kdl")?;
    println!("Created .toren.kdl with {} setup entries", total);

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
            if !existing.lines().any(|l| l.trim() == ".toren.kdl") {
                let mut content = existing;
                if !content.ends_with('\n') && !content.is_empty() {
                    content.push('\n');
                }
                content.push_str(".toren.kdl\n");
                std::fs::write(&exclude_path, content)
                    .context("Failed to update .git/info/exclude")?;
                println!("Added .toren.kdl to .git/info/exclude");
            }
        } else {
            println!("Warning: .git/info directory not found, --stealth had no effect");
        }
    }

    Ok(())
}

// ─── helpers ────────────────────────────────────────────────────────────────

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
        if parent.join(".toren.kdl").exists() {
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
        if parent.join(".toren.kdl").exists() {
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
