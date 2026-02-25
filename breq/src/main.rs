use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use colored::Colorize;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use toren_lib::{
    AssignmentManager, AssignmentRef, Config, Segment, SegmentManager,
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
#[command(about = "Spawn Claude ancillaries for bead-driven development")]
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
    /// Assign work to a Claude ancillary (from bead or prompt)
    #[command(visible_alias = "a")]
    Assign {
        /// Bead ID to assign (optional if using --prompt)
        bead: Option<String>,

        /// Create assignment from a prompt (auto-creates bead)
        #[arg(short, long, conflicts_with = "bead")]
        prompt: Option<String>,

        /// Title for prompt-based assignment (defaults to first line of prompt)
        #[arg(long, requires = "prompt")]
        title: Option<String>,

        /// Intent for handling the bead
        #[arg(short, long, default_value = "act")]
        intent: Intent,

        /// Segment to use (defaults to current directory's segment)
        #[arg(short, long)]
        segment: Option<String>,

        /// Skip permission prompts (passes --dangerously-skip-permissions to claude)
        #[arg(long)]
        danger: bool,
    },

    /// List assignments (defaults to current segment)
    List {
        /// List assignments from all segments
        #[arg(short, long)]
        all: bool,

        /// List assignments from a specific segment
        #[arg(short, long, conflicts_with = "all")]
        segment: Option<String>,
    },

    /// Show detailed assignment information
    Show {
        /// Bead ID or ancillary reference
        reference: String,
    },

    /// Continue work on an existing assignment (recreates workspace if missing)
    Resume {
        /// Bead ID or ancillary reference
        reference: String,

        /// Additional instructions
        #[arg(short, long)]
        instruction: Option<String>,

        /// Skip permission prompts
        #[arg(long)]
        danger: bool,
    },

    /// Discard workspace and optionally close bead
    Abort {
        /// Bead ID or ancillary reference
        reference: String,

        /// Also close the bead
        #[arg(long)]
        close: bool,

        /// Kill processes running in the workspace before cleanup
        #[arg(long)]
        kill: bool,
    },

    /// Complete work: cleanup workspace, close bead, print revision for integration
    Complete {
        /// Bead ID or ancillary reference
        reference: String,

        /// Also push the commits (jj git push -c <rev>)
        #[arg(long)]
        push: bool,

        /// Keep bead open instead of closing
        #[arg(long)]
        keep_open: bool,

        /// Kill processes running in the workspace before cleanup
        #[arg(long)]
        kill: bool,
    },

    /// Remove orphaned workspace directories (exist on disk but not tracked by jj)
    Cleanup {
        /// Segment to clean up (defaults to current directory's segment)
        #[arg(short, long)]
        segment: Option<String>,

        /// Clean up all segments
        #[arg(short, long, conflicts_with = "segment")]
        all: bool,
    },

    /// Workspace management commands
    #[command(visible_alias = "ws")]
    Workspace {
        #[command(subcommand)]
        command: WorkspaceCommands,
    },

    /// Navigate to a workspace directory or run a command in it
    #[command(visible_alias = "g")]
    Go {
        /// Workspace name (e.g. "one", "two") or ancillary reference
        workspace: String,

        /// Segment to use (defaults to current directory's segment)
        #[arg(short, long)]
        segment: Option<String>,

        /// Command to run in the workspace directory (after --)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        cmd: Vec<String>,
    },

    /// Setup/refresh Caddy reverse proxy routes for a workspace
    Proxy {
        /// Workspace name (e.g., "one", "two")
        workspace: String,

        /// Explicit port mapping (port:upstream), can be repeated
        #[arg(short, long = "port", value_name = "PORT:UPSTREAM")]
        p: Vec<String>,

        /// Segment to use (defaults to current directory's segment)
        #[arg(short, long)]
        segment: Option<String>,
    },

    /// Initialize .toren.kdl in the current repository
    Init {
        /// Add .toren.kdl to .git/info/exclude instead of committing it
        #[arg(long)]
        stealth: bool,
    },
}

#[derive(Subcommand)]
enum WorkspaceCommands {
    /// Run setup hooks for current workspace
    Setup,

    /// Run destroy hooks for current workspace
    Destroy,
}

#[derive(Clone, Copy, ValueEnum, Default)]
enum Intent {
    /// Execute: implement feature, fix bug, complete task
    #[default]
    Act,
    /// Design: propose approach, investigate, explore options
    Plan,
    /// Verify: assess completeness, check for issues
    Review,
}

impl Intent {
    fn template<'a>(&self, intents: &'a toren_lib::IntentsConfig) -> &'a str {
        match self {
            Intent::Act => &intents.act,
            Intent::Plan => &intents.plan,
            Intent::Review => &intents.review,
        }
    }
}

fn main() -> Result<()> {
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
        Commands::Assign {
            bead,
            prompt,
            title,
            intent,
            segment,
            danger,
        } => cmd_assign(
            &config,
            bead,
            prompt,
            title,
            intent,
            segment.as_deref(),
            danger,
        ),
        Commands::List { all, segment } => cmd_list(&config, all, segment),
        Commands::Show { reference } => cmd_show(&config, &reference),
        Commands::Resume {
            reference,
            instruction,
            danger,
        } => cmd_resume(&config, &reference, instruction.as_deref(), danger),
        Commands::Abort { reference, close, kill } => cmd_abort(&config, &reference, close, kill),
        Commands::Complete {
            reference,
            push,
            keep_open,
            kill,
        } => cmd_complete(&config, &reference, push, keep_open, kill),
        Commands::Cleanup { segment, all } => cmd_cleanup(&config, all, segment),
        Commands::Go {
            workspace,
            segment,
            cmd,
        } => cmd_go(&config, &workspace, segment.as_deref(), cmd),
        Commands::Workspace { command } => match command {
            WorkspaceCommands::Setup => cmd_ws_setup(&config),
            WorkspaceCommands::Destroy => cmd_ws_destroy(&config),
        },
        Commands::Proxy {
            workspace,
            p,
            segment,
        } => cmd_proxy(&config, &workspace, p, segment.as_deref()),
        Commands::Init { stealth } => cmd_init(stealth),
    }
}

/// Helper to find segment from current directory or specified name.
/// Segments are resolved dynamically - any directory under a segment root is valid.
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

/// Generate workspace name for a bead assignment.
/// Uses the ancillary number word (e.g. "one", "two") so workspaces can be reused.
fn workspace_name_for_assignment(ancillary_number: u32) -> String {
    toren_lib::number_to_word(ancillary_number).to_lowercase()
}

fn cmd_assign(
    config: &Config,
    bead: Option<String>,
    prompt: Option<String>,
    title: Option<String>,
    intent: Intent,
    segment_name: Option<&str>,
    danger: bool,
) -> Result<()> {
    let workspace_root = config
        .ancillary
        .workspace_root
        .clone()
        .context("workspace_root not configured in toren.toml")?;

    let workspace_mgr = WorkspaceManager::new(workspace_root);
    let segment_mgr = SegmentManager::new(config)?;
    let mut assignment_mgr = AssignmentManager::new()?;

    let segment = resolve_segment(&segment_mgr, segment_name)?;

    // Determine bead ID and title - either from existing bead or create from prompt
    let (bead_id, task_title, original_prompt) = if let Some(ref prompt_text) = prompt {
        // Create bead from prompt
        let bead_title = title.unwrap_or_else(|| {
            // Use first line of prompt as title, truncated
            prompt_text
                .lines()
                .next()
                .unwrap_or(prompt_text)
                .chars()
                .take(80)
                .collect()
        });

        println!("Creating bead from prompt: {}", bead_title);
        let new_bead_id = toren_lib::tasks::beads::create_and_claim_bead(
            &bead_title,
            Some(prompt_text),
            "claude",
            &segment.path,
        )?;
        println!("Created bead: {}", new_bead_id);

        (new_bead_id, bead_title, Some(prompt_text.clone()))
    } else if let Some(bead_id) = bead {
        // Use existing bead
        let task = toren_lib::tasks::fetch_task(&bead_id, &segment.path)?;
        println!("Assigning: {} - {}", task.id, task.title);

        toren_lib::tasks::beads::claim_bead(&bead_id, "claude", &segment.path)?;
        println!("Claimed bead for claude");

        (bead_id, task.title, None)
    } else {
        anyhow::bail!("Either <BEAD> or --prompt must be specified");
    };

    // Find next available ancillary, accounting for existing workspaces on disk
    let existing_workspaces = workspace_mgr
        .list_workspaces(&segment.path)
        .unwrap_or_default();
    let ancillary_id = assignment_mgr.next_available_ancillary(
        &segment.name,
        config.ancillary.pool_size,
        &existing_workspaces,
    );
    let ancillary_num = toren_lib::ancillary_number(&ancillary_id).unwrap_or(1);
    println!("Ancillary: {}", ancillary_id);

    // Generate workspace name from ancillary number word
    let ws_name = workspace_name_for_assignment(ancillary_num);

    // Create workspace and run setup hooks
    let (ws_path, _setup_result) = workspace_mgr.create_workspace_with_setup(
        &segment.path,
        &segment.name,
        &ws_name,
        ancillary_num,
        Some(&config.proxy),
    )?;
    println!("Workspace: {}", ws_path.display());

    // Record assignment
    if let Some(ref prompt_text) = original_prompt {
        assignment_mgr.create_from_prompt(
            &ancillary_id,
            &bead_id,
            prompt_text,
            &segment.name,
            ws_path.clone(),
            Some(task_title.clone()),
        )?;
    } else {
        assignment_mgr.create_from_bead(
            &ancillary_id,
            &bead_id,
            &segment.name,
            ws_path.clone(),
            Some(task_title.clone()),
        )?;
    }

    // Build prompt for Claude using config template + minijinja rendering
    let template = intent.template(&config.intents);
    let ctx = toren_lib::WorkspaceContext {
        ws: toren_lib::WorkspaceInfo {
            name: ws_name.clone(),
            num: ancillary_num,
            path: ws_path.display().to_string(),
        },
        repo: toren_lib::RepoInfo {
            root: segment.path.display().to_string(),
            name: segment.name.clone(),
        },
        task: Some(toren_lib::TaskInfo {
            id: bead_id.clone(),
            title: task_title.clone(),
        }),
        vars: std::collections::HashMap::new(),
        config: None,
    };
    let claude_prompt = toren_lib::render_template(template, &ctx)?;

    // Exec into claude
    println!("Starting Claude session in {}\n", ws_path.display());

    let mut cmd = Command::new("claude");
    if danger {
        cmd.arg("--dangerously-skip-permissions");
    }
    cmd.arg(&claude_prompt).current_dir(&ws_path);

    let err = cmd.exec();
    Err(err).context("Failed to exec claude")
}

fn cmd_list(config: &Config, all_segments: bool, segment_name: Option<String>) -> Result<()> {
    let workspace_root = config.ancillary.workspace_root.clone();
    let segment_mgr = SegmentManager::new(config)?;
    let mut assignment_mgr = AssignmentManager::new()?;

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
        // Default: current segment
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

    // Fixed column widths: workspace(10) + bead_id(15) + activity(6) + assignee(10) + spaces(4)
    let fixed_width: usize = 10 + 15 + 6 + 10 + 4;
    let narrow = term_width < fixed_width + 15;

    for assignment in &assignments {
        // Fetch composite status signals
        let segment_path = segment_mgr
            .find_by_name(&assignment.segment)
            .map(|s| s.path.clone());

        // 1. Bead status + assignee (from bd)
        let bead_info = segment_path
            .as_ref()
            .and_then(|seg_path| {
                toren_lib::tasks::beads::fetch_bead_info(&assignment.bead_id, seg_path).ok()
            });

        // 2. Agent activity (from Claude session log last-entry-type)
        let agent_activity = toren_lib::composite_status::detect_agent_activity(
            &assignment.workspace_path,
        );

        // 3. Has changes (from jj workspace)
        let has_changes = toren_lib::composite_status::workspace_has_changes(&assignment.workspace_path);

        // 4. Assignee
        let assignee = bead_info
            .as_ref()
            .map(|b| b.assignee.as_str())
            .unwrap_or("");
        let assignee_display = if assignee.is_empty() {
            String::new()
        } else {
            format!("@{}", assignee)
        };

        // 5. Title — use available terminal width
        let title_max = if narrow {
            term_width.saturating_sub(4) // indented on next line
        } else {
            term_width.saturating_sub(fixed_width)
        };
        let title = bead_info
            .as_ref()
            .map(|b| truncate_title(&b.title, title_max))
            .or_else(|| {
                segment_path
                    .as_ref()
                    .and_then(|seg| toren_lib::tasks::fetch_task(&assignment.bead_id, seg).ok())
                    .map(|task| truncate_title(&task.title, title_max))
            })
            .unwrap_or_else(|| "-".to_string());

        // 6. Workspace name — extract short name, mark dirty with *
        let ancillary_name = assignment
            .ancillary_id
            .split_whitespace()
            .last()
            .unwrap_or(&assignment.ancillary_id);

        // Pad plain text first, then colorize (ANSI codes break format width)
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

        let activity_text = format!("{:<6}", agent_activity);
        let activity_colored = if agent_activity == "busy" {
            activity_text.yellow()
        } else {
            activity_text.green()
        };

        if narrow {
            println!(
                "{} {:<15} {} {:<10}",
                ws_colored, assignment.bead_id, activity_colored, assignee_display
            );
            println!("    {}", title);
        } else {
            println!(
                "{} {:<15} {} {:<10} {}",
                ws_colored, assignment.bead_id, activity_colored, assignee_display, title
            );
        }
    }

    // Detect orphaned workspace directories
    if let Some(ref ws_root) = workspace_root {
        let ws_mgr = WorkspaceManager::new(ws_root.clone());
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

/// Find workspace directories that exist on disk but are not tracked by jj
/// and have no assignment record.
fn find_orphaned_workspaces(
    ws_mgr: &WorkspaceManager,
    segments: &[Segment],
    assignments: &[&toren_lib::Assignment],
) -> Vec<(String, String, std::path::PathBuf)> {
    let mut orphans = Vec::new();

    for segment in segments {
        // Get jj-tracked workspace names
        let jj_workspaces = ws_mgr.list_workspaces(&segment.path).unwrap_or_default();

        // Get assignment workspace paths for this segment
        let assigned_paths: std::collections::HashSet<_> = assignments
            .iter()
            .filter(|a| a.segment.to_lowercase() == segment.name.to_lowercase())
            .map(|a| a.workspace_path.clone())
            .collect();

        // Scan workspace directory for this segment
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

                // Skip if tracked by jj
                if jj_workspaces.contains(&ws_name) {
                    continue;
                }

                // Skip if has an assignment record
                if assigned_paths.contains(&path) {
                    continue;
                }

                // This is an orphaned directory
                orphans.push((segment.name.clone(), ws_name, path));
            }
        }
    }

    orphans
}

fn cmd_cleanup(config: &Config, all_segments: bool, segment_name: Option<String>) -> Result<()> {
    let workspace_root = config
        .ancillary
        .workspace_root
        .clone()
        .context("workspace_root not configured in toren.toml")?;

    let segment_mgr = SegmentManager::new(config)?;
    let mut assignment_mgr = AssignmentManager::new()?;
    let ws_mgr = WorkspaceManager::new(workspace_root);

    // Determine which segment(s) to clean up
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

fn truncate_title(title: &str, max_len: usize) -> String {
    if max_len < 4 {
        return title.chars().take(max_len).collect();
    }
    // Use char count for proper Unicode handling
    let char_count = title.chars().count();
    if char_count <= max_len {
        title.to_string()
    } else {
        let truncated: String = title.chars().take(max_len - 3).collect();
        format!("{}...", truncated)
    }
}

fn cmd_show(config: &Config, reference: &str) -> Result<()> {
    let segment_mgr = SegmentManager::new(config)?;
    let mut assignment_mgr = AssignmentManager::new()?;

    let segment = resolve_segment(&segment_mgr, None)?;
    let ref_ = AssignmentRef::parse(reference, &segment.name);
    let assignments = assignment_mgr.resolve(&ref_);

    if assignments.is_empty() {
        // Fall back to showing bead directly
        let bead_id = match &ref_ {
            AssignmentRef::Bead(id) => id.as_str(),
            AssignmentRef::Ancillary(_) => {
                anyhow::bail!("No assignment found for: {}", reference);
            }
        };

        let status = Command::new("bd")
            .args(["show", bead_id])
            .current_dir(&segment.path)
            .status()?;

        if !status.success() {
            anyhow::bail!("Failed to show bead");
        }
        return Ok(());
    }

    for assignment in assignments {
        println!("Assignment: {}", assignment.id);
        println!("  Ancillary: {}", assignment.ancillary_id);
        println!("  Bead:      {}", assignment.bead_id);
        println!("  Segment:   {}", assignment.segment);
        println!("  Status:    {:?}", assignment.status);
        println!("  Source:    {:?}", assignment.source);
        println!("  Workspace: {}", assignment.workspace_path.display());
        println!("  Created:   {}", assignment.created_at);
        println!("  Updated:   {}", assignment.updated_at);

        // Show bead info
        println!("\nBead details:");
        let _ = Command::new("bd")
            .args(["show", &assignment.bead_id])
            .current_dir(
                assignment
                    .workspace_path
                    .parent()
                    .unwrap_or(&assignment.workspace_path),
            )
            .status();

        // Show workspace info if exists
        if assignment.workspace_path.exists() {
            println!("\nRecent changes:");
            let _ = Command::new("jj")
                .args(["log", "-n", "5"])
                .current_dir(&assignment.workspace_path)
                .status();
        } else {
            println!("\n(Workspace not found - use `breq resume` to recreate)");
        }

        println!();
    }

    Ok(())
}

fn cmd_resume(
    config: &Config,
    reference: &str,
    instruction: Option<&str>,
    danger: bool,
) -> Result<()> {
    let workspace_root = config
        .ancillary
        .workspace_root
        .clone()
        .context("workspace_root not configured")?;

    let segment_mgr = SegmentManager::new(config)?;
    let workspace_mgr = WorkspaceManager::new(workspace_root);
    let mut assignment_mgr = AssignmentManager::new()?;

    let segment = resolve_segment(&segment_mgr, None)?;
    let ref_ = AssignmentRef::parse(reference, &segment.name);

    // Find assignment (all assignments are active — terminal actions remove them)
    let assignment = {
        let assignments = assignment_mgr.resolve(&ref_);

        if assignments.is_empty() {
            anyhow::bail!("No assignment found for: {}", reference);
        }

        if assignments.len() > 1 {
            println!("Multiple assignments found:");
            for a in &assignments {
                println!("  {} -> {}", a.ancillary_id, a.bead_id);
            }
            anyhow::bail!("Please specify a unique ancillary or bead");
        }

        assignments[0].clone()
    };

    // Use shared resume logic
    let opts = toren_lib::ResumeOptions {
        instruction,
        segment_path: &segment.path,
        segment_name: &segment.name,
        proxy_config: Some(&config.proxy),
    };

    let result =
        toren_lib::prepare_resume(&assignment, &mut assignment_mgr, &workspace_mgr, &opts)?;

    if result.workspace_recreated {
        println!(
            "Workspace recreated: {}",
            assignment.workspace_path.display()
        );
    }

    println!(
        "Resuming session in workspace: {}\n",
        assignment.workspace_path.display()
    );

    // Check if we have a session_id for --resume handoff
    let session_id = assignment_mgr
        .get(&assignment.id)
        .and_then(|a| a.session_id.clone());

    let mut cmd = Command::new("claude");
    if danger {
        cmd.arg("--dangerously-skip-permissions");
    }
    if let Some(sid) = session_id {
        cmd.arg("--resume").arg(&sid);
    } else {
        cmd.arg(&result.prompt);
    }
    cmd.current_dir(&assignment.workspace_path);

    let err = cmd.exec();
    Err(err).context("Failed to exec claude")
}

fn cmd_abort(config: &Config, reference: &str, close: bool, kill: bool) -> Result<()> {
    let workspace_root = config
        .ancillary
        .workspace_root
        .clone()
        .context("workspace_root not configured")?;

    let segment_mgr = SegmentManager::new(config)?;
    let workspace_mgr = WorkspaceManager::new(workspace_root);
    let mut assignment_mgr = AssignmentManager::new()?;

    let segment = resolve_segment(&segment_mgr, None)?;
    let ref_ = AssignmentRef::parse(reference, &segment.name);

    // Get all assignments (active or not) to abort
    let assignments: Vec<_> = assignment_mgr
        .resolve(&ref_)
        .iter()
        .map(|a| (*a).clone())
        .collect();

    if assignments.is_empty() {
        // No assignment found - handle as bead reference for cleanup
        let bead_id = match &ref_ {
            AssignmentRef::Bead(id) => id.clone(),
            AssignmentRef::Ancillary(anc) => {
                // Try to find workspace by ancillary name
                let ws_name = anc.split_whitespace().last().unwrap_or(anc).to_lowercase();
                let ws_path = workspace_mgr.workspace_path(&segment.name, &ws_name);
                if ws_path.exists() {
                    println!("Cleaning up orphaned workspace: {}", ws_path.display());
                    workspace_mgr.cleanup_workspace(&segment.path, &segment.name, &ws_name, None)?;
                    println!("Workspace removed.");
                } else {
                    println!("No assignment or workspace found for: {}", reference);
                }
                return Ok(());
            }
        };

        // Try to cleanup workspace if it exists (orphaned workspace case)
        let _ = workspace_mgr.cleanup_workspace(&segment.path, &segment.name, &bead_id, None);

        if close {
            println!("Closing bead {}...", bead_id);
            toren_lib::tasks::beads::update_bead_status(&bead_id, "closed", &segment.path)?;
            info!("Bead closed.");
        } else {
            // Unassign and reopen
            let _ = toren_lib::tasks::beads::update_bead_assignee(&bead_id, "", &segment.path);
            toren_lib::tasks::beads::update_bead_status(&bead_id, "open", &segment.path)?;
            println!("Bead {} unassigned and returned to open.", bead_id);
        }
        return Ok(());
    }

    // Process each assignment using shared abort logic
    let opts = toren_lib::AbortOptions {
        close_bead: close,
        segment_path: &segment.path,
        proxy_config: Some(&config.proxy),
        kill,
    };

    for assignment in &assignments {
        println!(
            "Aborting: {} -> {}",
            assignment.ancillary_id, assignment.bead_id
        );

        toren_lib::abort_assignment(assignment, &mut assignment_mgr, &workspace_mgr, &opts)?;

        if close {
            println!("Bead {} closed.", assignment.bead_id);
        } else {
            println!(
                "Bead {} unassigned and returned to open.",
                assignment.bead_id
            );
        }
    }

    Ok(())
}

fn cmd_complete(config: &Config, reference: &str, push: bool, keep_open: bool, kill: bool) -> Result<()> {
    let workspace_root = config
        .ancillary
        .workspace_root
        .clone()
        .context("workspace_root not configured")?;

    let segment_mgr = SegmentManager::new(config)?;
    let workspace_mgr = WorkspaceManager::new(workspace_root);
    let mut assignment_mgr = AssignmentManager::new()?;

    let segment = resolve_segment(&segment_mgr, None)?;
    let ref_ = AssignmentRef::parse(reference, &segment.name);

    // Find the assignment
    let assignment = {
        let assignments = assignment_mgr.resolve_active(&ref_);

        if assignments.is_empty() {
            // No assignment found - check if this is a bead reference
            let bead_id = match &ref_ {
                AssignmentRef::Bead(id) => id.clone(),
                AssignmentRef::Ancillary(_) => {
                    anyhow::bail!("No active assignment found for: {}", reference);
                }
            };

            // No assignment, just close the bead if requested
            if !keep_open {
                println!("No assignment found, closing bead {}...", bead_id);
                toren_lib::tasks::beads::update_bead_status(&bead_id, "closed", &segment.path)?;
                info!("Bead closed.");
            } else {
                println!("No assignment found for bead {}.", bead_id);
            }
            return Ok(());
        }

        if assignments.len() > 1 {
            println!("Multiple active assignments found:");
            for a in &assignments {
                println!("  {} -> {}", a.ancillary_id, a.bead_id);
            }
            anyhow::bail!("Please specify a unique reference");
        }

        assignments[0].clone()
    };

    println!(
        "Completing: {} -> {}",
        assignment.ancillary_id, assignment.bead_id
    );

    // Show changes before cleanup (interactive output for CLI)
    if assignment.workspace_path.exists() {
        println!("\nChanges:");
        let output = Command::new("jj")
            .args(["log", "-r", "@"])
            .current_dir(&assignment.workspace_path)
            .output();

        if let Ok(output) = output {
            if !output.stdout.is_empty() {
                print!("{}", String::from_utf8_lossy(&output.stdout));
            }
            let stderr = String::from_utf8_lossy(&output.stderr);
            for line in stderr.lines() {
                if !line.contains("working copy is stale")
                    && !line.contains("workspace update-stale")
                {
                    eprintln!("{}", line);
                }
            }
        }
    } else {
        println!("(Workspace already cleaned up)");
    }

    // Use shared complete logic
    let opts = toren_lib::CompleteOptions {
        push,
        keep_open,
        segment_path: &segment.path,
        proxy_config: Some(&config.proxy),
        kill,
    };

    let result =
        toren_lib::complete_assignment(&assignment, &mut assignment_mgr, &workspace_mgr, &opts)?;

    if !keep_open {
        println!("Bead closed.");
    }

    // Print integration instructions
    if let Some(rev) = result.revision {
        if !result.pushed {
            println!("\nCommit preserved at: {}", &rev[..12.min(rev.len())]);
            println!(
                "To integrate: jj rebase -r {} -d main",
                &rev[..12.min(rev.len())]
            );
        } else {
            println!("Pushed.");
        }
    }

    Ok(())
}

/// Detect workspace context from current directory
fn detect_workspace_context() -> Result<(std::path::PathBuf, std::path::PathBuf, String)> {
    let cwd = std::env::current_dir()?;

    // Check if we're in a jj workspace
    if !cwd.join(".jj").exists() {
        anyhow::bail!("Not in a jj workspace. Run this command from within a workspace directory.");
    }

    // Get the workspace name from jj
    let output = Command::new("jj")
        .args(["workspace", "root"])
        .current_dir(&cwd)
        .output()
        .context("Failed to run jj workspace root")?;

    if !output.status.success() {
        anyhow::bail!("Failed to determine workspace root");
    }

    let workspace_path =
        std::path::PathBuf::from(String::from_utf8_lossy(&output.stdout).trim().to_string());

    // Extract workspace name from path (last component)
    let workspace_name = workspace_path
        .file_name()
        .and_then(|n| n.to_str())
        .context("Invalid workspace path")?
        .to_string();

    // Find the repo root (segment path) - look for .toren.kdl or use jj root
    // First, try to find .toren.kdl by walking up from workspace
    let mut segment_path = None;
    let mut check_path = workspace_path.parent();
    while let Some(parent) = check_path {
        if parent.join(".toren.kdl").exists() {
            segment_path = Some(parent.to_path_buf());
            break;
        }
        // Also check if this is the repo root (has .jj and is not a workspace)
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

fn cmd_go(
    config: &Config,
    workspace: &str,
    segment_name: Option<&str>,
    cmd: Vec<String>,
) -> Result<()> {
    let workspace_root = config
        .ancillary
        .workspace_root
        .clone()
        .context("workspace_root not configured in toren.toml")?;

    let workspace_mgr = WorkspaceManager::new(workspace_root);
    let segment_mgr = SegmentManager::new(config)?;
    let segment = resolve_segment(&segment_mgr, segment_name)?;

    // Resolve workspace name: could be a word like "one" or an ancillary reference
    let ws_name = workspace.to_lowercase();
    let ws_path = workspace_mgr.workspace_path(&segment.name, &ws_name);

    if !ws_path.exists() {
        anyhow::bail!("Workspace '{}' not found at {}", ws_name, ws_path.display());
    }

    let (program, args): (String, Vec<String>) = if cmd.is_empty() {
        // No command: spawn an interactive shell in the workspace
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
}

fn cmd_proxy(
    config: &Config,
    workspace: &str,
    port_mappings: Vec<String>,
    segment_name: Option<&str>,
) -> Result<()> {
    let segment_mgr = SegmentManager::new(config)?;
    let segment = resolve_segment(&segment_mgr, segment_name)?;
    let ws_name = workspace.to_lowercase();

    let daemon_url = format!(
        "http://{}:{}/api/workspaces/proxy",
        config.host(),
        config.port()
    );

    let body = serde_json::json!({
        "segment": segment.name,
        "workspace": ws_name,
        "port_mappings": port_mappings,
    });

    println!("Refreshing proxy routes for workspace '{}'...", ws_name);

    let agent = ureq::Agent::new_with_config(
        ureq::config::Config::builder()
            .http_status_as_error(false)
            .build(),
    );

    let resp = agent
        .post(&daemon_url)
        .send_json(&body)
        .with_context(|| {
            format!(
                "Failed to connect to daemon at {} (is the daemon running?)",
                daemon_url
            )
        })?;

    let status = resp.status();
    let resp_body: serde_json::Value = resp
        .into_body()
        .read_json()
        .context("Failed to read daemon response")?;

    if status != 200 {
        let error = resp_body["error"]
            .as_str()
            .unwrap_or("unknown error");
        anyhow::bail!("Daemon returned HTTP {}: {}", status, error);
    }

    let count = resp_body["count"].as_u64().unwrap_or(0);
    if let Some(directives) = resp_body["directives"].as_array() {
        for d in directives {
            let host = d["host"].as_str().unwrap_or("?");
            let upstream = d["upstream"].as_str().unwrap_or("?");
            let port = d["port"].as_u64().unwrap_or(0);
            println!("  {} -> {} on :{}", host, upstream, port);
        }
    }
    println!("Registered {} proxy route(s).", count);
    Ok(())
}

fn cmd_ws_setup(config: &Config) -> Result<()> {
    let workspace_root = config
        .ancillary
        .workspace_root
        .clone()
        .context("workspace_root not configured")?;

    let workspace_mgr = WorkspaceManager::new(workspace_root);

    let (segment_path, workspace_path, workspace_name) = detect_workspace_context()?;
    // Infer ancillary number from workspace name (e.g., "one" -> 1)
    let ancillary_num = toren_lib::word_to_number(&workspace_name);

    println!(
        "Running setup for workspace '{}' in {}",
        workspace_name,
        workspace_path.display()
    );

    workspace_mgr.run_setup(
        &segment_path,
        &workspace_path,
        &workspace_name,
        ancillary_num.unwrap_or(0),
        Some(&config.proxy),
    )?;

    println!("Setup complete.");
    Ok(())
}

fn cmd_init(stealth: bool) -> Result<()> {
    let cwd = std::env::current_dir()?;

    // Must be in a jj repo
    if !cwd.join(".jj").exists() {
        anyhow::bail!("Not a jujutsu repository. breq init must be run from a jj repo root.");
    }

    // Must be at the workspace root (jj root == cwd)
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

    let config_path = cwd.join(".toren.kdl");
    if config_path.exists() {
        anyhow::bail!(".toren.kdl already exists. Remove it first to re-initialize.");
    }

    // Collect setup actions
    let mut copy_entries: Vec<String> = Vec::new();
    let mut share_entries: Vec<String> = Vec::new();

    // Check for .beads directory
    if cwd.join(".beads").exists() {
        // If .beads is tracked by VCS, skip it - workspaces will get it from the repo
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

    // Discover build artifact directories from .gitignore
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

            // Check if this gitignored entry matches a well-known artifact dir
            for artifact in &well_known_artifacts {
                if line == *artifact || line.ends_with(&format!("/{}", artifact)) {
                    // Check if the directory actually exists in the repo
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

    // Also check for well-known artifacts that exist even if not in .gitignore
    // (they might be using nested gitignores or global gitignore)
    for artifact in &well_known_artifacts {
        let artifact_path = cwd.join(artifact);
        if artifact_path.is_dir() {
            let entry = artifact.to_string();
            if !copy_entries.contains(&entry) {
                copy_entries.push(entry);
            }
        }
    }

    // Generate .toren.kdl content
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

    // Stealth mode: add to .git/info/exclude
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

fn cmd_ws_destroy(config: &Config) -> Result<()> {
    let workspace_root = config
        .ancillary
        .workspace_root
        .clone()
        .context("workspace_root not configured")?;

    let workspace_mgr = WorkspaceManager::new(workspace_root);

    let (segment_path, workspace_path, workspace_name) = detect_workspace_context()?;

    println!(
        "Running destroy for workspace '{}' in {}",
        workspace_name,
        workspace_path.display()
    );

    workspace_mgr.run_destroy(&segment_path, &workspace_path, &workspace_name, Some(&config.proxy))?;

    println!("Destroy complete.");
    Ok(())
}
