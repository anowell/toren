use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use std::os::unix::process::CommandExt;
use std::process::Command;
use toren_lib::{Config, SegmentManager, WorkspaceManager};

#[derive(Parser)]
#[command(name = "breq")]
#[command(about = "Spawn Claude ancillaries for bead-driven development")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Assign a bead to a Claude ancillary
    #[command(visible_alias = "a")]
    Assign {
        /// Bead ID to assign
        bead: String,

        /// Intent for handling the bead
        #[arg(short, long, default_value = "act")]
        intent: Intent,

        /// Segment to use (defaults to current directory's segment)
        #[arg(short, long)]
        segment: Option<String>,
    },

    /// List all assignments
    List,

    /// Show detailed assignment information
    Show {
        /// Bead ID to show
        bead: String,
    },

    /// Continue work on an existing assignment
    Extend {
        /// Bead ID to extend
        bead: String,

        /// Additional instructions
        #[arg(short, long)]
        instruction: Option<String>,
    },

    /// Discard workspace and optionally close bead
    Abort {
        /// Bead ID to abort
        bead: String,

        /// Also close the bead
        #[arg(long)]
        close: bool,
    },

    /// Accept work and push changes
    Approve {
        /// Bead ID to approve
        bead: String,
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
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Assign { bead, intent, segment } => cmd_assign(&bead, intent, segment.as_deref()),
        Commands::List => cmd_list(),
        Commands::Show { bead } => cmd_show(&bead),
        Commands::Extend { bead, instruction } => cmd_extend(&bead, instruction.as_deref()),
        Commands::Abort { bead, close } => cmd_abort(&bead, close),
        Commands::Approve { bead } => cmd_approve(&bead),
    }
}

fn cmd_assign(bead_id: &str, intent: Intent, segment_name: Option<&str>) -> Result<()> {
    let config = Config::load()?;

    // Get workspace root from config
    let workspace_root = config.ancillary.workspace_root
        .clone()
        .context("workspace_root not configured in toren.toml")?;

    let workspace_mgr = WorkspaceManager::new(workspace_root);
    let segment_mgr = SegmentManager::new(&config)?;

    // Determine segment
    let segment = if let Some(name) = segment_name {
        segment_mgr.get(name)
            .with_context(|| format!("Segment '{}' not found", name))?
            .clone()
    } else {
        // Try to find segment from current directory
        let cwd = std::env::current_dir()?;
        segment_mgr.list()
            .iter()
            .find(|s| cwd.starts_with(&s.path))
            .cloned()
            .context("Could not determine segment from current directory. Use --segment.")?
    };

    // Fetch bead info
    let task = toren_lib::tasks::fetch_task(bead_id, &segment.path)?;
    println!("Assigning: {} - {}", task.id, task.title);

    // Claim bead
    toren_lib::tasks::beads::claim_bead(bead_id, "claude", &segment.path)?;
    println!("Claimed bead for claude");

    // Create workspace
    let ws_path = workspace_mgr.create_workspace(&segment.path, &segment.name, bead_id)?;
    println!("Workspace: {}", ws_path.display());

    // Build prompt
    let prompt = intent.prompt_template(bead_id, &task.title);

    // Exec into claude (replaces this process)
    println!("Starting Claude session in {}\n", ws_path.display());

    let err = Command::new("claude")
        .arg(&prompt)
        .current_dir(&ws_path)
        .exec();

    // exec() only returns on error
    Err(err).context("Failed to exec claude")
}

fn cmd_list() -> Result<()> {
    let config = Config::load()?;

    let workspace_root = config.ancillary.workspace_root
        .clone()
        .context("workspace_root not configured")?;

    let workspace_mgr = WorkspaceManager::new(workspace_root.clone());
    let segment_mgr = SegmentManager::new(&config)?;

    println!("Assignments:");
    println!("{:<15} {:<15} {:<12} WORKSPACE", "SEGMENT", "BEAD", "STATUS");
    println!("{}", "-".repeat(60));

    for segment in segment_mgr.list() {
        let workspaces = workspace_mgr.list_workspaces(&segment.path).unwrap_or_default();

        // Filter to non-default workspaces (bead workspaces)
        for ws_name in workspaces {
            if ws_name == "default" {
                continue;
            }

            // Try to get bead status
            let status = if toren_lib::tasks::fetch_task(&ws_name, &segment.path).is_ok() {
                "in_progress"
            } else {
                "unknown"
            };

            let ws_path = workspace_mgr.workspace_path(&segment.name, &ws_name);
            let ws_exists = if ws_path.exists() { "yes" } else { "no" };

            println!("{:<15} {:<15} {:<12} {}", segment.name, ws_name, status, ws_exists);
        }
    }

    Ok(())
}

fn cmd_show(bead_id: &str) -> Result<()> {
    let config = Config::load()?;
    let segment_mgr = SegmentManager::new(&config)?;

    // Find segment containing this bead workspace
    let cwd = std::env::current_dir()?;
    let segment = segment_mgr.list()
        .iter()
        .find(|s| cwd.starts_with(&s.path))
        .cloned()
        .context("Could not determine segment")?;

    // Show bead info via bd
    let status = Command::new("bd")
        .args(["show", bead_id])
        .current_dir(&segment.path)
        .status()?;

    if !status.success() {
        anyhow::bail!("Failed to show bead");
    }

    // Show workspace info if it exists
    if let Some(workspace_root) = &config.ancillary.workspace_root {
        let workspace_mgr = WorkspaceManager::new(workspace_root.clone());
        let ws_path = workspace_mgr.workspace_path(&segment.name, bead_id);

        if ws_path.exists() {
            println!("\nWorkspace: {}", ws_path.display());

            // Show jj log
            println!("\nRecent changes:");
            let _ = Command::new("jj")
                .args(["log", "-n", "5"])
                .current_dir(&ws_path)
                .status();
        }
    }

    Ok(())
}

fn cmd_extend(bead_id: &str, instruction: Option<&str>) -> Result<()> {
    let config = Config::load()?;
    let segment_mgr = SegmentManager::new(&config)?;

    let workspace_root = config.ancillary.workspace_root
        .clone()
        .context("workspace_root not configured")?;

    let workspace_mgr = WorkspaceManager::new(workspace_root);

    // Find segment
    let cwd = std::env::current_dir()?;
    let segment = segment_mgr.list()
        .iter()
        .find(|s| cwd.starts_with(&s.path))
        .cloned()
        .context("Could not determine segment")?;

    let ws_path = workspace_mgr.workspace_path(&segment.name, bead_id);
    if !ws_path.exists() {
        anyhow::bail!("No workspace found for bead {}", bead_id);
    }

    let prompt = instruction.map(|s| s.to_string()).unwrap_or_else(|| {
        format!("Continue working on bead {}. Review progress and complete remaining work.", bead_id)
    });

    println!("Extending assignment in workspace: {}\n", ws_path.display());

    let err = Command::new("claude")
        .arg(&prompt)
        .current_dir(&ws_path)
        .exec();

    Err(err).context("Failed to exec claude")
}

fn cmd_abort(bead_id: &str, close: bool) -> Result<()> {
    let config = Config::load()?;
    let segment_mgr = SegmentManager::new(&config)?;

    let workspace_root = config.ancillary.workspace_root
        .clone()
        .context("workspace_root not configured")?;

    let workspace_mgr = WorkspaceManager::new(workspace_root);

    // Find segment
    let cwd = std::env::current_dir()?;
    let segment = segment_mgr.list()
        .iter()
        .find(|s| cwd.starts_with(&s.path))
        .cloned()
        .context("Could not determine segment")?;

    // Cleanup workspace
    println!("Cleaning up workspace for {}", bead_id);
    workspace_mgr.cleanup_workspace(&segment.path, &segment.name, bead_id)?;
    println!("Workspace removed.");

    if close {
        println!("Closing bead...");
        toren_lib::tasks::beads::update_bead_status(bead_id, "closed", &segment.path)?;
        println!("Bead closed.");
    } else {
        // Reopen bead
        toren_lib::tasks::beads::update_bead_status(bead_id, "open", &segment.path)?;
        println!("Bead returned to open.");
    }

    Ok(())
}

fn cmd_approve(bead_id: &str) -> Result<()> {
    let config = Config::load()?;
    let segment_mgr = SegmentManager::new(&config)?;

    let workspace_root = config.ancillary.workspace_root
        .clone()
        .context("workspace_root not configured")?;

    let workspace_mgr = WorkspaceManager::new(workspace_root);

    // Find segment
    let cwd = std::env::current_dir()?;
    let segment = segment_mgr.list()
        .iter()
        .find(|s| cwd.starts_with(&s.path))
        .cloned()
        .context("Could not determine segment")?;

    let ws_path = workspace_mgr.workspace_path(&segment.name, bead_id);
    if !ws_path.exists() {
        anyhow::bail!("No workspace found for bead {}", bead_id);
    }

    // Show what will be pushed
    println!("Changes to approve:");
    let _ = Command::new("jj")
        .args(["log", "-n", "10"])
        .current_dir(&ws_path)
        .status();

    // For now, just print instructions
    println!("\nTo push changes:");
    println!("  cd {}", ws_path.display());
    println!("  jj git push");
    println!("\nThen cleanup:");
    println!("  breq abort {}", bead_id);

    // TODO: Actually push and cleanup

    Ok(())
}
