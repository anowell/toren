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
    let args = ["new", title, "--status", "todo"];
    if desc != () {
        args += ["-f", "-"];
    }
    shell("runes", args)
}
