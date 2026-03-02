/// Claim a task and start a Claude session.
///
/// Usage: breq assign <task-id> [--intent <name>]
///
/// 1. Detects task source (from ID prefix or tasks.default_source config)
/// 2. Updates task status to in-progress via source-specific CLI
/// 3. Returns a deferred cmd action to start a Claude session
///
/// Options:
///   --intent, -i   Intent template to use (e.g., "act", "plan")
///
/// Supported sources: beads, github, linear

let parsed = parse_args(ARGS, #{
    intent: #{ type: "string", short: "i" },
});
let task_id = parsed.args[0];

// Infer task fields (source, title, url) from the ID
let task = task(task_id);
let source = if task.source != () { task.source } else { config("tasks.default_source") };

// Claim the task based on its source
switch source {
    "beads" => {
        shell("bd", ["update", task.id, "--status", "in_progress", "--assignee", "claude"]);
    },
    "github" => {
        // Assign to self and add in-progress label
        shell("gh", ["issue", "edit", task.id, "--add-assignee", "@me"]);
    },
    "linear" => {
        shell("linear", ["issue", "update", task.id, "--status", "In Progress"]);
    },
    _ => {
        print(`Warning: unknown task source '${source}', skipping status update`);
    },
}

// Return deferred action to start a Claude session
// The task description serves as the prompt (bead is the prompt)
let result = #{
    action: "cmd",
    task_id: task.id,
    task_title: task.title,
};
if task.description != () {
    result.prompt = task.description;
}
if task.url != () {
    result.task_url = task.url;
}
if parsed.opts.intent != () {
    result.intent = parsed.opts.intent;
}
result
