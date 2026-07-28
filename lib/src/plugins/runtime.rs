//! Rhai engine creation and host API registration.
//!
//! Host functions are organized into Rhai static modules:
//! - `json::parse`, `json::stringify`
//! - `fs::read`, `fs::write`, `fs::exists`, `fs::glob`, `fs::ls`, `fs::newest`, `fs::head`,
//!   `fs::tail`, `fs::age_secs`
//! - `path::join`, `path::parent`, `path::filename`, `path::ext`
//! - `toml::parse`
//! - `http::get`, `http::post`, `http::put`, `http::patch`, `http::delete`
//! - `toren::config`
//!
//! Plus flat `shell`, `env`, `cwd`, `platform`, `parse_args`, `eprint`, and the legacy
//! `json_parse` / `shell_status` / `config` aliases.
//!
//! Every plugin is a resolver, so there is exactly one engine shape. Resolvers deliberately
//! cannot reach back into breq's own state: they adapt an external system and return data.

use anyhow::Result;
use rhai::{Dynamic, Engine, Map, Module, Scope, AST};
use std::sync::Arc;

use super::PluginContext;

/// Nesting limit for plugin scripts.
///
/// Rhai defaults function bodies to a depth that a plainly-written resolver hits quickly —
/// an `if` inside a `for` inside two `if`s is enough. Plugins are the user's own trusted
/// code, so the limit buys nothing here.
const MAX_EXPR_DEPTH: usize = 128;

/// A bare engine with breq's parser limits, for compiling without registering host functions.
pub fn compiler() -> Engine {
    let mut engine = Engine::new();
    engine.set_max_expr_depths(MAX_EXPR_DEPTH, MAX_EXPR_DEPTH);
    engine
}

/// Create a Rhai engine with all host functions registered.
pub fn create_engine(ctx: Arc<PluginContext>) -> Engine {
    let mut engine = Engine::new();
    engine.set_max_expr_depths(MAX_EXPR_DEPTH, MAX_EXPR_DEPTH);

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
    engine.register_static_module("toren", build_toren_module());

    // ── Flat aliases for backwards compat (DEPRECATED) ───────────────
    register_flat_aliases(&mut engine);

    let _ = ctx;
    engine
}

/// Run a compiled AST with `ARGS` in scope.
pub fn eval_with_args(engine: &Engine, ast: &AST, args: &[String]) -> Result<Dynamic> {
    let mut scope = Scope::new();
    let args_array: rhai::Array = args.iter().map(|a| Dynamic::from(a.clone())).collect();
    scope.push("ARGS", args_array);

    engine
        .eval_ast_with_scope::<Dynamic>(&mut scope, ast)
        .map_err(|e| anyhow::anyhow!("Plugin script error: {}", e))
}

// ── Shell ───────────────────────────────────────────────────────────────────

/// `shell(program, args) -> String` — run command, return stdout, error on non-zero exit.
fn register_shell(engine: &mut Engine) {
    engine.register_fn(
        "shell",
        |program: &str, args: rhai::Array| -> Result<String, Box<rhai::EvalAltResult>> {
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
        },
    );
}

/// `shell(program, args, opts) -> Map` — extended shell with options.
///
/// opts keys: `dir`, `env`, `stdin`, `timeout`
/// Returns `#{ stdout, stderr, status }`
fn register_shell_extended(engine: &mut Engine) {
    engine.register_fn(
        "shell",
        |program: &str, args: rhai::Array, opts: Map| -> Result<Map, Box<rhai::EvalAltResult>> {
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

            let output = child
                .wait_with_output()
                .map_err(|e| format!("Failed to wait for '{}': {}", program, e))?;

            let mut result = Map::new();
            result.insert(
                "stdout".into(),
                Dynamic::from(String::from_utf8_lossy(&output.stdout).trim().to_string()),
            );
            result.insert(
                "stderr".into(),
                Dynamic::from(String::from_utf8_lossy(&output.stderr).trim().to_string()),
            );
            result.insert(
                "status".into(),
                Dynamic::from(output.status.code().unwrap_or(-1) as i64),
            );
            Ok(result)
        },
    );
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
                        return Err(format!("spec for '{}': unknown type '{}'", long, other).into())
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

                specs_by_long.insert(long.clone(), OptSpec { opt_type, default });
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
                    let spec = specs_by_long
                        .get(long_name)
                        .ok_or_else(|| format!("unknown option: --{}", long_name))?;
                    match spec.opt_type.as_str() {
                        "bool" => {
                            opts.insert(long_name.into(), Dynamic::from(true));
                        }
                        "string" => {
                            i += 1;
                            let val = str_args
                                .get(i)
                                .ok_or_else(|| format!("--{} requires a value", long_name))?;
                            opts.insert(long_name.into(), Dynamic::from(val.clone()));
                        }
                        "int" => {
                            i += 1;
                            let val_str = str_args
                                .get(i)
                                .ok_or_else(|| format!("--{} requires a value", long_name))?;
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
                    let long_name = short_to_long
                        .get(short_chars)
                        .ok_or_else(|| format!("unknown option: -{}", short_chars))?;
                    let spec = &specs_by_long[long_name];
                    match spec.opt_type.as_str() {
                        "bool" => {
                            opts.insert(long_name.as_str().into(), Dynamic::from(true));
                        }
                        "string" => {
                            i += 1;
                            let val = str_args
                                .get(i)
                                .ok_or_else(|| format!("-{} requires a value", short_chars))?;
                            opts.insert(long_name.as_str().into(), Dynamic::from(val.clone()));
                        }
                        "int" => {
                            i += 1;
                            let val_str = str_args
                                .get(i)
                                .ok_or_else(|| format!("-{} requires a value", short_chars))?;
                            let val: i64 = val_str.parse().map_err(|_| {
                                format!("-{}: '{}' is not a valid integer", short_chars, val_str)
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

    module.set_native_fn(
        "parse",
        |text: &str| -> Result<Dynamic, Box<rhai::EvalAltResult>> {
            let value: serde_json::Value =
                serde_json::from_str(text).map_err(|e| format!("JSON parse error: {}", e))?;
            rhai::serde::to_dynamic(&value)
                .map_err(|e| format!("JSON to Rhai conversion error: {}", e).into())
        },
    );

    module.set_native_fn(
        "stringify",
        |value: Dynamic| -> Result<String, Box<rhai::EvalAltResult>> {
            let json_value: serde_json::Value = rhai::serde::from_dynamic(&value)
                .map_err(|e| format!("Rhai to JSON conversion error: {}", e))?;
            serde_json::to_string(&json_value)
                .map_err(|e| format!("JSON stringify error: {}", e).into())
        },
    );

    module
}

/// Build the `fs` module: `fs::read`, `fs::write`, `fs::exists`, `fs::glob`, `fs::ls`
fn build_fs_module() -> Module {
    let mut module = Module::new();

    module.set_native_fn(
        "read",
        |path: &str| -> Result<String, Box<rhai::EvalAltResult>> {
            std::fs::read_to_string(path).map_err(|e| format!("fs::read error: {}", e).into())
        },
    );

    module.set_native_fn(
        "write",
        |path: &str, content: &str| -> Result<(), Box<rhai::EvalAltResult>> {
            std::fs::write(path, content).map_err(|e| format!("fs::write error: {}", e).into())
        },
    );

    module.set_native_fn(
        "exists",
        |path: &str| -> Result<bool, Box<rhai::EvalAltResult>> {
            Ok(std::path::Path::new(path).exists())
        },
    );

    module.set_native_fn(
        "glob",
        |pattern: &str| -> Result<rhai::Array, Box<rhai::EvalAltResult>> {
            let paths =
                glob::glob(pattern).map_err(|e| format!("fs::glob pattern error: {}", e))?;
            let result: rhai::Array = paths
                .filter_map(|r| r.ok())
                .map(|p| Dynamic::from(p.display().to_string()))
                .collect();
            Ok(result)
        },
    );

    module.set_native_fn(
        "ls",
        |path: &str| -> Result<rhai::Array, Box<rhai::EvalAltResult>> {
            let entries = std::fs::read_dir(path).map_err(|e| format!("fs::ls error: {}", e))?;
            let result: rhai::Array = entries
                .filter_map(|e| e.ok())
                .map(|e| Dynamic::from(e.file_name().to_string_lossy().to_string()))
                .collect();
            Ok(result)
        },
    );

    // The three below exist for agent resolvers introspecting session logs: find the newest
    // one, read an end of it, and judge staleness — all without slurping a multi-megabyte
    // JSONL into the script.

    // Path of the most recently modified file in `dir` with extension `ext` (`""` for any).
    module.set_native_fn(
        "newest",
        |dir: &str, ext: &str| -> Result<String, Box<rhai::EvalAltResult>> {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return Ok(String::new());
            };
            let mut best: Option<(std::path::PathBuf, std::time::SystemTime)> = None;
            for entry in entries.flatten() {
                let path = entry.path();
                if !ext.is_empty() && path.extension().and_then(|e| e.to_str()) != Some(ext) {
                    continue;
                }
                let Ok(modified) = path.metadata().and_then(|m| m.modified()) else {
                    continue;
                };
                if best.as_ref().is_none_or(|(_, t)| modified > *t) {
                    best = Some((path, modified));
                }
            }
            Ok(best
                .map(|(p, _)| p.display().to_string())
                .unwrap_or_default())
        },
    );

    // First non-empty line of a file.
    module.set_native_fn(
        "head",
        |path: &str| -> Result<String, Box<rhai::EvalAltResult>> {
            use std::io::BufRead;
            let Ok(file) = std::fs::File::open(path) else {
                return Ok(String::new());
            };
            for line in std::io::BufReader::new(file).lines().map_while(Result::ok) {
                if !line.trim().is_empty() {
                    return Ok(line);
                }
            }
            Ok(String::new())
        },
    );

    // Last non-empty line of a file, read by seeking from the end.
    module.set_native_fn(
        "tail",
        |path: &str| -> Result<String, Box<rhai::EvalAltResult>> {
            Ok(crate::fsutil::read_last_line(std::path::Path::new(path)).unwrap_or_default())
        },
    );

    // Seconds since a file was last modified; -1 when it doesn't exist.
    module.set_native_fn(
        "age_secs",
        |path: &str| -> Result<i64, Box<rhai::EvalAltResult>> {
            let Ok(modified) = std::fs::metadata(path).and_then(|m| m.modified()) else {
                return Ok(-1);
            };
            Ok(modified.elapsed().map(|d| d.as_secs() as i64).unwrap_or(0))
        },
    );

    module
}

/// Build the `path` module: `path::join`, `path::parent`, `path::filename`, `path::ext`
fn build_path_module() -> Module {
    let mut module = Module::new();

    module.set_native_fn(
        "join",
        |a: &str, b: &str| -> Result<String, Box<rhai::EvalAltResult>> {
            Ok(std::path::Path::new(a).join(b).display().to_string())
        },
    );

    module.set_native_fn(
        "parent",
        |p: &str| -> Result<String, Box<rhai::EvalAltResult>> {
            Ok(std::path::Path::new(p)
                .parent()
                .map(|pp| pp.display().to_string())
                .unwrap_or_default())
        },
    );

    module.set_native_fn(
        "filename",
        |p: &str| -> Result<String, Box<rhai::EvalAltResult>> {
            Ok(std::path::Path::new(p)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string())
        },
    );

    module.set_native_fn(
        "ext",
        |p: &str| -> Result<String, Box<rhai::EvalAltResult>> {
            Ok(std::path::Path::new(p)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_string())
        },
    );

    module
}

/// Build the `toml` module: `toml::parse(text)`
fn build_toml_module() -> Module {
    let mut module = Module::new();

    module.set_native_fn(
        "parse",
        |text: &str| -> Result<Dynamic, Box<rhai::EvalAltResult>> {
            let value: toml::Value =
                toml::from_str(text).map_err(|e| format!("TOML parse error: {}", e))?;
            // Convert toml::Value -> serde_json::Value -> Dynamic
            let json_value = serde_json::to_value(&value)
                .map_err(|e| format!("TOML to JSON conversion error: {}", e))?;
            rhai::serde::to_dynamic(&json_value)
                .map_err(|e| format!("JSON to Rhai conversion error: {}", e).into())
        },
    );

    module
}

/// Build the `http` module: `http::get`, `http::post`, `http::put`, `http::patch`, `http::delete`
///
/// Each method returns `#{ status, body, ok }`
fn build_http_module() -> Module {
    let mut module = Module::new();

    // GET with no opts
    module.set_native_fn(
        "get",
        |url: &str| -> Result<Map, Box<rhai::EvalAltResult>> {
            http_no_body("GET", url, &Map::new())
        },
    );

    // GET with opts (headers only, no body)
    module.set_native_fn(
        "get",
        |url: &str, opts: Map| -> Result<Map, Box<rhai::EvalAltResult>> {
            http_no_body("GET", url, &opts)
        },
    );

    // POST
    module.set_native_fn(
        "post",
        |url: &str, opts: Map| -> Result<Map, Box<rhai::EvalAltResult>> {
            http_with_body("POST", url, &opts)
        },
    );

    // PUT
    module.set_native_fn(
        "put",
        |url: &str, opts: Map| -> Result<Map, Box<rhai::EvalAltResult>> {
            http_with_body("PUT", url, &opts)
        },
    );

    // PATCH
    module.set_native_fn(
        "patch",
        |url: &str, opts: Map| -> Result<Map, Box<rhai::EvalAltResult>> {
            http_with_body("PATCH", url, &opts)
        },
    );

    // DELETE with no opts
    module.set_native_fn(
        "delete",
        |url: &str| -> Result<Map, Box<rhai::EvalAltResult>> {
            http_no_body("DELETE", url, &Map::new())
        },
    );

    // DELETE with opts
    module.set_native_fn(
        "delete",
        |url: &str, opts: Map| -> Result<Map, Box<rhai::EvalAltResult>> {
            http_no_body("DELETE", url, &opts)
        },
    );

    module
}

fn make_http_agent() -> ureq::Agent {
    ureq::Agent::new_with_config(
        ureq::config::Config::builder()
            .timeout_global(Some(std::time::Duration::from_secs(30)))
            .http_status_as_error(false)
            .build(),
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
        _ => {
            return Err(format!("http_with_body called with unsupported method: {}", method).into())
        }
    };

    for (k, v) in apply_headers(opts) {
        request = request.header(&k, &v);
    }

    // Determine body
    let body_str = if let Some(json_val) = opts.get("json") {
        let json_value: serde_json::Value = rhai::serde::from_dynamic(json_val)
            .map_err(|e| format!("json serialization error: {}", e))?;
        request = request.header("Content-Type", "application/json");
        serde_json::to_string(&json_value).map_err(|e| format!("json stringify error: {}", e))?
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

/// Build the `toren` module: `toren::config(key)`.
///
/// Read-only, and the only breq-owned data a resolver can see. Resolvers adapt external
/// systems; workspace state reaches them as function arguments, not ambient lookups.
fn build_toren_module() -> rhai::Shared<Module> {
    let mut module = Module::new();

    module.set_native_fn(
        "config",
        |key: &str| -> Result<String, Box<rhai::EvalAltResult>> { config_impl(key) },
    );

    module.into()
}

// ── Shared implementations ──────────────────────────────────────────────────

fn config_impl(key: &str) -> Result<String, Box<rhai::EvalAltResult>> {
    let config = crate::Config::load().map_err(|e| format!("Failed to load config: {}", e))?;
    config_value(&config, key)
}

/// One dotted key out of a loaded config, as a string.
fn config_value(config: &crate::Config, key: &str) -> Result<String, Box<rhai::EvalAltResult>> {
    // Virtual key for backwards compat: tasks.default_source -> first element of sources
    if key == "tasks.default_source" {
        return Ok(config
            .tasks
            .default_source()
            .unwrap_or_default()
            .to_string());
    }

    let json_value =
        serde_json::to_value(config).map_err(|e| format!("Failed to serialize config: {}", e))?;

    // Traverse dot-path segments
    let mut current = &json_value;
    for segment in key.split('.') {
        match current {
            serde_json::Value::Object(map) => {
                current = map
                    .get(segment)
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

// ── Flat aliases for backwards compatibility (DEPRECATED) ───────────────────

fn register_flat_aliases(engine: &mut Engine) {
    register_json_parse_alias(engine);
    register_shell_status_alias(engine);

    // config(key) -> toren::config(key)
    engine.register_fn(
        "config",
        |key: &str| -> Result<String, Box<rhai::EvalAltResult>> { config_impl(key) },
    );
}

fn register_json_parse_alias(engine: &mut Engine) {
    engine.register_fn(
        "json_parse",
        |text: &str| -> Result<Dynamic, Box<rhai::EvalAltResult>> {
            let value: serde_json::Value =
                serde_json::from_str(text).map_err(|e| format!("JSON parse error: {}", e))?;
            rhai::serde::to_dynamic(&value)
                .map_err(|e| format!("JSON to Rhai conversion error: {}", e).into())
        },
    );
}

fn register_shell_status_alias(engine: &mut Engine) {
    engine.register_fn(
        "shell_status",
        |program: &str, args: rhai::Array| -> Result<i64, Box<rhai::EvalAltResult>> {
            let str_args: Vec<String> = args
                .into_iter()
                .map(|a| a.into_string().unwrap_or_default())
                .collect();
            let status = std::process::Command::new(program)
                .args(&str_args)
                .status()
                .map_err(|e| format!("Failed to run '{}': {}", program, e))?;
            Ok(status.code().unwrap_or(-1) as i64)
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_json_parse_via_engine() {
        let ctx = Arc::new(PluginContext::default());
        let engine = create_engine(ctx);
        let ast = engine
            .compile(r#"let v = json_parse("{\"a\": 1}"); v.a"#)
            .unwrap();
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
        let ast = engine
            .compile(r#"env("__TOREN_NONEXISTENT_VAR__")"#)
            .unwrap();
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
        // Compiled, not evaluated: the flat alias reads the user's own config, and
        // `test_toren_config_module` covers the lookup itself.
        let ctx = Arc::new(PluginContext::default());
        let engine = create_engine(ctx);
        engine.compile(r#"config("tasks.default_source")"#).unwrap();
    }

    #[test]
    fn test_parse_args_bool_flag() {
        let ctx = Arc::new(PluginContext::default());
        let engine = create_engine(ctx);
        let ast = engine
            .compile(
                r#"
            let p = parse_args(["--push"], #{ push: #{ type: "bool" } });
            [p.opts.push, p.args.len()]
        "#,
            )
            .unwrap();
        let result: rhai::Array = engine.eval_ast(&ast).unwrap();
        assert!(result[0].clone().cast::<bool>());
        assert_eq!(result[1].clone().cast::<i64>(), 0);
    }

    #[test]
    fn test_parse_args_bool_default_false() {
        let ctx = Arc::new(PluginContext::default());
        let engine = create_engine(ctx);
        let ast = engine
            .compile(
                r#"
            let p = parse_args([], #{ push: #{ type: "bool" } });
            p.opts.push
        "#,
            )
            .unwrap();
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
        let ast = engine
            .compile(
                r#"
            let p = parse_args(["-s", "toren"], #{ segment: #{ type: "string", short: "s" } });
            p.opts.segment
        "#,
            )
            .unwrap();
        let result: String = engine.eval_ast(&ast).unwrap();
        assert_eq!(result, "toren");
    }

    #[test]
    fn test_parse_args_int_option() {
        let ctx = Arc::new(PluginContext::default());
        let engine = create_engine(ctx);
        let ast = engine
            .compile(
                r#"
            let p = parse_args(["--count", "10"], #{ count: #{ type: "int", default_val: 5 } });
            p.opts.count
        "#,
            )
            .unwrap();
        let result: i64 = engine.eval_ast(&ast).unwrap();
        assert_eq!(result, 10);
    }

    #[test]
    fn test_parse_args_int_default() {
        let ctx = Arc::new(PluginContext::default());
        let engine = create_engine(ctx);
        let ast = engine
            .compile(
                r#"
            let p = parse_args([], #{ count: #{ type: "int", default_val: 5 } });
            p.opts.count
        "#,
            )
            .unwrap();
        let result: i64 = engine.eval_ast(&ast).unwrap();
        assert_eq!(result, 5);
    }

    #[test]
    fn test_parse_args_string_absent_is_unit() {
        let ctx = Arc::new(PluginContext::default());
        let engine = create_engine(ctx);
        let ast = engine
            .compile(
                r#"
            let p = parse_args([], #{ name: #{ type: "string" } });
            p.opts.name == ()
        "#,
            )
            .unwrap();
        let result: bool = engine.eval_ast(&ast).unwrap();
        assert!(result);
    }

    #[test]
    fn test_parse_args_positional() {
        let ctx = Arc::new(PluginContext::default());
        let engine = create_engine(ctx);
        let ast = engine
            .compile(
                r#"
            let p = parse_args(["foo", "--push", "bar"], #{ push: #{ type: "bool" } });
            [p.args[0], p.args[1], p.opts.push]
        "#,
            )
            .unwrap();
        let result: rhai::Array = engine.eval_ast(&ast).unwrap();
        assert_eq!(result[0].clone().into_string().unwrap(), "foo");
        assert_eq!(result[1].clone().into_string().unwrap(), "bar");
        assert!(result[2].clone().cast::<bool>());
    }

    #[test]
    fn test_parse_args_double_dash_stops_parsing() {
        let ctx = Arc::new(PluginContext::default());
        let engine = create_engine(ctx);
        let ast = engine
            .compile(
                r#"
            let p = parse_args(["--", "--push"], #{ push: #{ type: "bool" } });
            [p.opts.push, p.args[0]]
        "#,
            )
            .unwrap();
        let result: rhai::Array = engine.eval_ast(&ast).unwrap();
        assert!(!result[0].clone().cast::<bool>());
        assert_eq!(result[1].clone().into_string().unwrap(), "--push");
    }

    #[test]
    fn test_parse_args_unknown_flag_errors() {
        let ctx = Arc::new(PluginContext::default());
        let engine = create_engine(ctx);
        let ast = engine
            .compile(
                r#"
            parse_args(["--unknown"], #{ push: #{ type: "bool" } })
        "#,
            )
            .unwrap();
        let result = engine.eval_ast::<Dynamic>(&ast);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("unknown option"), "Error was: {}", err);
    }

    #[test]
    fn test_parse_args_combined() {
        let ctx = Arc::new(PluginContext::default());
        let engine = create_engine(ctx);
        let ast = engine
            .compile(
                r#"
            let p = parse_args(
                ["task-123", "--push", "-i", "act"],
                #{
                    push: #{ type: "bool" },
                    intent: #{ type: "string", short: "i" },
                }
            );
            [p.args[0], p.opts.push, p.opts.intent]
        "#,
            )
            .unwrap();
        let result: rhai::Array = engine.eval_ast(&ast).unwrap();
        assert_eq!(result[0].clone().into_string().unwrap(), "task-123");
        assert!(result[1].clone().cast::<bool>());
        assert_eq!(result[2].clone().into_string().unwrap(), "act");
    }

    // ── New module tests ──────────────────────────────────────────────

    #[test]
    fn test_json_parse_module() {
        let ctx = Arc::new(PluginContext::default());
        let engine = create_engine(ctx);
        let ast = engine
            .compile(r#"let v = json::parse("{\"x\": 42}"); v.x"#)
            .unwrap();
        let result: i64 = engine.eval_ast(&ast).unwrap();
        assert_eq!(result, 42);
    }

    #[test]
    fn test_json_stringify_module() {
        let ctx = Arc::new(PluginContext::default());
        let engine = create_engine(ctx);
        let ast = engine
            .compile(r#"json::stringify(#{ a: 1, b: "hello" })"#)
            .unwrap();
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
        let ast = engine
            .compile(r#"fs::exists("/nonexistent_path_xyzzy")"#)
            .unwrap();
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
        let ast = engine
            .compile(r#"path::filename("/usr/local/bin/bash")"#)
            .unwrap();
        let result: String = engine.eval_ast(&ast).unwrap();
        assert_eq!(result, "bash");
    }

    #[test]
    fn test_path_ext_module() {
        let ctx = Arc::new(PluginContext::default());
        let engine = create_engine(ctx);
        let ast = engine
            .compile(r#"path::ext("/home/user/file.rs")"#)
            .unwrap();
        let result: String = engine.eval_ast(&ast).unwrap();
        assert_eq!(result, "rs");
    }

    #[test]
    fn test_toml_parse_module() {
        let ctx = Arc::new(PluginContext::default());
        let engine = create_engine(ctx);
        let ast = engine
            .compile(
                r#"
            let t = toml::parse("[section]\nkey = \"value\"\nnum = 42");
            [t.section.key, t.section.num]
        "#,
            )
            .unwrap();
        let result: rhai::Array = engine.eval_ast(&ast).unwrap();
        assert_eq!(result[0].clone().into_string().unwrap(), "value");
        assert_eq!(result[1].clone().cast::<i64>(), 42);
    }

    /// Evaluating this would load the user's own config, so the module is only compiled here
    /// and the lookup itself is exercised against a config the test owns.
    #[test]
    fn test_toren_config_module() {
        let ctx = Arc::new(PluginContext::default());
        let engine = create_engine(ctx);
        engine
            .compile(r#"toren::config("tasks.default_source")"#)
            .unwrap();

        let mut config = crate::Config::default();
        config.tasks.sources = vec!["runes".into()];
        assert_eq!(
            config_value(&config, "tasks.default_source").unwrap(),
            "runes"
        );
        assert_eq!(config_value(&config, "proxy.domain").unwrap(), "lvh.me");
        assert_eq!(config_value(&config, "server.port").unwrap(), "8787");
        assert!(config_value(&config, "nope.nothing").is_err());
    }

    #[test]
    fn test_shell_extended_overload() {
        let ctx = Arc::new(PluginContext::default());
        let engine = create_engine(ctx);
        let ast = engine
            .compile(
                r#"
            let r = shell("echo", ["hello"], #{});
            [r.stdout, r.status]
        "#,
            )
            .unwrap();
        let result: rhai::Array = engine.eval_ast(&ast).unwrap();
        assert_eq!(result[0].clone().into_string().unwrap(), "hello");
        assert_eq!(result[1].clone().cast::<i64>(), 0);
    }

    #[test]
    fn test_shell_extended_failure() {
        let ctx = Arc::new(PluginContext::default());
        let engine = create_engine(ctx);
        let ast = engine
            .compile(
                r#"
            let r = shell("false", [], #{});
            r.status
        "#,
            )
            .unwrap();
        let result: i64 = engine.eval_ast(&ast).unwrap();
        assert_ne!(result, 0);
    }

    #[test]
    fn test_shell_extended_with_stdin() {
        let ctx = Arc::new(PluginContext::default());
        let engine = create_engine(ctx);
        let ast = engine
            .compile(
                r#"
            let r = shell("cat", [], #{ stdin: "piped input" });
            r.stdout
        "#,
            )
            .unwrap();
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
