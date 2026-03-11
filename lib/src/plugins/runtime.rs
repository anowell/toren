//! Rhai engine creation and host API registration.
//!
//! Host functions are organized into Rhai static modules:
//! - `json::parse`, `json::stringify`
//! - `fs::read`, `fs::write`, `fs::exists`, `fs::glob`, `fs::ls`
//! - `path::join`, `path::parent`, `path::filename`, `path::ext`
//! - `toml::parse`
//! - `http::get`, `http::post`, `http::put`, `http::patch`, `http::delete`
//! - `toren::config`, `toren::assignment`
//! - `task::info`, `task::claim`, `task::complete`, `task::abort`, `task::create`
//! - `ws::changes`
//!
//! Flat aliases (`task`, `claim_task`, `complete_task`, `abort_task`, `ancillary`,
//! `config`, `ws_changes`, `json_parse`, `shell_status`) are kept for backwards
//! compatibility.

use anyhow::Result;
use rhai::{Dynamic, Engine, Map, Module, Scope, AST};
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

    // ── Core registrations ───────────────────────────────────────────
    register_shell(&mut engine);
    register_shell_extended(&mut engine);
    register_env(&mut engine);
    register_cwd(&mut engine);
    register_platform(&mut engine);
    register_parse_args(&mut engine);
    register_print_eprint(&mut engine);

    // ── Static modules ───────────────────────────────────────────────
    engine.register_static_module("json", build_json_module().into());
    engine.register_static_module("fs", build_fs_module().into());
    engine.register_static_module("path", build_path_module().into());
    engine.register_static_module("toml", build_toml_module().into());
    engine.register_static_module("http", build_http_module().into());
    engine.register_static_module("task", build_task_module(ctx.clone()));
    engine.register_static_module("toren", build_toren_module(ctx.clone()));
    engine.register_static_module("ws", build_ws_module());

    // ── Flat aliases for backwards compat (DEPRECATED) ───────────────
    register_flat_aliases(&mut engine);
    register_ctx_flat_aliases(&mut engine, ctx);

    engine
}

/// Create a resolver engine — same as `create_engine` but without `toren::task()`
/// to prevent infinite recursion when resolvers are called from `toren::task()`.
pub fn create_resolver_engine(_ctx: Arc<PluginContext>) -> Engine {
    let mut engine = Engine::new();

    engine.on_print(|s| println!("{}", s));
    engine.on_debug(|s, src, pos| {
        if let Some(src) = src {
            eprintln!("[{}:{:?}] {}", src, pos, s);
        } else {
            eprintln!("[{:?}] {}", pos, s);
        }
    });

    register_shell(&mut engine);
    register_shell_extended(&mut engine);
    register_env(&mut engine);
    register_cwd(&mut engine);
    register_platform(&mut engine);
    register_parse_args(&mut engine);
    register_print_eprint(&mut engine);

    engine.register_static_module("json", build_json_module().into());
    engine.register_static_module("fs", build_fs_module().into());
    engine.register_static_module("path", build_path_module().into());
    engine.register_static_module("toml", build_toml_module().into());
    engine.register_static_module("http", build_http_module().into());
    // No toren module (prevents recursion)
    // No ws module (needs PluginContext with segment, not useful in resolvers)

    // Flat aliases minus task/ancillary/ws_changes/config
    register_json_parse_alias(&mut engine);
    register_shell_status_alias(&mut engine);

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
/// If the script returns a map with `action: "do"` (or legacy `"cmd"`), it becomes a `DeferredAction::Do`.
/// Otherwise, it's `PluginResult::Ok`.
pub fn interpret_result(value: Dynamic) -> Result<PluginResult> {
    if value.is::<Map>() {
        let map = value.cast::<Map>();
        if let Some(action) = map.get("action") {
            let action_str = action.clone().into_string().ok();
            if action_str.as_deref() == Some("do") || action_str.as_deref() == Some("cmd") {
                let get_str = |key: &str| -> Option<String> {
                    map.get(key)
                        .and_then(|v| v.clone().into_string().ok())
                };

                return Ok(PluginResult::Action(DeferredAction::Do {
                    task_id: get_str("task_id"),
                    task_title: get_str("task_title"),
                    task_url: get_str("task_url"),
                    task_source: get_str("task_source"),
                    prompt: get_str("prompt"),
                    intent: get_str("intent"),
                }));
            }
        }
    }
    Ok(PluginResult::Ok)
}

// ── Shell ───────────────────────────────────────────────────────────────────

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

/// `shell(program, args, opts) -> Map` — extended shell with options.
///
/// opts keys: `dir`, `env`, `stdin`, `timeout`
/// Returns `#{ stdout, stderr, status }`
fn register_shell_extended(engine: &mut Engine) {
    engine.register_fn("shell", |program: &str, args: rhai::Array, opts: Map| -> Result<Map, Box<rhai::EvalAltResult>> {
        let str_args: Vec<String> = args
            .into_iter()
            .map(|a| a.into_string().unwrap_or_default())
            .collect();

        let mut cmd = std::process::Command::new(program);
        cmd.args(&str_args);

        // dir option
        if let Some(dir) = opts.get("dir") {
            if let Ok(d) = dir.clone().into_string() {
                cmd.current_dir(&d);
            }
        }

        // env option (map of overrides)
        if let Some(env_val) = opts.get("env") {
            if let Some(env_map) = env_val.clone().try_cast::<Map>() {
                for (k, v) in env_map.iter() {
                    cmd.env(k.as_str(), v.clone().into_string().unwrap_or_default());
                }
            }
        }

        // stdin option
        let stdin_data = opts.get("stdin").and_then(|v| v.clone().into_string().ok());
        if stdin_data.is_some() {
            cmd.stdin(std::process::Stdio::piped());
        }

        let mut child = cmd
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("Failed to run '{}': {}", program, e))?;

        if let Some(ref data) = stdin_data {
            use std::io::Write;
            if let Some(ref mut stdin) = child.stdin {
                let _ = stdin.write_all(data.as_bytes());
            }
            // Drop stdin to signal EOF
            child.stdin.take();
        }

        let output = child.wait_with_output()
            .map_err(|e| format!("Failed to wait for '{}': {}", program, e))?;

        let mut result = Map::new();
        result.insert("stdout".into(), Dynamic::from(
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        ));
        result.insert("stderr".into(), Dynamic::from(
            String::from_utf8_lossy(&output.stderr).trim().to_string()
        ));
        result.insert("status".into(), Dynamic::from(
            output.status.code().unwrap_or(-1) as i64
        ));
        Ok(result)
    });
}

// ── Flat registrations ──────────────────────────────────────────────────────

/// `env(name) -> String` — get environment variable or empty string.
fn register_env(engine: &mut Engine) {
    engine.register_fn("env", |name: &str| -> String {
        std::env::var(name).unwrap_or_default()
    });
}

/// `cwd() -> String` — get current working directory.
fn register_cwd(engine: &mut Engine) {
    engine.register_fn("cwd", || -> String {
        std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_default()
    });
}

/// `platform() -> String` — "macos", "linux", or "unknown".
fn register_platform(engine: &mut Engine) {
    engine.register_fn("platform", || -> String {
        if cfg!(target_os = "macos") {
            "macos".to_string()
        } else if cfg!(target_os = "linux") {
            "linux".to_string()
        } else {
            "unknown".to_string()
        }
    });
}

/// `eprint(text)` — print to stderr.
fn register_print_eprint(engine: &mut Engine) {
    engine.register_fn("eprint", |text: &str| {
        eprintln!("{}", text);
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

// ── Module builders ─────────────────────────────────────────────────────────

/// Build the `json` module: `json::parse(text)`, `json::stringify(value)`
fn build_json_module() -> Module {
    let mut module = Module::new();

    module.set_native_fn("parse", |text: &str| -> Result<Dynamic, Box<rhai::EvalAltResult>> {
        let value: serde_json::Value = serde_json::from_str(text)
            .map_err(|e| format!("JSON parse error: {}", e))?;
        rhai::serde::to_dynamic(&value)
            .map_err(|e| format!("JSON to Rhai conversion error: {}", e).into())
    });

    module.set_native_fn("stringify", |value: Dynamic| -> Result<String, Box<rhai::EvalAltResult>> {
        let json_value: serde_json::Value = rhai::serde::from_dynamic(&value)
            .map_err(|e| format!("Rhai to JSON conversion error: {}", e))?;
        serde_json::to_string(&json_value)
            .map_err(|e| format!("JSON stringify error: {}", e).into())
    });

    module
}

/// Build the `fs` module: `fs::read`, `fs::write`, `fs::exists`, `fs::glob`, `fs::ls`
fn build_fs_module() -> Module {
    let mut module = Module::new();

    module.set_native_fn("read", |path: &str| -> Result<String, Box<rhai::EvalAltResult>> {
        std::fs::read_to_string(path)
            .map_err(|e| format!("fs::read error: {}", e).into())
    });

    module.set_native_fn("write", |path: &str, content: &str| -> Result<(), Box<rhai::EvalAltResult>> {
        std::fs::write(path, content)
            .map_err(|e| format!("fs::write error: {}", e).into())
    });

    module.set_native_fn("exists", |path: &str| -> Result<bool, Box<rhai::EvalAltResult>> {
        Ok(std::path::Path::new(path).exists())
    });

    module.set_native_fn("glob", |pattern: &str| -> Result<rhai::Array, Box<rhai::EvalAltResult>> {
        let paths = glob::glob(pattern)
            .map_err(|e| format!("fs::glob pattern error: {}", e))?;
        let result: rhai::Array = paths
            .filter_map(|r| r.ok())
            .map(|p| Dynamic::from(p.display().to_string()))
            .collect();
        Ok(result)
    });

    module.set_native_fn("ls", |path: &str| -> Result<rhai::Array, Box<rhai::EvalAltResult>> {
        let entries = std::fs::read_dir(path)
            .map_err(|e| format!("fs::ls error: {}", e))?;
        let result: rhai::Array = entries
            .filter_map(|e| e.ok())
            .map(|e| Dynamic::from(e.file_name().to_string_lossy().to_string()))
            .collect();
        Ok(result)
    });

    module
}

/// Build the `path` module: `path::join`, `path::parent`, `path::filename`, `path::ext`
fn build_path_module() -> Module {
    let mut module = Module::new();

    module.set_native_fn("join", |a: &str, b: &str| -> Result<String, Box<rhai::EvalAltResult>> {
        Ok(std::path::Path::new(a).join(b).display().to_string())
    });

    module.set_native_fn("parent", |p: &str| -> Result<String, Box<rhai::EvalAltResult>> {
        Ok(std::path::Path::new(p)
            .parent()
            .map(|pp| pp.display().to_string())
            .unwrap_or_default())
    });

    module.set_native_fn("filename", |p: &str| -> Result<String, Box<rhai::EvalAltResult>> {
        Ok(std::path::Path::new(p)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string())
    });

    module.set_native_fn("ext", |p: &str| -> Result<String, Box<rhai::EvalAltResult>> {
        Ok(std::path::Path::new(p)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_string())
    });

    module
}

/// Build the `toml` module: `toml::parse(text)`
fn build_toml_module() -> Module {
    let mut module = Module::new();

    module.set_native_fn("parse", |text: &str| -> Result<Dynamic, Box<rhai::EvalAltResult>> {
        let value: toml::Value = toml::from_str(text)
            .map_err(|e| format!("TOML parse error: {}", e))?;
        // Convert toml::Value -> serde_json::Value -> Dynamic
        let json_value = serde_json::to_value(&value)
            .map_err(|e| format!("TOML to JSON conversion error: {}", e))?;
        rhai::serde::to_dynamic(&json_value)
            .map_err(|e| format!("JSON to Rhai conversion error: {}", e).into())
    });

    module
}

/// Build the `http` module: `http::get`, `http::post`, `http::put`, `http::patch`, `http::delete`
///
/// Each method returns `#{ status, body, ok }`
fn build_http_module() -> Module {
    let mut module = Module::new();

    // GET with no opts
    module.set_native_fn("get", |url: &str| -> Result<Map, Box<rhai::EvalAltResult>> {
        http_no_body("GET", url, &Map::new())
    });

    // GET with opts (headers only, no body)
    module.set_native_fn("get", |url: &str, opts: Map| -> Result<Map, Box<rhai::EvalAltResult>> {
        http_no_body("GET", url, &opts)
    });

    // POST
    module.set_native_fn("post", |url: &str, opts: Map| -> Result<Map, Box<rhai::EvalAltResult>> {
        http_with_body("POST", url, &opts)
    });

    // PUT
    module.set_native_fn("put", |url: &str, opts: Map| -> Result<Map, Box<rhai::EvalAltResult>> {
        http_with_body("PUT", url, &opts)
    });

    // PATCH
    module.set_native_fn("patch", |url: &str, opts: Map| -> Result<Map, Box<rhai::EvalAltResult>> {
        http_with_body("PATCH", url, &opts)
    });

    // DELETE with no opts
    module.set_native_fn("delete", |url: &str| -> Result<Map, Box<rhai::EvalAltResult>> {
        http_no_body("DELETE", url, &Map::new())
    });

    // DELETE with opts
    module.set_native_fn("delete", |url: &str, opts: Map| -> Result<Map, Box<rhai::EvalAltResult>> {
        http_no_body("DELETE", url, &opts)
    });

    module
}

fn make_http_agent() -> ureq::Agent {
    ureq::Agent::new_with_config(
        ureq::config::Config::builder()
            .timeout_global(Some(std::time::Duration::from_secs(30)))
            .http_status_as_error(false)
            .build()
    )
}

/// Apply headers from opts to a request builder via Agent::run.
fn apply_headers(opts: &Map) -> Vec<(String, String)> {
    let mut headers = Vec::new();
    if let Some(headers_val) = opts.get("headers") {
        if let Some(headers_map) = headers_val.clone().try_cast::<Map>() {
            for (k, v) in headers_map.iter() {
                headers.push((k.to_string(), v.clone().into_string().unwrap_or_default()));
            }
        }
    }
    headers
}

/// Make the response map from status + body.
fn make_response_map(status: u16, body: String) -> Map {
    let mut result = Map::new();
    result.insert("status".into(), Dynamic::from(status as i64));
    result.insert("body".into(), Dynamic::from(body));
    result.insert("ok".into(), Dynamic::from((200..300).contains(&status)));
    result
}

/// HTTP request for methods without body (GET, DELETE, HEAD).
fn http_no_body(method: &str, url: &str, opts: &Map) -> Result<Map, Box<rhai::EvalAltResult>> {
    let agent = make_http_agent();
    let mut request = match method {
        "GET" => agent.get(url),
        "DELETE" => agent.delete(url),
        _ => return Err(format!("http_no_body called with unsupported method: {}", method).into()),
    };

    for (k, v) in apply_headers(opts) {
        request = request.header(&k, &v);
    }

    match request.call() {
        Ok(resp) => {
            let status: u16 = resp.status().into();
            let body = resp.into_body().read_to_string().unwrap_or_default();
            Ok(make_response_map(status, body))
        }
        Err(e) => Err(format!("HTTP {} {} failed: {}", method, url, e).into()),
    }
}

/// HTTP request for methods with body (POST, PUT, PATCH).
fn http_with_body(method: &str, url: &str, opts: &Map) -> Result<Map, Box<rhai::EvalAltResult>> {
    let agent = make_http_agent();
    let mut request = match method {
        "POST" => agent.post(url),
        "PUT" => agent.put(url),
        "PATCH" => agent.patch(url),
        _ => return Err(format!("http_with_body called with unsupported method: {}", method).into()),
    };

    for (k, v) in apply_headers(opts) {
        request = request.header(&k, &v);
    }

    // Determine body
    let body_str = if let Some(json_val) = opts.get("json") {
        let json_value: serde_json::Value = rhai::serde::from_dynamic(json_val)
            .map_err(|e| format!("json serialization error: {}", e))?;
        request = request.header("Content-Type", "application/json");
        serde_json::to_string(&json_value)
            .map_err(|e| format!("json stringify error: {}", e))?
    } else if let Some(body_val) = opts.get("body") {
        body_val.clone().into_string().unwrap_or_default()
    } else {
        String::new()
    };

    match request.send(body_str.as_bytes()) {
        Ok(resp) => {
            let status: u16 = resp.status().into();
            let body = resp.into_body().read_to_string().unwrap_or_default();
            Ok(make_response_map(status, body))
        }
        Err(e) => Err(format!("HTTP {} {} failed: {}", method, url, e).into()),
    }
}

/// Register `toren::config`, `toren::task`, `toren::assignment` as flat functions
/// with a `toren` module prefix via engine.register_fn + static module.
///
/// Since Rhai's `set_native_fn` on Module doesn't support closures that capture state,
/// we register these as flat functions and also build a static module for the
/// non-capturing `toren::config`.
fn build_toren_module(ctx: Arc<PluginContext>) -> rhai::Shared<Module> {
    let mut module = Module::new();

    module.set_native_fn("config", |key: &str| -> Result<String, Box<rhai::EvalAltResult>> {
        config_impl(key)
    });

    let assign_ctx = ctx.clone();
    module.set_native_fn("assignment", move |workspace: &str| -> Result<Map, Box<rhai::EvalAltResult>> {
        assignment_impl(workspace, &assign_ctx)
    });

    module.into()
}

fn build_task_module(ctx: Arc<PluginContext>) -> rhai::Shared<Module> {
    let mut module = Module::new();

    let info_ctx = ctx.clone();
    module.set_native_fn("info", move |id: &str| -> Result<Map, Box<rhai::EvalAltResult>> {
        task_impl(id, &info_ctx)
    });

    let claim_ctx = ctx.clone();
    module.set_native_fn("claim", move |source: &str, id: &str, assignee: &str| -> Result<(), Box<rhai::EvalAltResult>> {
        claim_task_impl(source, id, assignee, &claim_ctx)
    });

    let complete_ctx = ctx.clone();
    module.set_native_fn("complete", move |source: &str, id: &str| -> Result<(), Box<rhai::EvalAltResult>> {
        complete_task_impl(source, id, &complete_ctx)
    });

    let abort_ctx = ctx.clone();
    module.set_native_fn("abort", move |source: &str, id: &str| -> Result<(), Box<rhai::EvalAltResult>> {
        abort_task_impl(source, id, &abort_ctx)
    });

    let create_ctx = ctx.clone();
    module.set_native_fn("create", move |source: &str, title: &str, desc: &str| -> Result<String, Box<rhai::EvalAltResult>> {
        create_task_impl(source, title, Some(desc), &create_ctx)
    });

    let create_no_desc_ctx = ctx;
    module.set_native_fn("create", move |source: &str, title: &str| -> Result<String, Box<rhai::EvalAltResult>> {
        create_task_impl(source, title, None, &create_no_desc_ctx)
    });

    module.into()
}

/// Register context-dependent flat aliases (DEPRECATED — use task:: and toren:: modules).
fn register_ctx_flat_aliases(engine: &mut Engine, ctx: Arc<PluginContext>) {
    let task_ctx = ctx.clone();
    engine.register_fn("task", move |id: &str| -> Result<Map, Box<rhai::EvalAltResult>> {
        task_impl(id, &task_ctx)
    });

    let assign_ctx = ctx.clone();
    engine.register_fn("ancillary", move |workspace: &str| -> Result<Map, Box<rhai::EvalAltResult>> {
        assignment_impl(workspace, &assign_ctx)
    });

    let ws_ctx = ctx.clone();
    engine.register_fn("ws_changes", move |workspace: &str| -> Result<rhai::Array, Box<rhai::EvalAltResult>> {
        ws_changes_impl(workspace, &ws_ctx)
    });

    let claim_ctx = ctx.clone();
    engine.register_fn("claim_task", move |source: &str, id: &str, assignee: &str| -> Result<(), Box<rhai::EvalAltResult>> {
        claim_task_impl(source, id, assignee, &claim_ctx)
    });

    let complete_ctx = ctx.clone();
    engine.register_fn("complete_task", move |source: &str, id: &str| -> Result<(), Box<rhai::EvalAltResult>> {
        complete_task_impl(source, id, &complete_ctx)
    });

    engine.register_fn("abort_task", move |source: &str, id: &str| -> Result<(), Box<rhai::EvalAltResult>> {
        abort_task_impl(source, id, &ctx)
    });
}

/// Build the `ws` module — currently empty since ws::changes needs context.
/// ws::changes is registered as a flat function via register_ctx_flat_aliases.
fn build_ws_module() -> rhai::Shared<Module> {
    let module = Module::new();
    module.into()
}

// ── Shared implementations ──────────────────────────────────────────────────

fn config_impl(key: &str) -> Result<String, Box<rhai::EvalAltResult>> {
    let config = crate::Config::load()
        .map_err(|e| format!("Failed to load config: {}", e))?;

    // Virtual key for backwards compat: tasks.default_source -> first element of sources
    if key == "tasks.default_source" {
        return Ok(config.tasks.default_source().unwrap_or_default().to_string());
    }

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
}

fn task_impl(id: &str, ctx: &PluginContext) -> Result<Map, Box<rhai::EvalAltResult>> {
    let config = crate::Config::load()
        .map_err(|e| format!("Failed to load config: {}", e))?;

    let inferred = crate::infer_task_fields(
        Some(id),
        None,
        None,
        None,
    );

    // Try to fetch task via resolver
    let fetched = if let Some(ref task_id) = inferred.task_id {
        // Determine which sources to try
        let sources_to_try: Vec<String> = if let Some(ref explicit_source) = inferred.task_source {
            vec![explicit_source.clone()]
        } else if !ctx.task_sources.is_empty() {
            ctx.task_sources.clone()
        } else if !config.tasks.sources.is_empty() {
            config.tasks.sources.clone()
        } else {
            // Auto-detect: try all available resolvers
            ctx.resolvers.keys().cloned().collect()
        };

        // Try each source's resolver until one succeeds
        let mut result = None;
        for source in &sources_to_try {
            if let Some(resolver_ast) = ctx.resolvers.get(source.as_str()) {
                let resolver_ctx = Arc::new(PluginContext::default());
                let engine = super::runtime::create_resolver_engine(resolver_ctx);
                let mut scope = Scope::new();
                if let Some(task) = engine
                    .call_fn::<Dynamic>(&mut scope, resolver_ast, "info", (task_id.clone(),))
                    .ok()
                    .and_then(|d| d.try_cast::<Map>())
                    .map(|m| {
                        let get_opt = |key: &str| -> Option<String> {
                            m.get(key).and_then(|v| {
                                if v.is::<()>() { None } else { v.clone().into_string().ok() }
                            })
                        };
                        crate::tasks::ResolvedTask {
                            id: get_opt("id").unwrap_or_else(|| task_id.clone()),
                            source: source.clone(),
                            kind: get_opt("kind"),
                            title: get_opt("title").unwrap_or_default(),
                            status: get_opt("status"),
                            assignee: get_opt("assignee"),
                            description: get_opt("description"),
                            created_at: get_opt("created_at"),
                            updated_at: get_opt("updated_at"),
                        }
                    })
                {
                    result = Some(task);
                    break;
                }
            }
        }
        result
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
    // Use source from fetched task (which was resolved) or inferred
    let source = fetched.as_ref().map(|t| t.source.clone()).or(inferred.task_source);
    if let Some(source) = source {
        map.insert("source".into(), Dynamic::from(source));
    }
    if let Some(status) = fetched.as_ref().and_then(|t| t.status.clone()) {
        map.insert("status".into(), Dynamic::from(status));
    }
    if let Some(assignee) = fetched.as_ref().and_then(|t| t.assignee.clone()) {
        map.insert("assignee".into(), Dynamic::from(assignee));
    }
    if let Some(kind) = fetched.as_ref().and_then(|t| t.kind.clone()) {
        map.insert("kind".into(), Dynamic::from(kind));
    }
    Ok(map)
}

fn call_resolver_void(
    source: &str,
    fn_name: &str,
    args: impl rhai::FuncArgs,
    ctx: &PluginContext,
) -> Result<(), Box<rhai::EvalAltResult>> {
    let resolver_ast = ctx
        .resolvers
        .get(source)
        .ok_or_else(|| format!("No task resolver found for source '{}'", source))?;

    let resolver_ctx = Arc::new(PluginContext::default());
    let engine = super::runtime::create_resolver_engine(resolver_ctx);
    let mut scope = Scope::new();
    let _ = engine
        .call_fn::<Dynamic>(&mut scope, resolver_ast, fn_name, args)
        .map_err(|e| format!("Resolver '{}' {}() failed: {}", source, fn_name, e))?;
    Ok(())
}

fn claim_task_impl(
    source: &str,
    id: &str,
    assignee: &str,
    ctx: &PluginContext,
) -> Result<(), Box<rhai::EvalAltResult>> {
    call_resolver_void(source, "claim", (id.to_string(), assignee.to_string()), ctx)
}

fn complete_task_impl(
    source: &str,
    id: &str,
    ctx: &PluginContext,
) -> Result<(), Box<rhai::EvalAltResult>> {
    call_resolver_void(source, "complete", (id.to_string(),), ctx)
}

fn abort_task_impl(
    source: &str,
    id: &str,
    ctx: &PluginContext,
) -> Result<(), Box<rhai::EvalAltResult>> {
    call_resolver_void(source, "abort", (id.to_string(),), ctx)
}

fn create_task_impl(
    source: &str,
    title: &str,
    desc: Option<&str>,
    ctx: &PluginContext,
) -> Result<String, Box<rhai::EvalAltResult>> {
    let resolver_ast = ctx
        .resolvers
        .get(source)
        .ok_or_else(|| format!("No task resolver found for source '{}'", source))?;

    let resolver_ctx = Arc::new(PluginContext::default());
    let engine = super::runtime::create_resolver_engine(resolver_ctx);
    let mut scope = Scope::new();

    let desc_arg = match desc {
        Some(d) => Dynamic::from(d.to_string()),
        None => Dynamic::UNIT,
    };
    let result = engine
        .call_fn::<Dynamic>(&mut scope, resolver_ast, "create", (title.to_string(), desc_arg))
        .map_err(|e| format!("Resolver '{}' create() failed: {}", source, e))?;
    Ok(result.into_string().unwrap_or_default())
}

fn assignment_impl(workspace: &str, ctx: &PluginContext) -> Result<Map, Box<rhai::EvalAltResult>> {
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
}

fn ws_changes_impl(workspace: &str, ctx: &PluginContext) -> Result<rhai::Array, Box<rhai::EvalAltResult>> {
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
}

// ── Flat aliases for backwards compatibility (DEPRECATED) ───────────────────

fn register_flat_aliases(engine: &mut Engine) {
    register_json_parse_alias(engine);
    register_shell_status_alias(engine);

    // config(key) -> toren::config(key)
    engine.register_fn("config", |key: &str| -> Result<String, Box<rhai::EvalAltResult>> {
        config_impl(key)
    });
}

fn register_json_parse_alias(engine: &mut Engine) {
    engine.register_fn("json_parse", |text: &str| -> Result<Dynamic, Box<rhai::EvalAltResult>> {
        let value: serde_json::Value = serde_json::from_str(text)
            .map_err(|e| format!("JSON parse error: {}", e))?;
        rhai::serde::to_dynamic(&value)
            .map_err(|e| format!("JSON to Rhai conversion error: {}", e).into())
    });
}

fn register_shell_status_alias(engine: &mut Engine) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_interpret_result_ok() {
        let result = interpret_result(Dynamic::from(42)).unwrap();
        assert!(matches!(result, PluginResult::Ok));
    }

    #[test]
    fn test_interpret_result_do_action() {
        let mut map = Map::new();
        map.insert("action".into(), Dynamic::from("do"));
        map.insert("task_id".into(), Dynamic::from("breq-123"));
        map.insert("task_title".into(), Dynamic::from("Fix the bug"));

        let result = interpret_result(Dynamic::from(map)).unwrap();
        match result {
            PluginResult::Action(DeferredAction::Do { task_id, task_title, .. }) => {
                assert_eq!(task_id.as_deref(), Some("breq-123"));
                assert_eq!(task_title.as_deref(), Some("Fix the bug"));
            }
            _ => panic!("Expected DeferredAction::Do"),
        }
    }

    #[test]
    fn test_interpret_result_legacy_cmd_action() {
        let mut map = Map::new();
        map.insert("action".into(), Dynamic::from("cmd"));
        map.insert("task_id".into(), Dynamic::from("breq-456"));

        let result = interpret_result(Dynamic::from(map)).unwrap();
        match result {
            PluginResult::Action(DeferredAction::Do { task_id, .. }) => {
                assert_eq!(task_id.as_deref(), Some("breq-456"));
            }
            _ => panic!("Expected DeferredAction::Do from legacy 'cmd'"),
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
        let ctx = Arc::new(PluginContext::default());
        let engine = create_engine(ctx);
        let ast = engine.compile(r#"let v = json_parse("{\"a\": 1}"); v.a"#).unwrap();
        let result: i64 = engine.eval_ast(&ast).unwrap();
        assert_eq!(result, 1);
    }

    #[test]
    fn test_env_via_engine() {
        let ctx = Arc::new(PluginContext::default());
        let engine = create_engine(ctx);
        // PATH should always be set
        let ast = engine.compile(r#"env("PATH")"#).unwrap();
        let result: String = engine.eval_ast(&ast).unwrap();
        assert!(!result.is_empty());
    }

    #[test]
    fn test_env_missing_returns_empty() {
        let ctx = Arc::new(PluginContext::default());
        let engine = create_engine(ctx);
        let ast = engine.compile(r#"env("__TOREN_NONEXISTENT_VAR__")"#).unwrap();
        let result: String = engine.eval_ast(&ast).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_shell_echo() {
        let ctx = Arc::new(PluginContext::default());
        let engine = create_engine(ctx);
        let ast = engine.compile(r#"shell("echo", ["hello"])"#).unwrap();
        let result: String = engine.eval_ast(&ast).unwrap();
        assert_eq!(result, "hello");
    }

    #[test]
    fn test_shell_status_success() {
        let ctx = Arc::new(PluginContext::default());
        let engine = create_engine(ctx);
        let ast = engine.compile(r#"shell_status("true", [])"#).unwrap();
        let result: i64 = engine.eval_ast(&ast).unwrap();
        assert_eq!(result, 0);
    }

    #[test]
    fn test_shell_status_failure() {
        let ctx = Arc::new(PluginContext::default());
        let engine = create_engine(ctx);
        let ast = engine.compile(r#"shell_status("false", [])"#).unwrap();
        let result: i64 = engine.eval_ast(&ast).unwrap();
        assert_ne!(result, 0);
    }

    #[test]
    fn test_config_via_engine() {
        let ctx = Arc::new(PluginContext::default());
        let engine = create_engine(ctx);
        // tasks.default_source returns empty string when no sources configured
        let ast = engine.compile(r#"config("tasks.default_source")"#).unwrap();
        let _result: String = engine.eval_ast(&ast).unwrap();
    }

    #[test]
    fn test_parse_args_bool_flag() {
        let ctx = Arc::new(PluginContext::default());
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
        let ctx = Arc::new(PluginContext::default());
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
        let ctx = Arc::new(PluginContext::default());
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
        let ctx = Arc::new(PluginContext::default());
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
        let ctx = Arc::new(PluginContext::default());
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
        let ctx = Arc::new(PluginContext::default());
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
        let ctx = Arc::new(PluginContext::default());
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
        let ctx = Arc::new(PluginContext::default());
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
        let ctx = Arc::new(PluginContext::default());
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
        let ctx = Arc::new(PluginContext::default());
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
        let ctx = Arc::new(PluginContext::default());
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

    // ── New module tests ──────────────────────────────────────────────

    #[test]
    fn test_json_parse_module() {
        let ctx = Arc::new(PluginContext::default());
        let engine = create_engine(ctx);
        let ast = engine.compile(r#"let v = json::parse("{\"x\": 42}"); v.x"#).unwrap();
        let result: i64 = engine.eval_ast(&ast).unwrap();
        assert_eq!(result, 42);
    }

    #[test]
    fn test_json_stringify_module() {
        let ctx = Arc::new(PluginContext::default());
        let engine = create_engine(ctx);
        let ast = engine.compile(r#"json::stringify(#{ a: 1, b: "hello" })"#).unwrap();
        let result: String = engine.eval_ast(&ast).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["a"], 1);
        assert_eq!(parsed["b"], "hello");
    }

    #[test]
    fn test_fs_exists_module() {
        let ctx = Arc::new(PluginContext::default());
        let engine = create_engine(ctx);
        // Cargo.toml should exist at the workspace root
        let ast = engine.compile(r#"fs::exists("/tmp")"#).unwrap();
        let result: bool = engine.eval_ast(&ast).unwrap();
        assert!(result);
    }

    #[test]
    fn test_fs_exists_not_found() {
        let ctx = Arc::new(PluginContext::default());
        let engine = create_engine(ctx);
        let ast = engine.compile(r#"fs::exists("/nonexistent_path_xyzzy")"#).unwrap();
        let result: bool = engine.eval_ast(&ast).unwrap();
        assert!(!result);
    }

    #[test]
    fn test_fs_read_write() {
        let ctx = Arc::new(PluginContext::default());
        let engine = create_engine(ctx);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt").display().to_string();
        let script = format!(
            r#"fs::write("{path}", "hello world"); fs::read("{path}")"#,
            path = path.replace('\\', "\\\\")
        );
        let ast = engine.compile(&script).unwrap();
        let result: String = engine.eval_ast(&ast).unwrap();
        assert_eq!(result, "hello world");
    }

    #[test]
    fn test_path_join_module() {
        let ctx = Arc::new(PluginContext::default());
        let engine = create_engine(ctx);
        let ast = engine.compile(r#"path::join("/usr", "local")"#).unwrap();
        let result: String = engine.eval_ast(&ast).unwrap();
        assert_eq!(result, "/usr/local");
    }

    #[test]
    fn test_path_parent_module() {
        let ctx = Arc::new(PluginContext::default());
        let engine = create_engine(ctx);
        let ast = engine.compile(r#"path::parent("/usr/local/bin")"#).unwrap();
        let result: String = engine.eval_ast(&ast).unwrap();
        assert_eq!(result, "/usr/local");
    }

    #[test]
    fn test_path_filename_module() {
        let ctx = Arc::new(PluginContext::default());
        let engine = create_engine(ctx);
        let ast = engine.compile(r#"path::filename("/usr/local/bin/bash")"#).unwrap();
        let result: String = engine.eval_ast(&ast).unwrap();
        assert_eq!(result, "bash");
    }

    #[test]
    fn test_path_ext_module() {
        let ctx = Arc::new(PluginContext::default());
        let engine = create_engine(ctx);
        let ast = engine.compile(r#"path::ext("/home/user/file.rs")"#).unwrap();
        let result: String = engine.eval_ast(&ast).unwrap();
        assert_eq!(result, "rs");
    }

    #[test]
    fn test_toml_parse_module() {
        let ctx = Arc::new(PluginContext::default());
        let engine = create_engine(ctx);
        let ast = engine.compile(r#"
            let t = toml::parse("[section]\nkey = \"value\"\nnum = 42");
            [t.section.key, t.section.num]
        "#).unwrap();
        let result: rhai::Array = engine.eval_ast(&ast).unwrap();
        assert_eq!(result[0].clone().into_string().unwrap(), "value");
        assert_eq!(result[1].clone().cast::<i64>(), 42);
    }

    #[test]
    fn test_toren_config_module() {
        let ctx = Arc::new(PluginContext::default());
        let engine = create_engine(ctx);
        // tasks.default_source returns empty string when no sources configured
        let ast = engine.compile(r#"toren::config("tasks.default_source")"#).unwrap();
        let _result: String = engine.eval_ast(&ast).unwrap();
    }

    #[test]
    fn test_shell_extended_overload() {
        let ctx = Arc::new(PluginContext::default());
        let engine = create_engine(ctx);
        let ast = engine.compile(r#"
            let r = shell("echo", ["hello"], #{});
            [r.stdout, r.status]
        "#).unwrap();
        let result: rhai::Array = engine.eval_ast(&ast).unwrap();
        assert_eq!(result[0].clone().into_string().unwrap(), "hello");
        assert_eq!(result[1].clone().cast::<i64>(), 0);
    }

    #[test]
    fn test_shell_extended_failure() {
        let ctx = Arc::new(PluginContext::default());
        let engine = create_engine(ctx);
        let ast = engine.compile(r#"
            let r = shell("false", [], #{});
            r.status
        "#).unwrap();
        let result: i64 = engine.eval_ast(&ast).unwrap();
        assert_ne!(result, 0);
    }

    #[test]
    fn test_shell_extended_with_stdin() {
        let ctx = Arc::new(PluginContext::default());
        let engine = create_engine(ctx);
        let ast = engine.compile(r#"
            let r = shell("cat", [], #{ stdin: "piped input" });
            r.stdout
        "#).unwrap();
        let result: String = engine.eval_ast(&ast).unwrap();
        assert_eq!(result, "piped input");
    }

    #[test]
    fn test_eprint() {
        let ctx = Arc::new(PluginContext::default());
        let engine = create_engine(ctx);
        // Just verify it compiles and runs without error
        let ast = engine.compile(r#"eprint("test stderr output")"#).unwrap();
        let _ = engine.eval_ast::<Dynamic>(&ast).unwrap();
    }

    #[test]
    fn test_cwd() {
        let ctx = Arc::new(PluginContext::default());
        let engine = create_engine(ctx);
        let ast = engine.compile(r#"cwd()"#).unwrap();
        let result: String = engine.eval_ast(&ast).unwrap();
        assert!(!result.is_empty());
    }

    #[test]
    fn test_platform() {
        let ctx = Arc::new(PluginContext::default());
        let engine = create_engine(ctx);
        let ast = engine.compile(r#"platform()"#).unwrap();
        let result: String = engine.eval_ast(&ast).unwrap();
        assert!(result == "macos" || result == "linux" || result == "unknown");
    }
}
