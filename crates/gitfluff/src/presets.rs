use crate::lint::BodyPolicy;

#[derive(Debug, Clone)]
pub struct Preset {
    pub message_pattern: &'static str,
    pub description: &'static str,
    pub body_policy: BodyPolicy,
    pub enforce_spec: bool,
}

// Align with commitlint's default `headerPattern` (via `conventional-changelog-conventionalcommits`):
//   /^(\w*)(?:\((.*)\))?!?: (.*)$/
// We require a non-empty type (`\\w+`) because gitfluff treats pattern mismatches as violations.
const CONVENTIONAL_PATTERN: &str =
    "^(?P<type>\\w+)(\\((?P<scope>.*)\\))?(?P<breaking>!)?: (?P<description>.+)$";

pub fn resolve_preset(name: &str) -> Option<Preset> {
    match name.to_lowercase().as_str() {
        "conventional" | "default" => Some(conventional()),
        "conventional-body" | "conventional_detailed" | "conventional-with-body" => {
            Some(conventional_with_body())
        }
        "simple" | "simple-single-line" => Some(simple_single_line()),
        _ => None,
    }
}

fn conventional() -> Preset {
    Preset {
        message_pattern: CONVENTIONAL_PATTERN,
        description: "Conventional Commits title line (AI signatures are cleaned automatically)",
        body_policy: BodyPolicy::Any,
        enforce_spec: true,
    }
}

fn conventional_with_body() -> Preset {
    Preset {
        message_pattern: CONVENTIONAL_PATTERN,
        description: "Conventional Commits title line with a required body section",
        body_policy: BodyPolicy::RequireBody,
        enforce_spec: true,
    }
}

fn simple_single_line() -> Preset {
    const SIMPLE_PATTERN: &str = "^[A-Za-z][^\\n]+$";
    Preset {
        message_pattern: SIMPLE_PATTERN,
        description: "Single-line summary starting with a letter",
        body_policy: BodyPolicy::SingleLine,
        enforce_spec: false,
    }
}
