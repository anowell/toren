/// Clean workspace and reopen the task.
///
/// Usage: breq abort <workspace>
///
/// 1. Resolves workspace to its active assignment
/// 2. Cleans the workspace (kill processes, no push)
/// 3. Reopens the task via its resolver plugin

let ws = ARGS[0];

// Get assignment info before cleaning
let info = toren::assignment(ws);
let task_id = info.task_id;
let source = if info.task_source != "" { info.task_source } else { toren::config("tasks.default_source") };

// Discover source from task resolvers if still unknown
if source == "" && task_id != "" {
    let resolved = task::info(task_id);
    if resolved.contains("source") { source = resolved.source; }
}

// Clean workspace with kill (no push)
shell("breq", ["clean", ws, "--kill"]);

// Reopen the task if we have a task ID and source
if task_id != "" && source != "" {
    task::abort(source, task_id);
}
