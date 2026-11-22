//! The vendored swarm payload.
//!
//! Eight files, a few KB, `include_str!`'d into the binary. They are VibeDesk's
//! own contract with the swarm, so a version skew between them and the engine is
//! a correctness bug rather than a preference. That is why they are vendored and
//! Aegis is not.
//!
//! The split matters: Vibe reads `~/.vibe/agents/*.toml` and
//! `~/.vibe/prompts/*.md`, and an agent TOML names its prompt through
//! `system_prompt_id`. A TOML without its MD is an agent that fails to load, so
//! the two are written together or not at all.

/// Which `~/.vibe` directory a payload file belongs in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Agents,
    Prompts,
}

impl Dir {
    pub fn name(self) -> &'static str {
        match self {
            Dir::Agents => "agents",
            Dir::Prompts => "prompts",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct File {
    pub name: &'static str,
    pub dir: Dir,
    pub body: &'static str,
}

pub const FILES: &[File] = &[
    File {
        name: "vibedesk-swarm-lead.toml",
        dir: Dir::Agents,
        body: include_str!("payload/vibedesk-swarm-lead.toml"),
    },
    File {
        name: "vibedesk-swarm-worker.toml",
        dir: Dir::Agents,
        body: include_str!("payload/vibedesk-swarm-worker.toml"),
    },
    File {
        name: "vibedesk-swarm-verifier.toml",
        dir: Dir::Agents,
        body: include_str!("payload/vibedesk-swarm-verifier.toml"),
    },
    File {
        name: "vibedesk-swarm-reflector.toml",
        dir: Dir::Agents,
        body: include_str!("payload/vibedesk-swarm-reflector.toml"),
    },
    File {
        name: "vibedesk-swarm-lead.md",
        dir: Dir::Prompts,
        body: include_str!("payload/vibedesk-swarm-lead.md"),
    },
    File {
        name: "vibedesk-swarm-worker.md",
        dir: Dir::Prompts,
        body: include_str!("payload/vibedesk-swarm-worker.md"),
    },
    File {
        name: "vibedesk-swarm-verifier.md",
        dir: Dir::Prompts,
        body: include_str!("payload/vibedesk-swarm-verifier.md"),
    },
    File {
        name: "vibedesk-swarm-reflector.md",
        dir: Dir::Prompts,
        body: include_str!("payload/vibedesk-swarm-reflector.md"),
    },
];

/// The agent name Vibe will advertise as a mode, from the TOML's file stem.
pub fn agent_name(file: &File) -> &'static str {
    file.name.trim_end_matches(".toml").trim_end_matches(".md")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_agent_toml_has_its_prompt() {
        let agents: Vec<_> = FILES.iter().filter(|f| f.dir == Dir::Agents).collect();
        let prompts: Vec<_> = FILES.iter().filter(|f| f.dir == Dir::Prompts).collect();
        assert_eq!(agents.len(), 4);
        assert_eq!(prompts.len(), 4);
        for agent in agents {
            let stem = agent.name.trim_end_matches(".toml");
            let prompt = format!("{stem}.md");
            assert!(
                prompts.iter().any(|p| p.name == prompt),
                "{stem}.toml has no {prompt}; the agent would fail to load"
            );
        }
    }

    #[test]
    fn every_toml_names_the_prompt_that_ships_beside_it() {
        for file in FILES.iter().filter(|f| f.dir == Dir::Agents) {
            let stem = file.name.trim_end_matches(".toml");
            let expected = format!("system_prompt_id = \"{stem}\"");
            assert!(
                file.body.contains(&expected),
                "{} must contain {expected}",
                file.name
            );
        }
    }

    #[test]
    fn no_prompt_is_empty_and_none_carries_a_placeholder() {
        for file in FILES {
            assert!(!file.body.trim().is_empty(), "{} is empty", file.name);
            for marker in ["TODO", "TBD", "FIXME", "lorem ipsum"] {
                assert!(
                    !file.body.contains(marker),
                    "{} still carries {marker}",
                    file.name
                );
            }
        }
    }

    #[test]
    fn the_lead_prompt_forbids_implementing() {
        let lead = FILES
            .iter()
            .find(|f| f.name == "vibedesk-swarm-lead.md")
            .expect("the lead prompt ships");
        assert!(lead.body.contains("You do not implement"));
        assert!(lead.body.contains("You have no worktree and no task"));
    }

    #[test]
    fn agent_names_are_namespaced_so_a_user_agent_is_never_clobbered() {
        for file in FILES.iter().filter(|f| f.dir == Dir::Agents) {
            assert!(
                agent_name(file).starts_with("vibedesk-swarm-"),
                "{} is not namespaced",
                file.name
            );
        }
    }
}
