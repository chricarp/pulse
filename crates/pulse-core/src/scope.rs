//! Permission scopes: does an agent's `allow` list cover an event?
//!
//! Rules, in order:
//! 1. **Default deny.** No grant whose type glob matches the event type: denied.
//! 2. A matching grant **without** a path part covers the event, whatever paths it carries.
//! 3. Otherwise only path-qualified grants match the type. The event must carry at least one
//!    path (a path-qualified grant cannot vouch for an event with none), and **every** path it
//!    carries (per the catalog, `Event::paths`) must be matched by one of those grants.
//!
//! What the core requires of its inputs: event paths are compared **as text**, and only clean
//! absolute paths can be in scope (see [`crate::path`]). A relative path, `..`, `.`, `//` or NUL
//! makes the event out of scope rather than being normalized, because lexical normalization is
//! wrong in the presence of symlinks. A single trailing slash is tolerated. Symlinks are not
//! resolved: a path is in scope if its text is under a granted glob. Matching is case-sensitive,
//! so on a case-insensitive filesystem a differently-cased path is denied, never granted.
//!
//! This bounds what an agent is *told*. It does not sandbox what a spawned agent can read.

use std::path::Path;

use pulse_contracts::config::Scope;
use pulse_contracts::pattern::{
    Glob, PatternError, clean_absolute, parse_path_glob, parse_type_glob,
};

use crate::glob;

#[derive(Clone, Debug)]
struct Grant {
    types: Glob,
    path: Option<Glob>,
}

#[derive(Clone, Debug, Default)]
pub struct ScopeSet {
    grants: Vec<Grant>,
}

/// Why an event is out of scope: a scope that would cover the uncovered part.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Denied {
    pub needed: String,
}

impl ScopeSet {
    pub fn compile(scopes: &[Scope], home: &Path) -> Result<ScopeSet, PatternError> {
        let grants = scopes
            .iter()
            .map(|s| {
                Ok(Grant {
                    types: parse_type_glob(s.types.as_str())?,
                    path: s.path.as_deref().map(|p| parse_path_glob(p, home)).transpose()?,
                })
            })
            .collect::<Result<_, PatternError>>()?;
        Ok(ScopeSet { grants })
    }

    /// Checks an event given its type and the paths it carries.
    pub fn check(&self, event_type: &str, paths: &[&str]) -> Result<(), Denied> {
        let typed: Vec<&Grant> =
            self.grants.iter().filter(|g| glob::matches(&g.types, event_type)).collect();
        let needed = |path: Option<&str>| Denied {
            needed: match path {
                Some(p) => format!("{event_type}:{p}"),
                None => event_type.to_owned(),
            },
        };
        if typed.is_empty() {
            return Err(needed(paths.first().copied()));
        }
        if typed.iter().any(|g| g.path.is_none()) {
            return Ok(());
        }
        if paths.is_empty() {
            return Err(needed(None));
        }
        for raw in paths {
            let clean =
                clean_absolute(raw).filter(|c| c.as_str() == *raw || format!("{c}/") == *raw);
            let covered = clean.as_deref().is_some_and(|p| {
                typed.iter().any(|g| g.path.as_ref().is_some_and(|pg| glob::matches(pg, p)))
            });
            if !covered {
                return Err(needed(Some(raw)));
            }
        }
        Ok(())
    }

    pub fn permits(&self, event: &pulse_contracts::Event) -> Result<(), Denied> {
        self.check(event.type_.as_str(), &event.paths())
    }

    /// Whether any event of a catalog type could ever be in scope: a grant matches the type, and
    /// either it has no path part or the type carries paths. A namespace written `prefix*` (the
    /// catalog's `ext.*`) counts when a grant can match some type in it. This is what an agent
    /// is shown as its catalog; it is not a permission check on an event.
    pub fn may_permit(&self, catalog_type: &str, carries_paths: bool) -> bool {
        self.grants.iter().any(|g| {
            let typed = match catalog_type.strip_suffix('*') {
                Some(prefix) => glob::may_match_prefix(&g.types, prefix),
                None => glob::matches(&g.types, catalog_type),
            };
            typed && (g.path.is_none() || carries_paths)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(scopes: &[&str]) -> ScopeSet {
        let scopes: Vec<Scope> = scopes.iter().map(|s| s.parse().unwrap()).collect();
        ScopeSet::compile(&scopes, Path::new("/home/me")).unwrap()
    }

    #[test]
    fn catalog_types_an_agent_may_receive() {
        // (grants, catalog type, carries paths, shown)
        let cases: &[(&[&str], &str, bool, bool)] = &[
            (&["fs.*:/tmp/x/**"], "fs.file.created", true, true),
            (&["*:/tmp/x/**"], "schedule.fired", false, false),
            (&["fs.*:/tmp/x/**"], "git.commit.created", true, false),
            (&["fs.*:/tmp/x/**"], "ext.*", false, false),
            (&["*"], "schedule.fired", false, true),
            (&["ext.ci.*"], "ext.*", false, true),
            (&["e*"], "ext.*", false, true),
            (&["{exit,ext}.ci.*"], "ext.*", false, true),
            (&["exit.*"], "ext.*", false, false),
            (&["*"], "ext.*", false, true),
            (&["*"], "calendar.event.starting", false, true),
            (&[], "fs.file.created", true, false),
        ];
        for (grants, ty, paths, want) in cases {
            assert_eq!(set(grants).may_permit(ty, *paths), *want, "{grants:?} {ty}");
        }
    }

    #[test]
    #[allow(clippy::type_complexity)]
    fn scope_table() {
        let uni = &["fs.*:~/University/**", "calendar.*"][..];
        // (grants, event type, paths, Ok or the `needed` scope)
        let cases: Vec<(&[&str], &str, Vec<&str>, Result<(), &str>)> = vec![
            (
                &[],
                "fs.file.created",
                vec!["/home/me/University/a.pdf"],
                Err("fs.file.created:/home/me/University/a.pdf"),
            ),
            (&[], "schedule.fired", vec![], Err("schedule.fired")),
            (
                uni,
                "fs.file.created",
                vec!["/home/me/University/a.pdf", "/home/me/University/a.pdf"],
                Ok(()),
            ),
            (uni, "fs.file.created", vec!["/home/me/University"], Ok(())),
            (uni, "fs.file.created", vec!["/home/me/University/"], Ok(())),
            (
                uni,
                "fs.file.created",
                vec!["/home/me/Downloads/a.pdf"],
                Err("fs.file.created:/home/me/Downloads/a.pdf"),
            ),
            (
                uni,
                "fs.file.created",
                vec!["/home/me/UniversityX/a"],
                Err("fs.file.created:/home/me/UniversityX/a"),
            ),
            // A move from outside the scope reveals the old path: every path must be covered.
            (
                uni,
                "fs.file.moved",
                vec!["/home/me/University/a", "/home/me/University/a", "/home/me/.ssh/id_rsa"],
                Err("fs.file.moved:/home/me/.ssh/id_rsa"),
            ),
            // Walking out with `..`, `.`, empty segments, relative or NUL paths fails closed.
            (
                uni,
                "fs.file.created",
                vec!["/home/me/University/../.ssh/id_rsa"],
                Err("fs.file.created:/home/me/University/../.ssh/id_rsa"),
            ),
            (
                uni,
                "fs.file.created",
                vec!["/home/me/University/./a"],
                Err("fs.file.created:/home/me/University/./a"),
            ),
            (
                uni,
                "fs.file.created",
                vec!["/home/me/University//a"],
                Err("fs.file.created:/home/me/University//a"),
            ),
            (uni, "fs.file.created", vec!["University/a"], Err("fs.file.created:University/a")),
            (uni, "fs.file.created", vec!["~/University/a"], Err("fs.file.created:~/University/a")),
            (
                uni,
                "fs.file.created",
                vec!["/home/me/University/a\0"],
                Err("fs.file.created:/home/me/University/a\0"),
            ),
            (
                uni,
                "fs.file.created",
                vec!["/home/me/university/a"],
                Err("fs.file.created:/home/me/university/a"),
            ),
            // Path-qualified grants cannot vouch for an event without paths.
            (uni, "fs.burst", vec![], Err("fs.burst")),
            (uni, "calendar.event.starting", vec![], Ok(())),
            (
                uni,
                "git.commit.created",
                vec!["/home/me/University"],
                Err("git.commit.created:/home/me/University"),
            ),
            // An unqualified grant covers every path.
            (&["fs.*"], "fs.file.created", vec!["/etc/passwd"], Ok(())),
            (&["fs.*:/a/**", "fs.file.*"], "fs.file.created", vec!["/etc/passwd"], Ok(())),
            // Different paths may be covered by different grants.
            (&["fs.*:/a/**", "fs.*:/b/**"], "fs.file.moved", vec!["/a/x", "/b/y"], Ok(())),
            (
                &["fs.file.created:/a/**", "fs.file.moved:/b/**"],
                "fs.file.moved",
                vec!["/a/x", "/b/y"],
                Err("fs.file.moved:/a/x"),
            ),
            (&["*"], "ext.ci.build", vec![], Ok(())),
            (&["ext.ci.*"], "ext.cd.build", vec![], Err("ext.cd.build")),
        ];
        for (grants, ty, paths, want) in cases {
            let got = set(grants).check(ty, &paths).map_err(|d| d.needed);
            assert_eq!(got, want.map_err(str::to_owned), "{grants:?} {ty} {paths:?}");
        }
    }
}
