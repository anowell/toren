/// OpenAI Codex CLI.
///
/// ctx: #{ prompt, model, auto_approve, session_id, workspace }

fn argv(ctx) {
    let args = ["codex"];
    if ctx.model != () && ctx.model != "" {
        args += ["-m", ctx.model];
    }
    if ctx.auto_approve == true {
        args += ["--full-auto"];
    }
    if ctx.prompt != () && ctx.prompt != "" {
        args += [ctx.prompt];
    }
    args
}

fn resume_argv(ctx) {
    let args = ["codex", "resume"];
    if ctx.session_id != () && ctx.session_id != "" {
        args += [ctx.session_id];
    } else {
        args += ["--last"];
    }
    args
}

/// Codex records rollouts under ~/.codex/sessions; it has no per-workspace index, so
/// liveness comes from the pane and only recency is available here.
fn activity(ws_path) {
    ""
}

fn title(ws_path) {
    ""
}

fn session_id(ws_path) {
    ""
}
