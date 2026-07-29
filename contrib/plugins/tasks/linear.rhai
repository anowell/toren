/// Linear task tracking. Resolves tasks via the Linear GraphQL API.
/// Requires LINEAR_API_KEY environment variable.

fn linear_request(body) {
    let key = env("LINEAR_API_KEY");
    if key == "" { throw "LINEAR_API_KEY not set"; }
    let resp = http::post("https://api.linear.app/graphql", #{
        headers: #{ "Authorization": key },
        json: body
    });
    if !resp.ok { throw "Linear API error: " + resp.body; }
    let data = json::parse(resp.body);
    if data.errors != () { throw "Linear: " + json::stringify(data.errors); }
    data.data
}

fn linear_gql(query) {
    linear_request(#{ "query": query })
}

fn linear_gql(query, vars) {
    linear_request(#{ "query": query, "variables": vars })
}

fn issue_with_states(id) {
    let data = linear_gql(`{ issues(filter: { identifier: { eq: "${id}" } }) { nodes { id identifier team { states { nodes { id name type } } } } } }`);
    if data.issues.nodes.len() == 0 { throw "Not found: " + id; }
    data.issues.nodes[0]
}

fn state_by_type(issue, state_type) {
    for s in issue.team.states.nodes {
        if s.type == state_type { return s.id; }
    }
    throw "No '" + state_type + "' workflow state found";
}

fn state_by_name(issue, name) {
    for s in issue.team.states.nodes {
        if s.name == name { return s.id; }
    }
    ()
}

fn info(id) {
    let data = linear_gql(`{ issues(filter: { identifier: { eq: "${id}" } }) { nodes { identifier title description url state { name } assignee { displayName } } } }`);
    if data.issues.nodes.len() == 0 { throw "Not found: " + id; }
    let issue = data.issues.nodes[0];
    #{
        id: issue.identifier,
        title: issue.title,
        status: issue.state.name,
        assignee: if issue.assignee != () { issue.assignee.displayName } else { "" },
        description: if issue.description != () { issue.description } else { "" },
        url: issue.url
    }
}

fn claim(id, assignee) {
    let issue = issue_with_states(id);
    let state_id = state_by_type(issue, "started");
    linear_gql(`mutation { issueUpdate(id: "${issue.id}", input: { stateId: "${state_id}" }) { success } }`);
}

fn set_field(id, field, value) {
    if field == "status" {
        let issue = issue_with_states(id);
        let state_id = state_by_name(issue, value);
        if state_id == () { state_id = state_by_type(issue, value); }
        linear_gql(`mutation { issueUpdate(id: "${issue.id}", input: { stateId: "${state_id}" }) { success } }`);
    } else if field == "assignee" {
        let issue = issue_with_states(id);
        if value == "none" || value == "" {
            linear_gql(`mutation { issueUpdate(id: "${issue.id}", input: { assigneeId: null }) { success } }`);
        } else {
            let users = linear_gql(`{ users(filter: { displayName: { eq: "${value}" } }) { nodes { id } } }`);
            if users.users.nodes.len() == 0 { throw `linear: no user named "${value}"`; }
            linear_gql(`mutation { issueUpdate(id: "${issue.id}", input: { assigneeId: "${users.users.nodes[0].id}" }) { success } }`);
        }
    } else if field == "title" {
        let issue = issue_with_states(id);
        linear_gql(`mutation { issueUpdate(id: "${issue.id}", input: { title: "${value}" }) { success } }`);
    } else {
        throw `linear: cannot write field "${field}"`;
    }
}

fn create(title, desc) {
    let viewer = linear_gql("{ viewer { teamMemberships(first: 1) { nodes { team { id } } } } }");
    let team_id = viewer.viewer.teamMemberships.nodes[0].team.id;
    let input = #{ "teamId": team_id, "title": title };
    if desc != () { input.description = desc; }
    let data = linear_gql(
        "mutation($input: IssueCreateInput!) { issueCreate(input: $input) { issue { identifier } } }",
        #{ "input": input }
    );
    data.issueCreate.issue.identifier
}
