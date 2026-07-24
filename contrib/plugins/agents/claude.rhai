/// Claude Code.
///
/// Agent resolvers answer two questions core can't: how to spawn the agent, and how to read
/// what it's doing. Everything else — sessions, windows, attaching — is agent-agnostic.
///
/// ctx: #{ prompt, model, auto_approve, session_id, workspace }

fn argv(ctx) {
    let args = ["claude"];
    if ctx.model != () && ctx.model != "" {
        args += ["--model", ctx.model];
    }
    if ctx.auto_approve == true {
        args += ["--dangerously-skip-permissions"];
    }
    if ctx.prompt != () && ctx.prompt != "" {
        args += [ctx.prompt];
    }
    args
}

fn resume_argv(ctx) {
    let args = ["claude", "--resume"];
    if ctx.session_id != () && ctx.session_id != "" {
        args += [ctx.session_id];
    }
    if ctx.model != () && ctx.model != "" {
        args += ["--model", ctx.model];
    }
    if ctx.prompt != () && ctx.prompt != "" {
        args += [ctx.prompt];
    }
    args
}

/// Claude keeps per-directory logs at ~/.claude/projects/<mangled-path>/<session-id>.jsonl
fn log_dir(ws_path) {
    let mangled = ws_path;
    mangled.replace("/", "-");
    mangled.replace(".", "-");
    env("HOME") + "/.claude/projects/" + mangled
}

fn newest_log(ws_path) {
    fs::newest(log_dir(ws_path), "jsonl")
}

/// "running" while Claude is mid-turn, "idle" otherwise, "" with no session at all.
///
/// Reads the last log entry rather than trusting file mtime, so a long tool call (a 90s test
/// run) still reads as running.
fn activity(ws_path) {
    let log = newest_log(ws_path);
    if log == "" {
        return "";
    }
    // A session untouched for five minutes is finished, whatever its last entry claims.
    if fs::age_secs(log) > 300 {
        return "idle";
    }

    let last = fs::tail(log);
    if last == "" {
        return "idle";
    }

    let entry = json::parse(last);
    let kind = if entry.type != () { entry.type } else { "" };

    if kind == "progress" {
        return "running";
    }
    if kind.contains("thinking") {
        return "running";
    }
    if kind == "assistant" || kind == "user" {
        let msg = entry.message;
        if msg == () {
            return "idle";
        }
        // A tool_use block means a tool is executing; a tool_result means Claude is chewing
        // on its output. Either way the turn is still open.
        let content = msg.content;
        if type_of(content) == "array" {
            for block in content {
                let bt = if block.type != () { block.type } else { "" };
                if bt == "tool_use" || bt == "tool_result" {
                    return "running";
                }
            }
            return "idle";
        }
        if kind == "user" {
            return "running";
        }
    }
    "idle"
}

/// The agent's own summary of the session, used as a workspace title of last resort.
fn title(ws_path) {
    let log = newest_log(ws_path);
    if log == "" {
        return "";
    }
    let first = fs::head(log);
    if first == "" {
        return "";
    }
    let entry = json::parse(first);
    if entry.summary != () {
        return entry.summary;
    }
    let msg = entry.message;
    if msg != () && type_of(msg.content) == "string" {
        let text = msg.content;
        if text.len > 100 {
            return text.sub_string(0, 100);
        }
        return text;
    }
    ""
}

/// Claude names its logs after the session id, so the newest log's stem is the id.
fn session_id(ws_path) {
    let log = newest_log(ws_path);
    if log == "" {
        return "";
    }
    let name = path::filename(log);
    name.replace(".jsonl", "");
    name
}
