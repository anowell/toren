// abort — clean workspace and reopen the bead.
//
// Usage: breq abort <workspace>
//
// 1. Cleans the workspace (kill processes, remove)
// 2. Reopens the bead (status -> open, assignee -> "")

let ws = ARGS[0];

// Clean workspace with kill (no push)
let result = breq_clean_with(ws, #{ kill: true, push: false });

// Reopen and unassign the bead if we have a task ID
if result.id != () {
    shell("bd", ["update", result.id, "--status", "open", "--assignee", ""]);
}
