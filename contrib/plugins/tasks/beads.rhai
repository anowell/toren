/// Beads task tracking. Resolves tasks via the `bd` CLI.

fn info(id) {
    let result = shell("bd", ["show", id, "--json"]);
    let beads = json::parse(result);
    let bead = beads[0];
    let status = if bead.status != () { bead.status } else { "open" };
    let assignee = if bead.assignee != () { bead.assignee } else { "" };
    #{ id: bead.id, title: bead.title, status: status, assignee: assignee, description: bead.description }
}

fn claim(id, assignee) {
    shell("bd", ["update", id, "--status", "in_progress", "--assignee", assignee]);
}

fn set_field(id, field, value) {
    let unassigning = field == "assignee" && value == "none";
    shell("bd", ["update", id, "--" + field, if unassigning { "" } else { value }]);
}

fn create(title, desc) {
    let args = ["create", "--silent", "--title", title];
    if desc != () {
        args += ["--description", desc];
    }
    shell("bd", args)
}
