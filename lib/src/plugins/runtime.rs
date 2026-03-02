//! Rhai engine creation and host API registration.

use anyhow::Result;
use rhai::{Dynamic, Engine, Map, Scope, AST};
use std::sync::Arc;

use super::{DeferredAction, PluginContext, PluginResult};

/// Create a Rhai engine with all host functions registered.
pub fn create_engine(ctx: Arc<PluginContext>) -> Engine {
    let mut engine = Engine::new();

    // Plugin print goes to stdout
    engine.on_print(|s| println!("{}", s));
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
    register_config(&mut engine);
    register_parse_args(&mut engine);
    register_task(&mut engine, ctx.clone());
    register_ancillary(&mut engine, ctx.clone());
    register_ws_changes(&mut engine, ctx);

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

/// `parse_args(args, spec) -> Map` — parse CLI-style arguments according to a spec.
///
/// `spec` is a map where each key is a long option name and each value is a config map with:
/// - `type` (required): `"bool"`, `"string"`, or `"int"`
/// - `short` (optional): single-char short alias (e.g. `"s"` for `-s`)
/// - `default_val` (optional): default value if not provided
///
/// Returns a map with:
/// - `args`: array of positional arguments
/// - `opts`: map of parsed option values keyed by long name
fn register_parse_args(engine: &mut Engine) {
    engine.register_fn(
        "parse_args",
        |args: rhai::Array, spec: Map| -> Result<Map, Box<rhai::EvalAltResult>> {
            // Build lookup tables from the spec
            struct OptSpec {
                opt_type: String,
                default: Dynamic,
            }

            let mut specs_by_long: std::collections::HashMap<String, OptSpec> =
                std::collections::HashMap::new();
            let mut short_to_long: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();

            for (key, value) in spec.iter() {
                let long = key.to_string();
                let conf = value
                    .clone()
                    .try_cast::<Map>()
                    .ok_or_else(|| format!("spec for '{}' must be a map", long))?;

                let opt_type = conf
                    .get("type")
                    .ok_or_else(|| format!("spec for '{}' missing 'type' field", long))?
                    .clone()
                    .into_string()
                    .map_err(|_| format!("spec for '{}': 'type' must be a string", long))?;

                match opt_type.as_str() {
                    "bool" | "string" | "int" => {}
                    other => {
                        return Err(
                            format!("spec for '{}': unknown type '{}'", long, other).into()
                        )
                    }
                }

                let default = if let Some(d) = conf.get("default_val") {
                    d.clone()
                } else {
                    match opt_type.as_str() {
                        "bool" => Dynamic::from(false),
                        _ => Dynamic::UNIT,
                    }
                };

                if let Some(short_val) = conf.get("short") {
                    let short = short_val
                        .clone()
                        .into_string()
                        .map_err(|_| format!("spec for '{}': 'short' must be a string", long))?;
                    short_to_long.insert(short, long.clone());
                }

                specs_by_long.insert(
                    long.clone(),
                    OptSpec {
                        opt_type,
                        default,
                    },
                );
            }

            // Parse the args
            let str_args: Vec<String> = args
                .into_iter()
                .map(|a| a.into_string().unwrap_or_default())
                .collect();

            let mut positional: rhai::Array = Vec::new();
            let mut opts = Map::new();

            // Initialize defaults
            for (long, spec) in &specs_by_long {
                opts.insert(long.as_str().into(), spec.default.clone());
            }

            let mut i = 0;
            let mut rest_positional = false;

            while i < str_args.len() {
                let arg = &str_args[i];

                if rest_positional {
                    positional.push(Dynamic::from(arg.clone()));
                    i += 1;
                    continue;
                }

                if arg == "--" {
                    rest_positional = true;
                    i += 1;
                    continue;
                }

                if let Some(long_name) = arg.strip_prefix("--") {
                    // Long option
                    let spec = specs_by_long.get(long_name).ok_or_else(|| {
                        format!("unknown option: --{}", long_name)
                    })?;
                    match spec.opt_type.as_str() {
                        "bool" => {
                            opts.insert(long_name.into(), Dynamic::from(true));
                        }
                        "string" => {
                            i += 1;
                            let val = str_args.get(i).ok_or_else(|| {
                                format!("--{} requires a value", long_name)
                            })?;
                            opts.insert(long_name.into(), Dynamic::from(val.clone()));
                        }
                        "int" => {
                            i += 1;
                            let val_str = str_args.get(i).ok_or_else(|| {
                                format!("--{} requires a value", long_name)
                            })?;
                            let val: i64 = val_str.parse().map_err(|_| {
                                format!("--{}: '{}' is not a valid integer", long_name, val_str)
                            })?;
                            opts.insert(long_name.into(), Dynamic::from(val));
                        }
                        _ => unreachable!(),
                    }
                } else if let Some(short_chars) = arg.strip_prefix('-') {
                    if short_chars.is_empty() {
                        // Bare "-" is positional
                        positional.push(Dynamic::from(arg.clone()));
                        i += 1;
                        continue;
                    }
                    // Short option
                    let long_name = short_to_long.get(short_chars).ok_or_else(|| {
                        format!("unknown option: -{}", short_chars)
                    })?;
                    let spec = &specs_by_long[long_name];
                    match spec.opt_type.as_str() {
                        "bool" => {
                            opts.insert(long_name.as_str().into(), Dynamic::from(true));
                        }
                        "string" => {
                            i += 1;
                            let val = str_args.get(i).ok_or_else(|| {
                                format!("-{} requires a value", short_chars)
                            })?;
                            opts.insert(long_name.as_str().into(), Dynamic::from(val.clone()));
                        }
                        "int" => {
                            i += 1;
                            let val_str = str_args.get(i).ok_or_else(|| {
                                format!("-{} requires a value", short_chars)
                            })?;
                            let val: i64 = val_str.parse().map_err(|_| {
                                format!(
                                    "-{}: '{}' is not a valid integer",
                                    short_chars, val_str
                                )
                            })?;
                            opts.insert(long_name.as_str().into(), Dynamic::from(val));
                        }
                        _ => unreachable!(),
                    }
                } else {
                    // Positional argument
                    positional.push(Dynamic::from(arg.clone()));
                }

                i += 1;
            }

            let mut result = Map::new();
            result.insert("args".into(), Dynamic::from(positional));
            result.insert("opts".into(), Dynamic::from(opts));
            Ok(result)
        },
    );
}

/// `config(key) -> String` — read a config value by dot-separated path.
fn register_config(engine: &mut Engine) {
    engine.register_fn("config", |key: &str| -> Result<String, Box<rhai::EvalAltResult>> {
        let config = crate::Config::load()
            .map_err(|e| format!("Failed to load config: {}", e))?;

        let json_value = serde_json::to_value(&config)
            .map_err(|e| format!("Failed to serialize config: {}", e))?;

        // Traverse dot-path segments
        let mut current = &json_value;
        for segment in key.split('.') {
            match current {
                serde_json::Value::Object(map) => {
                    current = map.get(segment)
                        .ok_or_else(|| format!("Config key not found: {}", key))?;
                }
                _ => return Err(format!("Config key not found: {}", key).into()),
            }
        }

        // Convert to string representation
        let result = match current {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::Bool(b) => b.to_string(),
            serde_json::Value::Null => String::new(),
            other => other.to_string(),
        };
        Ok(result)
    });
}

/// `task(id) -> Map` — infer task fields from an ID.
fn register_task(engine: &mut Engine, ctx: Arc<PluginContext>) {
    engine.register_fn("task", move |id: &str| -> Result<Map, Box<rhai::EvalAltResult>> {
        let config = crate::Config::load()
            .map_err(|e| format!("Failed to load config: {}", e))?;

        let inferred = crate::infer_task_fields(
            Some(id),
            None,
            None,
            None,
            &config.tasks.default_source,
        );

        // Try to fetch the task from the task source
        let fetched = if let Some(ref segment_path) = ctx.segment_path {
            if let Some(ref task_id) = inferred.task_id {
                crate::fetch_task(task_id, segment_path).ok()
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
        let title = fetched.as_ref().map(|t| t.title.clone()).or(inferred.task_title);
        if let Some(t) = title {
            map.insert("title".into(), Dynamic::from(t));
        }
        if let Some(desc) = fetched.as_ref().and_then(|t| t.description.clone()) {
            map.insert("description".into(), Dynamic::from(desc));
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

/// `ancillary(workspace) -> Map` — resolve workspace to assignment, return all fields.
fn register_ancillary(engine: &mut Engine, ctx: Arc<PluginContext>) {
    engine.register_fn("ancillary", move |workspace: &str| -> Result<Map, Box<rhai::EvalAltResult>> {
        let mut assignment_mgr = crate::AssignmentManager::new()
            .map_err(|e| format!("Failed to load assignments: {}", e))?;

        let segment_name = ctx.segment_name.as_deref().unwrap_or("");

        // Resolve workspace to assignment
        let ws_name = workspace.to_lowercase();
        let ancillary_num = crate::word_to_number(&ws_name).unwrap_or(0);
        let anc_id = crate::ancillary_id(segment_name, ancillary_num);

        let assignment = assignment_mgr
            .get_active_for_ancillary(&anc_id)
            .ok_or_else(|| format!("No assignment found for workspace '{}'", workspace))?;

        let mut map = Map::new();
        map.insert("id".into(), Dynamic::from(assignment.id.clone()));
        map.insert("ancillary_id".into(), Dynamic::from(assignment.ancillary_id.clone()));
        map.insert("segment".into(), Dynamic::from(assignment.segment.clone()));
        map.insert("workspace_path".into(), Dynamic::from(assignment.workspace_path.display().to_string()));
        map.insert("status".into(), Dynamic::from(format!("{:?}", assignment.status)));
        map.insert("task_id".into(), Dynamic::from(assignment.task_id.clone().unwrap_or_default()));
        map.insert("task_title".into(), Dynamic::from(assignment.task_title.clone().unwrap_or_default()));
        map.insert("task_url".into(), Dynamic::from(assignment.task_url.clone().unwrap_or_default()));
        map.insert("task_source".into(), Dynamic::from(assignment.task_source.clone().unwrap_or_default()));
        map.insert("session_id".into(), Dynamic::from(assignment.session_id.clone().unwrap_or_default()));
        map.insert("ancillary_num".into(), Dynamic::from(assignment.ancillary_num.unwrap_or(0) as i64));
        map.insert("base_branch".into(), Dynamic::from(assignment.base_branch.clone().unwrap_or_default()));
        Ok(map)
    });
}

/// `ws_changes(workspace) -> Array` — get workspace commit info as array of `{id, summary}` maps.
fn register_ws_changes(engine: &mut Engine, ctx: Arc<PluginContext>) {
    engine.register_fn("ws_changes", move |workspace: &str| -> Result<rhai::Array, Box<rhai::EvalAltResult>> {
        let config = crate::Config::load()
            .map_err(|e| format!("Failed to load config: {}", e))?;
        let mut assignment_mgr = crate::AssignmentManager::new()
            .map_err(|e| format!("Failed to load assignments: {}", e))?;

        let segment_name = ctx.segment_name.as_deref().unwrap_or("");
        let segment_path = ctx.segment_path.as_ref()
            .ok_or_else(|| "No segment path available".to_string())?;

        // Resolve workspace to assignment
        let ws_name = workspace.to_lowercase();
        let ancillary_num = crate::word_to_number(&ws_name).unwrap_or(0);
        let anc_id = crate::ancillary_id(segment_name, ancillary_num);

        let assignment = assignment_mgr
            .get_active_for_ancillary(&anc_id)
            .ok_or_else(|| format!("No assignment found for workspace '{}'", workspace))?;

        let workspace_root = config.ancillaries.workspace_root.clone();
        let ws_mgr = crate::WorkspaceManager::new(workspace_root, Some(config.proxy.domain.clone()));

        let commits = ws_mgr
            .workspace_info(segment_path, &assignment.workspace_path, assignment.base_branch.as_deref())
            .map_err(|e| format!("Failed to get workspace info: {}", e))?;

        let result: rhai::Array = commits
            .into_iter()
            .map(|c| {
                let mut m = Map::new();
                m.insert("id".into(), Dynamic::from(c.id));
                m.insert("summary".into(), Dynamic::from(c.summary));
                Dynamic::from(m)
            })
            .collect();

        Ok(result)
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

    #[test]
    fn test_config_via_engine() {
        let ctx = Arc::new(PluginContext {
            segment_path: None,
            segment_name: None,
        });
        let engine = create_engine(ctx);
        // tasks.default_source should always exist
        let ast = engine.compile(r#"config("tasks.default_source")"#).unwrap();
        let result: String = engine.eval_ast(&ast).unwrap();
        assert!(!result.is_empty());
    }

    #[test]
    fn test_parse_args_bool_flag() {
        let ctx = Arc::new(PluginContext { segment_path: None, segment_name: None });
        let engine = create_engine(ctx);
        let ast = engine.compile(r#"
            let p = parse_args(["--push"], #{ push: #{ type: "bool" } });
            [p.opts.push, p.args.len()]
        "#).unwrap();
        let result: rhai::Array = engine.eval_ast(&ast).unwrap();
        assert_eq!(result[0].clone().cast::<bool>(), true);
        assert_eq!(result[1].clone().cast::<i64>(), 0);
    }

    #[test]
    fn test_parse_args_bool_default_false() {
        let ctx = Arc::new(PluginContext { segment_path: None, segment_name: None });
        let engine = create_engine(ctx);
        let ast = engine.compile(r#"
            let p = parse_args([], #{ push: #{ type: "bool" } });
            p.opts.push
        "#).unwrap();
        let result: bool = engine.eval_ast(&ast).unwrap();
        assert!(!result);
    }

    #[test]
    fn test_parse_args_string_option() {
        let ctx = Arc::new(PluginContext { segment_path: None, segment_name: None });
        let engine = create_engine(ctx);
        let ast = engine.compile(r#"
            let p = parse_args(["--segment", "toren"], #{ segment: #{ type: "string", short: "s" } });
            p.opts.segment
        "#).unwrap();
        let result: String = engine.eval_ast(&ast).unwrap();
        assert_eq!(result, "toren");
    }

    #[test]
    fn test_parse_args_short_alias() {
        let ctx = Arc::new(PluginContext { segment_path: None, segment_name: None });
        let engine = create_engine(ctx);
        let ast = engine.compile(r#"
            let p = parse_args(["-s", "toren"], #{ segment: #{ type: "string", short: "s" } });
            p.opts.segment
        "#).unwrap();
        let result: String = engine.eval_ast(&ast).unwrap();
        assert_eq!(result, "toren");
    }

    #[test]
    fn test_parse_args_int_option() {
        let ctx = Arc::new(PluginContext { segment_path: None, segment_name: None });
        let engine = create_engine(ctx);
        let ast = engine.compile(r#"
            let p = parse_args(["--count", "10"], #{ count: #{ type: "int", default_val: 5 } });
            p.opts.count
        "#).unwrap();
        let result: i64 = engine.eval_ast(&ast).unwrap();
        assert_eq!(result, 10);
    }

    #[test]
    fn test_parse_args_int_default() {
        let ctx = Arc::new(PluginContext { segment_path: None, segment_name: None });
        let engine = create_engine(ctx);
        let ast = engine.compile(r#"
            let p = parse_args([], #{ count: #{ type: "int", default_val: 5 } });
            p.opts.count
        "#).unwrap();
        let result: i64 = engine.eval_ast(&ast).unwrap();
        assert_eq!(result, 5);
    }

    #[test]
    fn test_parse_args_string_absent_is_unit() {
        let ctx = Arc::new(PluginContext { segment_path: None, segment_name: None });
        let engine = create_engine(ctx);
        let ast = engine.compile(r#"
            let p = parse_args([], #{ name: #{ type: "string" } });
            p.opts.name == ()
        "#).unwrap();
        let result: bool = engine.eval_ast(&ast).unwrap();
        assert!(result);
    }

    #[test]
    fn test_parse_args_positional() {
        let ctx = Arc::new(PluginContext { segment_path: None, segment_name: None });
        let engine = create_engine(ctx);
        let ast = engine.compile(r#"
            let p = parse_args(["foo", "--push", "bar"], #{ push: #{ type: "bool" } });
            [p.args[0], p.args[1], p.opts.push]
        "#).unwrap();
        let result: rhai::Array = engine.eval_ast(&ast).unwrap();
        assert_eq!(result[0].clone().into_string().unwrap(), "foo");
        assert_eq!(result[1].clone().into_string().unwrap(), "bar");
        assert_eq!(result[2].clone().cast::<bool>(), true);
    }

    #[test]
    fn test_parse_args_double_dash_stops_parsing() {
        let ctx = Arc::new(PluginContext { segment_path: None, segment_name: None });
        let engine = create_engine(ctx);
        let ast = engine.compile(r#"
            let p = parse_args(["--", "--push"], #{ push: #{ type: "bool" } });
            [p.opts.push, p.args[0]]
        "#).unwrap();
        let result: rhai::Array = engine.eval_ast(&ast).unwrap();
        assert_eq!(result[0].clone().cast::<bool>(), false);
        assert_eq!(result[1].clone().into_string().unwrap(), "--push");
    }

    #[test]
    fn test_parse_args_unknown_flag_errors() {
        let ctx = Arc::new(PluginContext { segment_path: None, segment_name: None });
        let engine = create_engine(ctx);
        let ast = engine.compile(r#"
            parse_args(["--unknown"], #{ push: #{ type: "bool" } })
        "#).unwrap();
        let result = engine.eval_ast::<Dynamic>(&ast);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("unknown option"), "Error was: {}", err);
    }

    #[test]
    fn test_parse_args_combined() {
        let ctx = Arc::new(PluginContext { segment_path: None, segment_name: None });
        let engine = create_engine(ctx);
        let ast = engine.compile(r#"
            let p = parse_args(
                ["task-123", "--push", "-i", "act"],
                #{
                    push: #{ type: "bool" },
                    intent: #{ type: "string", short: "i" },
                }
            );
            [p.args[0], p.opts.push, p.opts.intent]
        "#).unwrap();
        let result: rhai::Array = engine.eval_ast(&ast).unwrap();
        assert_eq!(result[0].clone().into_string().unwrap(), "task-123");
        assert_eq!(result[1].clone().cast::<bool>(), true);
        assert_eq!(result[2].clone().into_string().unwrap(), "act");
    }
}
