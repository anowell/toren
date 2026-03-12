/// GitHub Issues task tracking. Resolves tasks via the `gh` CLI.

fn info(id) {
    let result = shell("gh", ["issue", "view", id, "--json", "number,title,state,assignees,body,url"]);
    let issue = json::parse(result);
    let assignee = if issue.assignees.len() > 0 { issue.assignees[0].login } else { "" };
    let status = if issue.state == "OPEN" { "open" } else { "closed" };
    #{
        id: `${issue.number}`,
        title: issue.title,
        status: status,
        assignee: assignee,
        description: if issue.body != () { issue.body } else { "" },
        url: issue.url
    }
}

fn claim(id, assignee) {
    // Non-throwing: assignee may not be a valid GitHub login
    shell("gh", ["issue", "edit", id, "--add-assignee", assignee], #{});
}

fn complete(id) {
    shell("gh", ["issue", "close", id]);
}

fn abort(id) {
    // Non-throwing: issue may already be open
    shell("gh", ["issue", "reopen", id], #{});
}

fn create(title, desc) {
    let args = ["issue", "create", "--title", title];
    if desc != () {
        args += ["--body", desc];
    }
    let url = shell("gh", args);
    let parts = url.split("/");
    parts[parts.len() - 1]
}
