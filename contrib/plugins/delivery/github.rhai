/// GitHub pull requests and checks, via the `gh` CLI.
///
/// Delivery resolvers answer "what happened to this branch after it left the workspace".
/// Read-only and slow (network), so breq caches the result with a timestamp and never calls
/// it on the `breq list` hot path — only on `--refresh` or from the daemon's poll.
///
/// ctx: #{ path, branches }  — branches are VCS-derived, e.g. "feature@origin" (jj) or
/// "origin/feature" (git).
/// returns: array of #{ branch, id, url, state, ci }

/// "feature@origin" / "origin/feature" -> "feature"
fn local_name(branch) {
    let name = branch;
    if name.contains("@") {
        let parts = name.split("@");
        return parts[0];
    }
    if name.contains("/") {
        let parts = name.split("/");
        return parts[parts.len() - 1];
    }
    name
}

/// GitHub's own verdict on merged-ness — no squash-merge detection needed on our side.
fn prs(ctx) {
    let found = [];
    let seen = [];

    for branch in ctx.branches {
        let head = local_name(branch);
        if head == "" || seen.contains(head) {
            continue;
        }
        seen.push(head);

        let out = shell(
            "gh",
            [
                "pr", "list", "--head", head, "--state", "all", "--limit", "5",
                "--json", "number,url,state,headRefName,statusCheckRollup",
            ],
            #{ dir: ctx.path },
        );
        if out.status != 0 {
            continue;
        }

        let prs = json::parse(out.stdout);
        for pr in prs {
            found.push(#{
                branch: pr.headRefName,
                id: "#" + pr.number,
                url: pr.url,
                state: pr.state.to_lower(),
                ci: rollup(pr.statusCheckRollup),
            });
        }
    }

    found
}

/// Collapse the check rollup into one word: any failure wins, then anything pending.
fn rollup(checks) {
    if checks == () || checks.len() == 0 {
        return "";
    }

    let pending = false;
    for check in checks {
        let state = if check.conclusion != () && check.conclusion != "" {
            check.conclusion
        } else if check.state != () {
            check.state
        } else {
            ""
        };
        let s = state.to_lower();
        if s == "failure" || s == "timed_out" || s == "cancelled" || s == "action_required" {
            return "failing";
        }
        if s == "pending" || s == "in_progress" || s == "queued" || s == "" {
            pending = true;
        }
    }

    if pending { "pending" } else { "passing" }
}
