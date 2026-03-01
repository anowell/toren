// assign — claim a bead and start a Claude session.
//
// Usage: breq assign <bead-id>
//
// 1. Claims the bead (status -> in_progress, assignee -> claude)
// 2. Infers task fields (title, url) from the bead
// 3. Returns a deferred cmd action to start a Claude session

let bead_id = ARGS[0];

// Claim the bead
shell("bd", ["update", bead_id, "--status", "in_progress", "--assignee", "claude"]);

// Infer task fields (title etc.) from the bead ID
let task = task_infer(bead_id);

// Return deferred action to start a Claude session
let result = #{
    action: "cmd",
    task_id: task.id,
    task_title: task.title,
};
if task.url != () {
    result.task_url = task.url;
}
result
