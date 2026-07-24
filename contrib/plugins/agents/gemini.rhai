/// Google Gemini CLI.
///
/// ctx: #{ prompt, model, auto_approve, session_id, workspace }

fn argv(ctx) {
    let args = ["gemini"];
    if ctx.model != () && ctx.model != "" {
        args += ["--model", ctx.model];
    }
    if ctx.prompt != () && ctx.prompt != "" {
        args += [ctx.prompt];
    }
    args
}

fn resume_argv(ctx) {
    argv(ctx)
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
