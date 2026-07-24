/// OpenCode.
///
/// ctx: #{ prompt, model, auto_approve, session_id, workspace }

fn argv(ctx) {
    let args = ["opencode"];
    if ctx.model != () && ctx.model != "" {
        args += ["--model", ctx.model];
    }
    if ctx.prompt != () && ctx.prompt != "" {
        args += [ctx.prompt];
    }
    args
}

fn resume_argv(ctx) {
    let args = ["opencode", "--continue"];
    if ctx.prompt != () && ctx.prompt != "" {
        args += [ctx.prompt];
    }
    args
}

fn activity(ws_path) {
    ""
}

fn title(ws_path) {
    ""
}

fn session_id(ws_path) {
    ""
}
