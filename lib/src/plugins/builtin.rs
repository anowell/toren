//! Resolver plugins vendored into the binary.
//!
//! Agents and delivery forges need to work out of the box — `breq do -p "..."` cannot ask the
//! user to install a plugin first. So the common ones ship compiled in, and a user file of the
//! same name in `~/.toren/plugins/<family>/` shadows them completely. That makes these files
//! double as the reference implementations: copy one out, edit it, and it wins.
//!
//! Task resolvers are deliberately *not* vendored. Which tracker you use is a workflow choice
//! breq has no business defaulting.

use super::Family;

/// Vendored agent resolvers. Adding an agent is one file here (or in `~/.toren/plugins/agents/`).
const AGENTS: &[(&str, &str)] = &[
    (
        "claude",
        include_str!("../../../contrib/plugins/agents/claude.rhai"),
    ),
    (
        "codex",
        include_str!("../../../contrib/plugins/agents/codex.rhai"),
    ),
    (
        "gemini",
        include_str!("../../../contrib/plugins/agents/gemini.rhai"),
    ),
    (
        "opencode",
        include_str!("../../../contrib/plugins/agents/opencode.rhai"),
    ),
    (
        "pi",
        include_str!("../../../contrib/plugins/agents/pi.rhai"),
    ),
];

/// Vendored delivery resolvers.
const DELIVERY: &[(&str, &str)] = &[(
    "github",
    include_str!("../../../contrib/plugins/delivery/github.rhai"),
)];

/// The vendored plugins for a family, as `(name, source)`.
pub fn for_family(family: Family) -> &'static [(&'static str, &'static str)] {
    match family {
        Family::Agents => AGENTS,
        Family::Delivery => DELIVERY,
        Family::Tasks => &[],
    }
}
