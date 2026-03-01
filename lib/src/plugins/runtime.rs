//! Rhai engine creation and host API registration.

use anyhow::Result;
use rhai::{Dynamic, Engine, Map, Scope, AST};
use std::sync::Arc;

use super::{DeferredAction, PluginContext, PluginResult};

/// Create a Rhai engine with all host functions registered.
pub fn create_engine(ctx: Arc<PluginContext>) -> Engine {
    let mut engine = Engine::new();

    // Override print to use stderr (stdout reserved for structured output)
    engine.on_print(|s| eprintln!("{}", s));
    engine.on_debug(|s, src, pos| {
        if let Some(src) = src {
            eprintln!("[{}:{:?}] {}", src, pos, s);
        } else {
            eprintln!("[{:?}] {}", pos, s);
        }
    });

    // Register host functions
    register_shell(&mut engine);
    register_shell_status(&mut engine);
    register_env(&mut engine);
    register_json_parse(&mut engine);
    register_breq_show(&mut engine, ctx.clone());
    register_breq_clean(&mut engine, ctx.clone());
    register_breq_clean_with(&mut engine, ctx.clone());
    register_task_infer(&mut engine, ctx);

    engine
}

/// Run a compiled AST with the given arguments, return interpreted result.
pub fn run_ast(engine: &Engine, ast: &AST, args: &[String]) -> Result<PluginResult> {
    let mut scope = Scope::new();

    // Set ARGS as a Rhai array
    let args_array: rhai::Array = args.iter().map(|a| Dynamic::from(a.clone())).collect();
    scope.push("ARGS", args_array);

    let result = engine
        .eval_ast_with_scope::<Dynamic>(&mut scope, ast)
        .map_err(|e| anyhow::anyhow!("Plugin script error: {}", e))?;

    interpret_result(result)
}

/// Interpret the script's return value.
///
/// If the script returns a map with `action: "cmd"`, it becomes a `DeferredAction::Cmd`.
/// Otherwise, it's `PluginResult::Ok`.
pub fn interpret_result(value: Dynamic) -> Result<PluginResult> {
    if value.is::<Map>() {
        let map = value.cast::<Map>();
        if let Some(action) = map.get("action") {
            if action.clone().into_string().ok().as_deref() == Some("cmd") {
                let get_str = |key: &str| -> Option<String> {
                    map.get(key)
                        .and_then(|v| v.clone().into_string().ok())
                };

                return Ok(PluginResult::Action(DeferredAction::Cmd {
                    task_id: get_str("task_id"),
                    task_title: get_str("task_title"),
                    task_url: get_str("task_url"),
                    prompt: get_str("prompt"),
                    intent: get_str("intent"),
                }));
            }
        }
    }
    Ok(PluginResult::Ok)
}

// ── Host function registrations ─────────────────────────────────────────────

/// `shell(program, args) -> String` — run command, return stdout, error on non-zero exit.
fn register_shell(engine: &mut Engine) {
    engine.register_fn("shell", |program: &str, args: rhai::Array| -> Result<String, Box<rhai::EvalAltResult>> {
        let str_args: Vec<String> = args
            .into_iter()
            .map(|a| a.into_string().unwrap_or_default())
            .collect();
        let output = std::process::Command::new(program)
            .args(&str_args)
            .output()
            .map_err(|e| format!("Failed to run '{}': {}", program, e))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "'{}' exited with {}: {}",
                program,
                output.status.code().unwrap_or(-1),
                stderr.trim()
            )
            .into());
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    });
}

/// `shell_status(program, args) -> i64` — run command, return exit code.
fn register_shell_status(engine: &mut Engine) {
    engine.register_fn("shell_status", |program: &str, args: rhai::Array| -> Result<i64, Box<rhai::EvalAltResult>> {
        let str_args: Vec<String> = args
            .into_iter()
            .map(|a| a.into_string().unwrap_or_default())
            .collect();
        let status = std::process::Command::new(program)
            .args(&str_args)
            .status()
            .map_err(|e| format!("Failed to run '{}': {}", program, e))?;
        Ok(status.code().unwrap_or(-1) as i64)
    });
}

/// `env(name) -> String` — get environment variable or empty string.
fn register_env(engine: &mut Engine) {
    engine.register_fn("env", |name: &str| -> String {
        std::env::var(name).unwrap_or_default()
    });
}

/// `json_parse(text) -> Dynamic` — parse JSON string to Rhai value.
fn register_json_parse(engine: &mut Engine) {
    engine.register_fn("json_parse", |text: &str| -> Result<Dynamic, Box<rhai::EvalAltResult>> {
        let value: serde_json::Value = serde_json::from_str(text)
            .map_err(|e| format!("JSON parse error: {}", e))?;
        rhai::serde::to_dynamic(&value)
            .map_err(|e| format!("JSON to Rhai conversion error: {}", e).into())
    });
}

/// `breq_show(workspace, field) -> String` — read assignment field (native Rust, no subprocess).
fn register_breq_show(engine: &mut Engine, ctx: Arc<PluginContext>) {
    engine.register_fn("breq_show", move |workspace: &str, field: &str| -> Result<String, Box<rhai::EvalAltResult>> {
        let mut assignment_mgr = crate::AssignmentManager::new()
            .map_err(|e| format!("Failed to load assignments: {}", e))?;

        let segment_name = ctx.segment_name.as_deref().unwrap_or("");

        // Resolve workspace to assignment
        let ws_name = workspace.to_lowercase();
        let ancillary_num = crate::word_to_number(&ws_name).unwrap_or(0);
        let ancillary_id = crate::ancillary_id(segment_name, ancillary_num);

        let assignment = assignment_mgr
            .get_active_for_ancillary(&ancillary_id)
            .ok_or_else(|| format!("No assignment found for workspace '{}'", workspace))?;

        let value = match field {
            "task_id" | "id" => assignment.task_id.clone().unwrap_or_default(),
            "task_title" | "title" => assignment.task_title.clone().unwrap_or_default(),
            "task_url" | "url" => assignment.task_url.clone().unwrap_or_default(),
            "task_source" | "source" => assignment.task_source.clone().unwrap_or_default(),
            "workspace" | "workspace_path" => assignment.workspace_path.display().to_string(),
            "segment" => assignment.segment.clone(),
            "ancillary_id" => assignment.ancillary_id.clone(),
            "status" => format!("{:?}", assignment.status),
            _ => return Err(format!("Unknown field: {}", field).into()),
        };
        Ok(value)
    });
}

/// `breq_clean(workspace) -> Map` — clean workspace with defaults, return result map.
fn register_breq_clean(engine: &mut Engine, ctx: Arc<PluginContext>) {
    let ctx2 = ctx.clone();
    engine.register_fn("breq_clean", move |workspace: &str| -> Result<Map, Box<rhai::EvalAltResult>> {
        do_breq_clean(workspace, false, false, &ctx2)
    });
}

/// `breq_clean_with(workspace, opts) -> Map` — clean workspace with options, return result map.
fn register_breq_clean_with(engine: &mut Engine, ctx: Arc<PluginContext>) {
    engine.register_fn("breq_clean_with", move |workspace: &str, opts: Map| -> Result<Map, Box<rhai::EvalAltResult>> {
        let kill = opts.get("kill").and_then(|v| v.as_bool().ok()).unwrap_or(false);
        let push = opts.get("push").and_then(|v| v.as_bool().ok()).unwrap_or(false);
        do_breq_clean(workspace, kill, push, &ctx)
    });
}

/// Shared implementation for breq_clean and breq_clean_with.
fn do_breq_clean(workspace: &str, kill: bool, push: bool, ctx: &PluginContext) -> Result<Map, Box<rhai::EvalAltResult>> {
    let config = crate::Config::load()
        .map_err(|e| format!("Failed to load config: {}", e))?;

    let workspace_root = config.ancillaries.workspace_root.clone();
    let ws_mgr = crate::WorkspaceManager::new(workspace_root, Some(config.proxy.domain.clone()));
    let mut assignment_mgr = crate::AssignmentManager::new()
        .map_err(|e| format!("Failed to load assignments: {}", e))?;

    let segment_name = ctx.segment_name.as_deref().unwrap_or("");
    let segment_path = ctx.segment_path.as_ref()
        .ok_or_else(|| "No segment path available".to_string())?;

    // Resolve workspace to assignment
    let ws_name = workspace.to_lowercase();
    let ancillary_num = crate::word_to_number(&ws_name).unwrap_or(0);
    let ancillary_id = crate::ancillary_id(segment_name, ancillary_num);

    let assignment = assignment_mgr
        .get_active_for_ancillary(&ancillary_id)
        .cloned()
        .ok_or_else(|| format!("No assignment found for workspace '{}'", workspace))?;

    // Render auto-commit message
    let auto_commit_message = crate::render_auto_commit_message(
        crate::DEFAULT_AUTO_COMMIT_MESSAGE,
        &assignment,
        segment_name,
        segment_path,
    );

    let opts = crate::CleanOptions {
        push,
        segment_path,
        kill,
        auto_commit_message,
    };

    let result = crate::clean_assignment(&assignment, &mut assignment_mgr, &ws_mgr, &opts)
        .map_err(|e| format!("Clean failed: {}", e))?;

    // Build result map
    let mut map = Map::new();
    map.insert("workspace".into(), Dynamic::from(result.workspace));
    map.insert("segment".into(), Dynamic::from(result.segment));
    if let Some(id) = result.id {
        map.insert("id".into(), Dynamic::from(id));
    }
    if let Some(rev) = result.revision {
        map.insert("revision".into(), Dynamic::from(rev));
    }
    Ok(map)
}

/// `task_infer(id) -> Map` — infer task fields from an ID.
fn register_task_infer(engine: &mut Engine, ctx: Arc<PluginContext>) {
    engine.register_fn("task_infer", move |id: &str| -> Result<Map, Box<rhai::EvalAltResult>> {
        let config = crate::Config::load()
            .map_err(|e| format!("Failed to load config: {}", e))?;

        let inferred = crate::infer_task_fields(
            Some(id),
            None,
            None,
            None,
            &config.tasks.default_source,
        );

        // Try to fetch the task title from the task source
        let title = if let Some(ref segment_path) = ctx.segment_path {
            if let Some(ref task_id) = inferred.task_id {
                crate::fetch_task(task_id, segment_path)
                    .ok()
                    .map(|t| t.title)
            } else {
                None
            }
        } else {
            None
        };

        let mut map = Map::new();
        if let Some(id) = inferred.task_id {
            map.insert("id".into(), Dynamic::from(id));
        }
        if let Some(t) = title.or(inferred.task_title) {
            map.insert("title".into(), Dynamic::from(t));
        }
        if let Some(url) = inferred.task_url {
            map.insert("url".into(), Dynamic::from(url));
        }
        if let Some(source) = inferred.task_source {
            map.insert("source".into(), Dynamic::from(source));
        }
        Ok(map)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_interpret_result_ok() {
        let result = interpret_result(Dynamic::from(42)).unwrap();
        assert!(matches!(result, PluginResult::Ok));
    }

    #[test]
    fn test_interpret_result_cmd_action() {
        let mut map = Map::new();
        map.insert("action".into(), Dynamic::from("cmd"));
        map.insert("task_id".into(), Dynamic::from("breq-123"));
        map.insert("task_title".into(), Dynamic::from("Fix the bug"));

        let result = interpret_result(Dynamic::from(map)).unwrap();
        match result {
            PluginResult::Action(DeferredAction::Cmd { task_id, task_title, .. }) => {
                assert_eq!(task_id.as_deref(), Some("breq-123"));
                assert_eq!(task_title.as_deref(), Some("Fix the bug"));
            }
            _ => panic!("Expected DeferredAction::Cmd"),
        }
    }

    #[test]
    fn test_interpret_result_non_cmd_map() {
        let mut map = Map::new();
        map.insert("action".into(), Dynamic::from("other"));

        let result = interpret_result(Dynamic::from(map)).unwrap();
        assert!(matches!(result, PluginResult::Ok));
    }

    #[test]
    fn test_json_parse_via_engine() {
        let ctx = Arc::new(PluginContext {
            segment_path: None,
            segment_name: None,
        });
        let engine = create_engine(ctx);
        let ast = engine.compile(r#"let v = json_parse("{\"a\": 1}"); v.a"#).unwrap();
        let result: i64 = engine.eval_ast(&ast).unwrap();
        assert_eq!(result, 1);
    }

    #[test]
    fn test_env_via_engine() {
        let ctx = Arc::new(PluginContext {
            segment_path: None,
            segment_name: None,
        });
        let engine = create_engine(ctx);
        // PATH should always be set
        let ast = engine.compile(r#"env("PATH")"#).unwrap();
        let result: String = engine.eval_ast(&ast).unwrap();
        assert!(!result.is_empty());
    }

    #[test]
    fn test_env_missing_returns_empty() {
        let ctx = Arc::new(PluginContext {
            segment_path: None,
            segment_name: None,
        });
        let engine = create_engine(ctx);
        let ast = engine.compile(r#"env("__TOREN_NONEXISTENT_VAR__")"#).unwrap();
        let result: String = engine.eval_ast(&ast).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_shell_echo() {
        let ctx = Arc::new(PluginContext {
            segment_path: None,
            segment_name: None,
        });
        let engine = create_engine(ctx);
        let ast = engine.compile(r#"shell("echo", ["hello"])"#).unwrap();
        let result: String = engine.eval_ast(&ast).unwrap();
        assert_eq!(result, "hello");
    }

    #[test]
    fn test_shell_status_success() {
        let ctx = Arc::new(PluginContext {
            segment_path: None,
            segment_name: None,
        });
        let engine = create_engine(ctx);
        let ast = engine.compile(r#"shell_status("true", [])"#).unwrap();
        let result: i64 = engine.eval_ast(&ast).unwrap();
        assert_eq!(result, 0);
    }

    #[test]
    fn test_shell_status_failure() {
        let ctx = Arc::new(PluginContext {
            segment_path: None,
            segment_name: None,
        });
        let engine = create_engine(ctx);
        let ast = engine.compile(r#"shell_status("false", [])"#).unwrap();
        let result: i64 = engine.eval_ast(&ast).unwrap();
        assert_ne!(result, 0);
    }
}
