use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use std::os::unix::process::CommandExt;
use std::process::Command;
use toren_lib::{
    AssignmentManager, AssignmentRef, AssignmentStatus, Config, Segment, SegmentManager,
    WorkspaceManager,
};

#[derive(Parser)]
#[command(name = "breq")]
#[command(about = "Spawn Claude ancillaries for bead-driven development")]
struct Cli {
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

    /// List all assignments
    List {
        /// Show all assignments (including completed/aborted)
        #[arg(long)]
        all: bool,
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
    },

    /// Accept work and push changes
    Approve {
        /// Bead ID or ancillary reference
        reference: String,
    },

    /// Free up an ancillary without touching bead/workspace
    Dismiss {
        /// Bead ID or ancillary reference
        reference: String,
    },
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
    fn prompt_template(&self, bead_id: &str, title: &str) -> String {
        match self {
            Intent::Act => format!(
                "Implement bead {bead_id}: {title}\n\n\
                Complete the task as specified. When done, summarize changes in a bead comment."
            ),
            Intent::Plan => format!(
                "Design an approach for bead {bead_id}: {title}\n\n\
                Investigate the codebase, explore options, and propose a design. \
                Update the bead's design field with your proposal."
            ),
            Intent::Review => format!(
                "Review the implementation of bead {bead_id}: {title}\n\n\
                Verify completeness, check for issues, and assess confidence. \
                Add review comments to the bead."
            ),
        }
    }
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Assign {
            bead,
            prompt,
            title,
            intent,
            segment,
            danger,
        } => cmd_assign(bead, prompt, title, intent, segment.as_deref(), danger),
        Commands::List { all } => cmd_list(all),
        Commands::Show { reference } => cmd_show(&reference),
        Commands::Resume {
            reference,
            instruction,
            danger,
        } => cmd_resume(&reference, instruction.as_deref(), danger),
        Commands::Abort { reference, close } => cmd_abort(&reference, close),
        Commands::Approve { reference } => cmd_approve(&reference),
        Commands::Dismiss { reference } => cmd_dismiss(&reference),
    }
}

/// Helper to find segment from current directory or specified name
fn resolve_segment(segment_mgr: &SegmentManager, segment_name: Option<&str>) -> Result<Segment> {
    if let Some(name) = segment_name {
        segment_mgr
            .get(name)
            .with_context(|| format!("Segment '{}' not found", name))?
            .clone()
            .pipe(Ok)
    } else {
        let cwd = std::env::current_dir()?;
        segment_mgr
            .list()
            .iter()
            .find(|s| cwd.starts_with(&s.path))
            .cloned()
            .context("Could not determine segment from current directory. Use --segment.")
    }
}

/// Helper trait to allow chaining with Ok()
trait Pipe: Sized {
    fn pipe<F, R>(self, f: F) -> R
    where
        F: FnOnce(Self) -> R,
    {
        f(self)
    }
}
impl<T> Pipe for T {}

/// Generate workspace name for a bead assignment.
/// If multiple ancillaries work on the same bead, suffix with ancillary number.
fn workspace_name_for_assignment(
    assignment_mgr: &AssignmentManager,
    bead_id: &str,
    ancillary_number: u32,
) -> String {
    let existing = assignment_mgr.get_by_bead(bead_id);
    let active_count = existing
        .iter()
        .filter(|a| matches!(a.status, AssignmentStatus::Pending | AssignmentStatus::Active))
        .count();

    if active_count == 0 {
        bead_id.to_string()
    } else {
        format!("{}-{}", bead_id, ancillary_number)
    }
}

fn cmd_assign(
    bead: Option<String>,
    prompt: Option<String>,
    title: Option<String>,
    intent: Intent,
    segment_name: Option<&str>,
    danger: bool,
) -> Result<()> {
    let config = Config::load()?;

    let workspace_root = config
        .ancillary
        .workspace_root
        .clone()
        .context("workspace_root not configured in toren.toml")?;

    let workspace_mgr = WorkspaceManager::new(workspace_root);
    let segment_mgr = SegmentManager::new(&config)?;
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

    // Find next available ancillary
    let ancillary_id =
        assignment_mgr.next_available_ancillary(&segment.name, config.ancillary.pool_size);
    let ancillary_num = toren_lib::ancillary_number(&ancillary_id).unwrap_or(1);
    println!("Ancillary: {}", ancillary_id);

    // Generate workspace name
    let ws_name = workspace_name_for_assignment(&assignment_mgr, &bead_id, ancillary_num);

    // Create workspace
    let ws_path = workspace_mgr.create_workspace(&segment.path, &segment.name, &ws_name)?;
    println!("Workspace: {}", ws_path.display());

    // Record assignment
    if let Some(ref prompt_text) = original_prompt {
        assignment_mgr.create_from_prompt(
            &ancillary_id,
            &bead_id,
            prompt_text,
            &segment.name,
            ws_path.clone(),
        )?;
    } else {
        assignment_mgr.create_from_bead(&ancillary_id, &bead_id, &segment.name, ws_path.clone())?;
    }

    // Build prompt for Claude
    let claude_prompt = intent.prompt_template(&bead_id, &task_title);

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

fn cmd_list(show_all: bool) -> Result<()> {
    let assignment_mgr = AssignmentManager::new()?;

    let assignments = if show_all {
        assignment_mgr.list()
    } else {
        assignment_mgr.list_active()
    };

    if assignments.is_empty() {
        if show_all {
            println!("No assignments.");
        } else {
            println!("No active assignments. Use --all to see completed/aborted.");
        }
        return Ok(());
    }

    println!(
        "{:<18} {:<15} {:<12} WORKSPACE",
        "ANCILLARY", "BEAD", "STATUS"
    );
    println!("{}", "-".repeat(70));

    for assignment in assignments {
        let ws_exists = assignment.workspace_path.exists();

        let status_str = match assignment.status {
            AssignmentStatus::Pending => {
                if ws_exists {
                    "pending"
                } else {
                    "ws-missing"
                }
            }
            AssignmentStatus::Active => {
                if ws_exists {
                    "active"
                } else {
                    "ws-missing"
                }
            }
            AssignmentStatus::Completed => "completed",
            AssignmentStatus::Aborted => "aborted",
        };

        let ws_display = if ws_exists {
            assignment.workspace_path.display().to_string()
        } else {
            "-".to_string()
        };

        println!(
            "{:<18} {:<15} {:<12} {}",
            assignment.ancillary_id, assignment.bead_id, status_str, ws_display
        );
    }

    Ok(())
}

fn cmd_show(reference: &str) -> Result<()> {
    let config = Config::load()?;
    let segment_mgr = SegmentManager::new(&config)?;
    let assignment_mgr = AssignmentManager::new()?;

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

fn cmd_resume(reference: &str, instruction: Option<&str>, danger: bool) -> Result<()> {
    let config = Config::load()?;

    let workspace_root = config
        .ancillary
        .workspace_root
        .clone()
        .context("workspace_root not configured")?;

    let segment_mgr = SegmentManager::new(&config)?;
    let workspace_mgr = WorkspaceManager::new(workspace_root);
    let mut assignment_mgr = AssignmentManager::new()?;

    let segment = resolve_segment(&segment_mgr, None)?;
    let ref_ = AssignmentRef::parse(reference, &segment.name);

    // Clone the assignment data we need before mutating assignment_mgr
    let assignment = {
        let assignments = assignment_mgr.resolve_active(&ref_);

        if assignments.is_empty() {
            anyhow::bail!("No active assignment found for: {}", reference);
        }

        if assignments.len() > 1 {
            println!("Multiple active assignments found:");
            for a in &assignments {
                println!("  {} -> {}", a.ancillary_id, a.bead_id);
            }
            anyhow::bail!("Please specify a unique ancillary or bead");
        }

        assignments[0].clone()
    };

    let ws_path = &assignment.workspace_path;

    // Recreate workspace if missing
    if !ws_path.exists() {
        println!("Workspace missing, recreating...");
        let ws_name = ws_path
            .file_name()
            .and_then(|n| n.to_str())
            .context("Invalid workspace path")?;

        workspace_mgr.create_workspace(&segment.path, &segment.name, ws_name)?;
        println!("Workspace recreated: {}", ws_path.display());
    }

    // Update status to active
    assignment_mgr.update_status(&assignment.id, AssignmentStatus::Active)?;

    // Check if bead is closed and reopen if needed
    if let Ok(task) = toren_lib::tasks::fetch_task(&assignment.bead_id, &segment.path) {
        println!("Resuming work on {} - {}", task.id, task.title);
    } else {
        println!("Attempting to reopen bead...");
        let _ = toren_lib::tasks::beads::update_bead_status(
            &assignment.bead_id,
            "in_progress",
            &segment.path,
        );
    }

    let prompt = instruction.map(|s| s.to_string()).unwrap_or_else(|| {
        format!(
            "Continue working on bead {}. Review progress and complete remaining work.",
            assignment.bead_id
        )
    });

    println!("Resuming session in workspace: {}\n", ws_path.display());

    let mut cmd = Command::new("claude");
    if danger {
        cmd.arg("--dangerously-skip-permissions");
    }
    cmd.arg(&prompt).current_dir(ws_path);

    let err = cmd.exec();
    Err(err).context("Failed to exec claude")
}

fn cmd_abort(reference: &str, close: bool) -> Result<()> {
    let config = Config::load()?;

    let workspace_root = config
        .ancillary
        .workspace_root
        .clone()
        .context("workspace_root not configured")?;

    let segment_mgr = SegmentManager::new(&config)?;
    let workspace_mgr = WorkspaceManager::new(workspace_root);
    let mut assignment_mgr = AssignmentManager::new()?;

    let segment = resolve_segment(&segment_mgr, None)?;
    let ref_ = AssignmentRef::parse(reference, &segment.name);

    // Get active assignments to abort
    let assignments: Vec<_> = assignment_mgr
        .resolve_active(&ref_)
        .iter()
        .map(|a| (*a).clone())
        .collect();

    if assignments.is_empty() {
        // Fall back to legacy behavior - treat as bead ID directly
        let bead_id = match &ref_ {
            AssignmentRef::Bead(id) => id.clone(),
            AssignmentRef::Ancillary(_) => {
                anyhow::bail!("No active assignment found for: {}", reference);
            }
        };

        // Legacy: cleanup workspace by bead ID
        println!("Cleaning up workspace for {}", bead_id);
        workspace_mgr.cleanup_workspace(&segment.path, &segment.name, &bead_id)?;
        println!("Workspace removed.");

        if close {
            println!("Closing bead...");
            toren_lib::tasks::beads::update_bead_status(&bead_id, "closed", &segment.path)?;
            println!("Bead closed.");
        } else {
            toren_lib::tasks::beads::update_bead_status(&bead_id, "open", &segment.path)?;
            println!("Bead returned to open.");
        }
        return Ok(());
    }

    for assignment in &assignments {
        println!(
            "Aborting assignment: {} -> {}",
            assignment.ancillary_id, assignment.bead_id
        );

        // Cleanup workspace
        if assignment.workspace_path.exists() {
            let ws_name = assignment
                .workspace_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(&assignment.bead_id);

            workspace_mgr.cleanup_workspace(&segment.path, &segment.name, ws_name)?;
            println!("  Workspace removed.");
        }

        // Update assignment status to aborted
        assignment_mgr.update_status(&assignment.id, AssignmentStatus::Aborted)?;
    }

    // Handle bead status
    let bead_ids: std::collections::HashSet<_> = assignments.iter().map(|a| &a.bead_id).collect();

    for bead_id in bead_ids {
        if close {
            println!("Closing bead {}...", bead_id);
            toren_lib::tasks::beads::update_bead_status(bead_id, "closed", &segment.path)?;
        } else {
            // Only reopen if no other active assignments exist for this bead
            let remaining = assignment_mgr
                .get_by_bead(bead_id)
                .into_iter()
                .filter(|a| matches!(a.status, AssignmentStatus::Pending | AssignmentStatus::Active))
                .count();

            if remaining == 0 {
                toren_lib::tasks::beads::update_bead_status(bead_id, "open", &segment.path)?;
                println!("Bead {} returned to open.", bead_id);
            }
        }
    }

    Ok(())
}

fn cmd_approve(reference: &str) -> Result<()> {
    let config = Config::load()?;

    let workspace_root = config
        .ancillary
        .workspace_root
        .clone()
        .context("workspace_root not configured")?;

    let segment_mgr = SegmentManager::new(&config)?;
    let workspace_mgr = WorkspaceManager::new(workspace_root);
    let mut assignment_mgr = AssignmentManager::new()?;

    let segment = resolve_segment(&segment_mgr, None)?;
    let ref_ = AssignmentRef::parse(reference, &segment.name);

    // Clone the assignment data we need before mutating assignment_mgr
    let assignment = {
        let assignments = assignment_mgr.resolve_active(&ref_);

        if assignments.is_empty() {
            // Fall back to legacy behavior
            let bead_id = match &ref_ {
                AssignmentRef::Bead(id) => id.as_str(),
                AssignmentRef::Ancillary(_) => {
                    anyhow::bail!("No active assignment found for: {}", reference);
                }
            };

            let ws_path = workspace_mgr.workspace_path(&segment.name, bead_id);
            if !ws_path.exists() {
                anyhow::bail!("No workspace found for bead {}", bead_id);
            }

            println!("Changes to approve:");
            let _ = Command::new("jj")
                .args(["log", "-n", "10"])
                .current_dir(&ws_path)
                .status();

            println!("\nTo push changes:");
            println!("  cd {}", ws_path.display());
            println!("  jj git push");
            println!("\nThen cleanup:");
            println!("  breq abort {}", bead_id);
            return Ok(());
        }

        if assignments.len() > 1 {
            println!("Multiple active assignments found:");
            for a in &assignments {
                println!("  {} -> {}", a.ancillary_id, a.bead_id);
            }
            anyhow::bail!("Please specify a unique ancillary to approve");
        }

        assignments[0].clone()
    };

    if !assignment.workspace_path.exists() {
        anyhow::bail!(
            "Workspace not found: {}",
            assignment.workspace_path.display()
        );
    }

    println!(
        "Approving: {} -> {}",
        assignment.ancillary_id, assignment.bead_id
    );
    println!("\nChanges to approve:");
    let _ = Command::new("jj")
        .args(["log", "-n", "10"])
        .current_dir(&assignment.workspace_path)
        .status();

    // Mark as completed
    assignment_mgr.update_status(&assignment.id, AssignmentStatus::Completed)?;

    println!("\nTo push changes:");
    println!("  cd {}", assignment.workspace_path.display());
    println!("  jj git push");
    println!("\nWorkspace left for manual cleanup.");

    Ok(())
}

fn cmd_dismiss(reference: &str) -> Result<()> {
    let config = Config::load()?;
    let segment_mgr = SegmentManager::new(&config)?;
    let mut assignment_mgr = AssignmentManager::new()?;

    let segment = resolve_segment(&segment_mgr, None)?;
    let ref_ = AssignmentRef::parse(reference, &segment.name);

    match &ref_ {
        AssignmentRef::Ancillary(ancillary_id) => {
            let dismissed = assignment_mgr.dismiss_ancillary(ancillary_id)?;
            if dismissed.is_empty() {
                println!("No assignments found for ancillary: {}", ancillary_id);
            } else {
                for a in dismissed {
                    println!("Dismissed: {} -> {}", a.ancillary_id, a.bead_id);
                }
            }
        }
        AssignmentRef::Bead(bead_id) => {
            let dismissed = assignment_mgr.dismiss_bead(bead_id)?;
            if dismissed.is_empty() {
                println!("No assignments found for bead: {}", bead_id);
            } else {
                for a in dismissed {
                    println!("Dismissed: {} -> {}", a.ancillary_id, a.bead_id);
                }
            }
        }
    }

    println!("Workspace and bead left unchanged. Use `breq abort` to cleanup.");
    Ok(())
}
