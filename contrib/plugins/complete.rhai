// complete — clean workspace, push changes, and close the bead.
//
// Usage: breq complete <workspace>
//
// 1. Cleans the workspace (auto-commit, capture revision, push, remove)
// 2. Closes the bead (status -> closed)

let ws = ARGS[0];

// Clean workspace with push and kill
let result = breq_clean_with(ws, #{ kill: true, push: true });

// Close the bead if we have a task ID
if result.id != () {
    shell("bd", ["update", result.id, "--status", "closed"]);
}
