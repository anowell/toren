/// Runes task tracking. Resolves tasks via the `runes` CLI.

fn info(id) {
    let result = shell("runes", ["show", id, "--json"]);
    let rune = json::parse(result);
    let status = if rune.status != () { rune.status } else { "todo" };
    let assignee = if rune.assignee != () { rune.assignee } else { "" };
    #{ id: rune.id, kind: rune.kind, title: rune.title, status: status, assignee: assignee, description: rune.description }
}

fn claim(id, assignee) {
    shell("runes", ["edit", id, "--status", "in-progress", "--assignee", assignee]);
}

fn complete(id) {
    shell("runes", ["edit", id, "--status", "done"]);
}

fn abort(id) {
    shell("runes", ["edit", id, "--status", "todo", "--assignee", "none"]);
}

fn create(title, desc) {
    // --commit: automated creation has no human to edit-then-commit the draft
    let args = ["new", title, "--status", "todo", "--commit"];
    let opts = #{};
    if desc != () {
        args += ["-f", "-"];
        opts.stdin = desc;
    }
    let result = shell("runes", args, opts);
    if result.status != 0 {
        throw "runes new failed: " + result.stderr;
    }
    // runes new prints the id on the first line; path and status lines may follow
    let id = result.stdout;
    let nl = id.index_of('\n');
    if nl >= 0 {
        id = id.sub_string(0, nl);
    }
    id.trim();
    id
}
