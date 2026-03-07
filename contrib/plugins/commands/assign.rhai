/// Claim a task and start a coding agent session.
///
/// Usage: breq assign <task-id> [--intent <name>]
///
/// 1. Resolves task via installed task plugins
/// 2. Claims the task via its resolver (updates status/assignee)
/// 3. Returns a deferred action to start a coding agent session
///
/// Options:
///   --intent, -i   Intent template to use (e.g., "act", "plan")

let parsed = parse_args(ARGS, #{
    intent: #{ type: "string", short: "i" },
});
let task_id = parsed.args[0];

// Resolve task fields (source, title, description) via task plugins
let t = task::info(task_id);

// Claim the task via its resolver
if t.source != () {
    task::claim(t.source, t.id, "claude");
}

// Build prompt with task context
let prompt = `${t.source} ${t.id}: ${t.title}`;
if t.description != () {
    prompt += "\n\n" + t.description;
}

// Return deferred action to start a coding agent session
let result = #{
    action: "do",
    task_id: t.id,
    task_title: t.title,
    prompt: prompt,
};
if t.url != () {
    result.task_url = t.url;
}
if parsed.opts.intent != () {
    result.intent = parsed.opts.intent;
}
result
