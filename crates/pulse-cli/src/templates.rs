//! The demo rules shipped in `templates/`, for `pulse init --template`.

pub struct Template {
    pub name: &'static str,
    pub summary: &'static str,
    pub text: &'static str,
}

pub const TEMPLATES: &[Template] = &[
    Template {
        name: "lecture-notes",
        summary: "a new lecture PDF -> a summary and flashcards next to it (Claude Code)",
        text: include_str!("../../../templates/lecture-notes.toml"),
    },
    Template {
        name: "build-failure",
        summary: "a build wrapped with `pulse run` fails -> a diagnosis (Claude Code, read-only)",
        text: include_str!("../../../templates/build-failure.toml"),
    },
];

pub const STARTER: &str = r#"# Pulse configuration: sources watch the machine, rules wake agents.
#
# The daemon reloads this file when you save it; `pulse status` shows any errors, and the last
# good version stays in effect meanwhile. Edits made from the UI or CLI are written back here,
# keeping your comments.
#
# Add a ready-made rule:  pulse init --template lecture-notes
#                         pulse init --template build-failure
# List them:              pulse init --list
#
# [daemon]
# port = 7477
# log_level = "info"
#
# A minimal example: notify on every new file in ~/Downloads.
#
# [[source]]
# name = "downloads"
# kind = "fs"
# path = "~/Downloads"
#
# [[agent]]
# name = "notify"
# adapter = "command"
# command = ["notify-send", "Pulse", "--", "{{subject}}"]
# allow = ["fs.file.created:~/Downloads/**"]
#
# [[rule]]
# name = "new-download"
# on = "fs.file.created"
# where = { path = "~/Downloads/**" }
# run = { agent = "notify" }
"#;

pub fn find(name: &str) -> Option<&'static Template> {
    TEMPLATES.iter().find(|t| t.name == name)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use pulse_contracts::config::{ConfigOrigin, Sourced, parse_config_file, resolve};

    use super::*;

    #[test]
    fn templates_and_starter_are_valid_config_together() {
        let origin = ConfigOrigin::User { path: "/p/pulse.toml".into() };
        let home = Path::new("/home/me");
        let mut all = STARTER.to_owned();
        for t in TEMPLATES {
            let file = parse_config_file(t.text, &origin, home)
                .unwrap_or_else(|e| panic!("{}: {e}", t.name));
            let config = resolve(Sourced { origin: origin.clone(), config: file }, vec![])
                .unwrap_or_else(|e| panic!("{}: {e}", t.name));
            assert_eq!(config.rules.len(), 1, "{}", t.name);
            pulse_core::Routing::compile(&config, home).unwrap();
            all.push('\n');
            all.push_str(t.text);
        }
        // What `pulse init` writes when given every template.
        parse_config_file(&all, &origin, home).unwrap();
    }
}
