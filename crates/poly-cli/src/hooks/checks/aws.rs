//! The `aws_credentials` check: reject files that contain an AWS secret the
//! developer's own environment or credentials files hold.
//!
//! This is `detect-aws-credentials` from pre-commit-hooks with
//! `--allow-missing-credentials` as the only mode: when no credentials are
//! configured there is nothing to leak, so the check passes instead of failing
//! every commit on a machine without AWS access.
//!
//! The secret values are never printed; a finding names the file and line.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::{Violation, read_for_scan};

/// Environment variables whose value is an AWS secret.
const SECRET_ENV_VARS: &[&str] = &["AWS_SECRET_ACCESS_KEY", "AWS_SECURITY_TOKEN", "AWS_SESSION_TOKEN"];

/// Environment variables that name an extra credentials file.
const FILE_ENV_VARS: &[&str] = &[
    "AWS_CONFIG_FILE",
    "AWS_CREDENTIAL_FILE",
    "AWS_SHARED_CREDENTIALS_FILE",
    "BOTO_CONFIG",
];

/// Credentials files under the home directory.
const HOME_FILES: &[&str] = &[".aws/config", ".aws/credentials", ".boto", ".s3cfg", ".passwd-s3fs"];

/// Keys inside a credentials file whose value is a secret.
const SECRET_KEYS: &[&str] = &[
    "aws_secret_access_key",
    "aws_security_token",
    "aws_session_token",
    "secret_key",
    "security_token",
];

/// Secrets shorter than this are ignored: they would match unrelated text.
const MIN_SECRET_LEN: usize = 4;

/// Where credentials are read from. Injectable so tests never touch the real
/// environment or home directory.
pub(super) struct Sources<'a> {
    pub env: &'a dyn Fn(&str) -> Option<String>,
    pub home: Option<PathBuf>,
}

fn ambient_env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

impl Sources<'static> {
    /// The process environment and the user's home directory.
    pub(super) fn ambient() -> Self {
        Sources {
            env: &ambient_env,
            home: std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(PathBuf::from),
        }
    }
}

/// Every AWS secret reachable from `sources`.
pub(super) fn collect_secrets(sources: &Sources<'_>) -> BTreeSet<String> {
    let mut secrets: BTreeSet<String> = SECRET_ENV_VARS.iter().filter_map(|name| (sources.env)(name)).collect();
    let mut files: Vec<PathBuf> = FILE_ENV_VARS
        .iter()
        .filter_map(|name| (sources.env)(name))
        .map(PathBuf::from)
        .collect();
    if let Some(home) = &sources.home {
        files.extend(HOME_FILES.iter().map(|file| home.join(file)));
    }
    for file in files {
        if let Ok(text) = std::fs::read_to_string(&file) {
            secrets.extend(secrets_in(&text));
        }
    }
    secrets.retain(|secret| secret.len() >= MIN_SECRET_LEN);
    secrets
}

fn secrets_in(text: &str) -> impl Iterator<Item = String> + '_ {
    text.lines().filter_map(|line| {
        let (key, value) = line.split_once(['=', ':'])?;
        let key = key.trim().to_ascii_lowercase();
        let value = value.trim().trim_matches(['"', '\'']);
        (SECRET_KEYS.contains(&key.as_str()) && !value.is_empty()).then(|| value.to_string())
    })
}

/// Report every line of `files` that contains one of `secrets`.
pub(super) fn check_aws_credentials(root: &Path, files: &[PathBuf], secrets: &BTreeSet<String>) -> Vec<Violation> {
    if secrets.is_empty() {
        return Vec::new();
    }
    let mut violations = Vec::new();
    for path in files {
        let Some(bytes) = read_for_scan(&root.join(path)) else {
            continue;
        };
        let text = String::from_utf8_lossy(&bytes);
        for (index, line) in text.lines().enumerate() {
            if secrets.iter().any(|secret| line.contains(secret.as_str())) {
                violations.push(Violation::new(
                    "detect-aws-credentials",
                    path.clone(),
                    format!("line {} contains an AWS secret from your credentials", index + 1),
                ));
            }
        }
    }
    violations
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::fs;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let map: HashMap<String, String> = pairs.iter().map(|(k, v)| ((*k).into(), (*v).into())).collect();
        move |name| map.get(name).cloned()
    }

    #[test]
    fn secrets_come_from_the_environment() {
        let env = env_of(&[("AWS_SECRET_ACCESS_KEY", "s3cr3t-value"), ("PATH", "/bin")]);
        let secrets = collect_secrets(&Sources { env: &env, home: None });
        assert_eq!(secrets.into_iter().collect::<Vec<_>>(), ["s3cr3t-value"]);
    }

    #[test]
    fn secrets_come_from_home_credentials_files() {
        let home = tempfile::tempdir().unwrap();
        fs::create_dir(home.path().join(".aws")).unwrap();
        fs::write(
            home.path().join(".aws/credentials"),
            "[default]\naws_access_key_id = AKIAPUBLIC\naws_secret_access_key = hunter2hunter2\n",
        )
        .unwrap();
        let env = env_of(&[]);
        let secrets = collect_secrets(&Sources {
            env: &env,
            home: Some(home.path().to_path_buf()),
        });
        assert!(secrets.contains("hunter2hunter2"));
        assert!(!secrets.contains("AKIAPUBLIC"), "the access key id is not a secret");
    }

    #[test]
    fn secrets_come_from_a_file_named_by_the_environment() {
        let dir = tempfile::tempdir().unwrap();
        let creds = dir.path().join("creds.ini");
        fs::write(&creds, "aws_session_token=\"tok-123456\"\n").unwrap();
        let env = env_of(&[("AWS_SHARED_CREDENTIALS_FILE", creds.to_str().unwrap())]);
        assert!(collect_secrets(&Sources { env: &env, home: None }).contains("tok-123456"));
    }

    #[test]
    fn short_secrets_are_ignored() {
        let env = env_of(&[("AWS_SECRET_ACCESS_KEY", "abc")]);
        assert!(collect_secrets(&Sources { env: &env, home: None }).is_empty());
    }

    #[test]
    fn no_credentials_means_nothing_to_report() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "anything at all\n").unwrap();
        assert!(check_aws_credentials(dir.path(), &["a.txt".into()], &BTreeSet::new()).is_empty());
    }

    #[test]
    fn a_file_holding_a_secret_is_reported_without_printing_it() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("leak.py"), "ok = 1\nKEY = 'hunter2hunter2'\n").unwrap();
        fs::write(dir.path().join("clean.py"), "ok = 1\n").unwrap();
        let secrets = BTreeSet::from(["hunter2hunter2".to_string()]);
        let violations = check_aws_credentials(dir.path(), &["leak.py".into(), "clean.py".into()], &secrets);
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].path, PathBuf::from("leak.py"));
        assert!(violations[0].message.contains("line 2"));
        assert!(!violations[0].message.contains("hunter2hunter2"));
    }
}
