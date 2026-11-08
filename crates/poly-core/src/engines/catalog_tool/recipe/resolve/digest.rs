//! Reading what upstream publishes: digests, Maven metadata and version lists.

/// A lower-case hex SHA-256 from `text`, if it is one.
pub(super) fn sha256_hex(text: &str) -> Option<String> {
    let text = text.trim();
    (text.len() == 64 && text.chars().all(|c| c.is_ascii_hexdigit())).then(|| text.to_ascii_lowercase())
}

/// The digest of a GitHub asset's `digest` field (`sha256:<hex>`).
pub(super) fn from_api_digest(field: &str) -> Option<String> {
    sha256_hex(field.strip_prefix("sha256:")?)
}

/// The digest in a sidecar file (`<hex>  <name>` or just `<hex>`). A file name
/// in the sidecar must be `name`: another asset's digest is not this one's.
pub(super) fn from_sidecar(body: &str, name: &str) -> Option<String> {
    let mut words = body.split_whitespace();
    let digest = sha256_hex(words.next()?)?;
    if let Some(file) = words.next() {
        let file = file.trim_start_matches('*');
        if file.rsplit('/').next().unwrap_or(file) != name {
            return None;
        }
    }
    Some(digest)
}

/// Whether `version` is safe to use as a path segment and in a URL:
/// `[A-Za-z0-9._+-]+` with no `..`.
pub(in crate::engines::catalog_tool) fn valid_version(version: &str) -> bool {
    !version.is_empty()
        && !version.contains("..")
        && version
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-'))
}

/// The digest of `name` in a `<hex>  <name>` list (`sha256sum` output, where a
/// binary-mode name is `*<name>`).
pub(super) fn from_list(body: &str, name: &str) -> Option<String> {
    body.lines().find_map(|line| {
        let (digest, file) = line.trim().split_once(char::is_whitespace)?;
        let file = file.trim().trim_start_matches('*');
        let file = file.rsplit('/').next().unwrap_or(file);
        (file == name).then(|| sha256_hex(digest)).flatten()
    })
}

/// The text between `<tag>` and `</tag>`.
fn element<'a>(xml: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&format!("</{tag}>"))? + start;
    Some(xml[start..end].trim())
}

/// The newest release in a Maven `maven-metadata.xml`.
pub(super) fn maven_release(xml: &str) -> Option<String> {
    element(xml, "release")
        .or_else(|| element(xml, "latest"))
        .filter(|version| valid_version(version))
        .map(str::to_owned)
}

/// Every version a Maven `maven-metadata.xml` lists.
pub(super) fn maven_versions(xml: &str) -> Vec<String> {
    let Some(block) = element(xml, "versions") else {
        return Vec::new();
    };
    block
        .split("<version>")
        .skip(1)
        .filter_map(|part| part.split("</version>").next())
        .map(|version| version.trim().to_owned())
        .filter(|version| valid_version(version))
        .collect()
}

/// A version as numbers, so `2.10.0` sorts after `2.9.0`.
pub(in crate::engines::catalog_tool) fn version_key(version: &str) -> Vec<u64> {
    version
        .trim_start_matches('v')
        .split(|c: char| !c.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .map(|part| part.parse().unwrap_or(0))
        .collect()
}

fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut previous = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let held = row[j + 1];
            row[j + 1] = (previous + usize::from(ca != *cb)).min(row[j] + 1).min(row[j + 1] + 1);
            previous = held;
        }
    }
    row[b.len()]
}

/// Up to `count` of `candidates` nearest to `wanted`, nearest first.
pub(super) fn closest(wanted: &str, candidates: &[String], count: usize) -> Vec<String> {
    let mut scored: Vec<(usize, Vec<u64>, &String)> = candidates
        .iter()
        .map(|candidate| (distance(wanted, candidate), version_key(candidate), candidate))
        .collect();
    scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| b.1.cmp(&a.1)));
    scored.into_iter().take(count).map(|(_, _, c)| c.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEX: &str = "24a984105fe95594bbb360b7799ada7db13dddeb9fce69640df471061040b5a4";

    #[test]
    fn digests_are_read_from_every_published_shape() {
        assert_eq!(from_api_digest(&format!("sha256:{HEX}")).as_deref(), Some(HEX));
        assert_eq!(from_api_digest(&format!("sha512:{HEX}")), None);
        assert_eq!(
            from_sidecar(&format!("{HEX}  helm.tar.gz\n"), "helm.tar.gz").as_deref(),
            Some(HEX)
        );
        assert_eq!(from_sidecar(&format!("{HEX}\n"), "helm.tar.gz").as_deref(), Some(HEX));
        assert_eq!(from_sidecar("<html>not found</html>", "helm.tar.gz"), None);
        let list = format!("{}  other\n{HEX} *tool_1.0_linux_amd64.tar.gz\n", "0".repeat(64));
        assert_eq!(from_list(&list, "tool_1.0_linux_amd64.tar.gz").as_deref(), Some(HEX));
        assert_eq!(from_list(&list, "absent"), None);
    }

    #[test]
    fn a_sidecar_naming_another_file_is_not_this_files_digest() {
        let body = format!("{HEX}  helm-v1.0.0-linux-arm64.tar.gz\n");
        assert_eq!(from_sidecar(&body, "helm-v1.0.0-linux-amd64.tar.gz"), None);
        let starred = format!("{HEX} *dist/helm.tar.gz\n");
        assert_eq!(from_sidecar(&starred, "helm.tar.gz").as_deref(), Some(HEX));
    }

    #[test]
    fn only_path_and_url_safe_versions_are_valid() {
        for good in ["1.2.3", "v2", "14.4.0", "1.0.0-rc.1+build_5"] {
            assert!(valid_version(good), "{good}");
        }
        for bad in ["", "..", "1/../2", "1..2", "a b", "1.0/x", "%2e%2e", "1\\2", "x?y"] {
            assert!(!valid_version(bad), "{bad}");
        }
    }

    #[test]
    fn maven_metadata_yields_the_release_and_the_versions() {
        let xml = "<metadata><versioning><latest>2.104.0</latest><release>2.103.0</release>\
                   <versions><version>2.102.0</version><version>2.103.0</version></versions></versioning></metadata>";
        assert_eq!(maven_release(xml).as_deref(), Some("2.103.0"));
        assert_eq!(maven_versions(xml), ["2.102.0", "2.103.0"]);
        assert_eq!(maven_release("<metadata/>"), None);
    }

    #[test]
    fn versions_order_numerically_and_the_closest_come_first() {
        assert!(version_key("2.10.0") > version_key("2.9.0"));
        assert_eq!(version_key("v1.2.3"), [1, 2, 3]);
        let published: Vec<String> = ["2.15.0", "2.14.1", "2.14.0", "1.0.0"].map(String::from).to_vec();
        assert_eq!(closest("2.14.2", &published, 2), ["2.14.1", "2.14.0"]);
    }
}
