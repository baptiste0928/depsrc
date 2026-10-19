//! Dependency specs: `<kind>:<name>[@<version>|#<ref>]`.

use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spec {
    pub kind: Kind,
    pub name: String,
    pub at: Option<At>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum At {
    /// `@<version>`: registry version or npm dist-tag.
    Version(String),
    /// `#<ref>`: git tag, branch or commit.
    Ref(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Crate,
    Npm,
    Gh,
    Git,
}

impl Kind {
    pub const ALL: [Kind; 4] = [Kind::Crate, Kind::Npm, Kind::Gh, Kind::Git];

    /// Spec prefix, without the colon.
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Crate => "crate",
            Kind::Npm => "npm",
            Kind::Gh => "gh",
            Kind::Git => "git",
        }
    }
}

impl Spec {
    /// Same spec at `at`.
    pub fn with(&self, at: At) -> Spec {
        Spec { at: Some(at), ..self.clone() }
    }
}

impl FromStr for Spec {
    type Err = String;

    fn from_str(s: &str) -> Result<Spec, String> {
        let invalid = || {
            let kinds: Vec<&str> = Kind::ALL.iter().map(|k| k.as_str()).collect();
            format!("invalid spec `{s}`, expected <{}>:<name>[@<version>|#<ref>]", kinds.join("|"))
        };

        let (kind, rest) = s.split_once(':').ok_or_else(invalid)?;
        let kind = Kind::ALL.into_iter().find(|k| k.as_str() == kind).ok_or_else(invalid)?;
        let (rest, git_ref) = rest.split_once('#').map_or((rest, None), |(rest, r)| (rest, Some(r)));

        let (name, version) = match kind {
            // A leading `@` is an npm scope.
            Kind::Crate | Kind::Npm => match rest.rfind('@').filter(|&i| i > 0) {
                Some(i) => (&rest[..i], Some(&rest[i + 1..])),
                None => (rest, None),
            },
            Kind::Gh | Kind::Git => (rest, None),
        };

        let part = |v: &str| if v.is_empty() || v.starts_with('-') { Err(invalid()) } else { Ok(v.to_string()) };

        let at = match (version, git_ref) {
            (Some(_), Some(_)) => return Err(invalid()),
            (Some(v), None) => Some(At::Version(part(v)?)),
            (None, Some(r)) => Some(At::Ref(part(r)?)),
            (None, None) => None,
        };

        if !valid_name(kind, name) {
            // A git ref after `@` is an easy mistake: name it.
            if let Some((fixed, _)) = name.rsplit_once('@')
                && git_ref.is_none()
                && valid_name(kind, fixed)
            {
                return Err(format!("invalid spec `{s}`, git refs follow `#`: {}:{fixed}#<ref>", kind.as_str()));
            }

            return Err(invalid());
        }

        Ok(Spec { kind, name: name.to_string(), at })
    }
}

impl fmt::Display for Spec {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}:{}", self.kind.as_str(), self.name)?;
        match &self.at {
            Some(At::Version(v)) => write!(f, "@{v}"),
            Some(At::Ref(r)) => write!(f, "#{r}"),
            None => Ok(()),
        }
    }
}

/// `owner/repo` for gh, a url without `@` in its path for git, as `@` starts versions elsewhere.
fn valid_name(kind: Kind, name: &str) -> bool {
    let valid = match kind {
        Kind::Gh => name.split('/').count() == 2 && !name.split('/').any(str::is_empty) && !name.contains('@'),
        Kind::Git => name
            .split_once("://")
            .is_some_and(|(_, rest)| !rest.split_once('/').is_some_and(|(_, path)| path.contains('@'))),
        Kind::Crate | Kind::Npm => !name.is_empty(),
    };
    valid && !name.starts_with('-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse() {
        let version = |v: &str| Some(At::Version(v.to_string()));
        let git_ref = |r: &str| Some(At::Ref(r.to_string()));

        for (input, name, at) in [
            ("crate:serde", "serde", None),
            ("crate:serde@1.0.210", "serde", version("1.0.210")),
            ("crate:serde#v1.0.210", "serde", git_ref("v1.0.210")),
            ("npm:@types/node", "@types/node", None),
            ("npm:@types/node@20.0.0", "@types/node", version("20.0.0")),
            ("npm:@types/node#main", "@types/node", git_ref("main")),
            ("gh:jj-vcs/jj#feat/x@y", "jj-vcs/jj", git_ref("feat/x@y")),
            ("git:ssh://git@host.com/a/b#main", "ssh://git@host.com/a/b", git_ref("main")),
            ("git:https://host.com/a/b", "https://host.com/a/b", None),
            ("git:ssh://git@host.com", "ssh://git@host.com", None),
        ] {
            let spec: Spec = input.parse().unwrap();
            assert_eq!((spec.name.as_str(), &spec.at), (name, &at), "{input}");
            assert_eq!(spec.to_string(), input);
        }

        for input in [
            "serde",
            "pip:x",
            "crate:",
            "crate:x@",
            "crate:x#",
            "crate:x@1#main",
            "gh:jj",
            "gh:jj-vcs/jj@main",
            "git:https://host.com/a/b@main",
            "git:host/a",
            "git:--upload-pack=x://a",
            "npm:x@-v",
        ] {
            assert!(input.parse::<Spec>().is_err(), "{input}");
        }

        let hint = |input: &str| input.parse::<Spec>().unwrap_err().contains("git refs follow `#`");
        assert!(hint("gh:jj-vcs/jj@main") && hint("git:ssh://git@host.com/a/b@main"));
        assert!(!hint("gh:jj@main"));
    }
}
