/// Clean workspace and close the task.
///
/// Usage: breq complete <workspace>
///
/// 1. Resolves workspace to its active assignment
/// 2. Cleans the workspace (auto-commit, push, kill processes)
/// 3. Closes the task via its resolver plugin

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

// Clean workspace with push and kill
shell("breq", ["clean", ws, "--kill"]);

// Close the task if we have a task ID and source
if task_id != "" && source != "" {
    task::complete(source, task_id);
}
