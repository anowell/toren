/// Clean workspace, push changes, and close the task.
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

// Clean workspace with push and kill
shell("breq", ["clean", ws, "--kill", "--push"]);

// Close the task if we have a task ID
if task_id != "" {
    task::complete(source, task_id);
}
