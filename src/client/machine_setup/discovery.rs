use std::{
    collections::BTreeSet,
    io::Read,
    path::{Path, PathBuf},
};

fn aliases(text: &str) -> Vec<String> {
    text.lines()
        .flat_map(|line| {
            let mut fields = line.split_whitespace();
            if fields
                .next()
                .is_some_and(|v| v.eq_ignore_ascii_case("host"))
            {
                fields
                    .take_while(|s| !s.starts_with('#'))
                    .filter(|s| !s.contains(['*', '?', '!']) && s.len() <= 256)
                    .map(str::to_owned)
                    .collect()
            } else {
                Vec::new()
            }
        })
        .collect()
}
fn includes(path: &Path, base: &Path, home: Option<&Path>) -> Vec<PathBuf> {
    let path = if let Some(suffix) = path.to_str().and_then(|p| p.strip_prefix("~/")) {
        home.map(|home| home.join(suffix)).unwrap_or_default()
    } else if path.is_absolute() {
        path.to_owned()
    } else {
        base.join(path)
    };
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return Vec::new();
    };
    if !name.contains(['*', '?']) {
        return vec![path];
    }
    let Some(parent) = path.parent() else {
        return Vec::new();
    };
    let mut files = std::fs::read_dir(parent)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|entry| wildcard(name, &entry.file_name().to_string_lossy()))
        .map(|entry| entry.path())
        .take(32)
        .collect::<Vec<_>>();
    files.sort();
    files
}
fn wildcard(pattern: &str, value: &str) -> bool {
    let (p, v) = (pattern.as_bytes(), value.as_bytes());
    let (mut i, mut j, mut star, mut retry) = (0, 0, None, 0);
    while j < v.len() {
        if i < p.len() && (p[i] == b'?' || p[i] == v[j]) {
            i += 1;
            j += 1;
        } else if i < p.len() && p[i] == b'*' {
            star = Some(i);
            i += 1;
            retry = j;
        } else if let Some(at) = star {
            retry += 1;
            j = retry;
            i = at + 1;
        } else {
            return false;
        }
    }
    while i < p.len() && p[i] == b'*' {
        i += 1;
    }
    i == p.len()
}
fn ssh_targets() -> BTreeSet<String> {
    let config = crate::platform::remote_ssh_config_paths();
    let home = config
        .user_config
        .as_ref()
        .and_then(|p| p.parent()?.parent())
        .map(Path::to_owned);
    let mut pending = Vec::new();
    for path in [config.user_config, config.system_config]
        .into_iter()
        .flatten()
    {
        let base = path.parent().unwrap_or(Path::new(".")).to_owned();
        pending.push((path, base, 0));
    }
    let mut visited = BTreeSet::new();
    let mut result = BTreeSet::new();
    let mut remaining = 1024 * 1024;
    while let Some((path, base, depth)) = pending.pop() {
        if depth > 8 || visited.len() >= 32 || remaining == 0 {
            break;
        }
        let Ok(path) = std::fs::canonicalize(path) else {
            continue;
        };
        if !visited.insert(path.clone()) {
            continue;
        }
        let Ok(file) = std::fs::File::open(path) else {
            continue;
        };
        let mut text = String::new();
        if file
            .take(remaining.min(256 * 1024))
            .read_to_string(&mut text)
            .is_err()
        {
            continue;
        }
        remaining = remaining.saturating_sub(text.len() as u64);
        result.extend(aliases(&text));
        for line in text.lines() {
            let mut words = line.split_whitespace();
            if words
                .next()
                .is_some_and(|w| w.eq_ignore_ascii_case("include"))
            {
                for name in words.take_while(|w| !w.starts_with('#')) {
                    for path in includes(
                        Path::new(name.trim_matches(['\'', '"'])),
                        &base,
                        home.as_deref(),
                    ) {
                        pending.push((path, base.clone(), depth + 1));
                    }
                }
            }
        }
    }
    result
}
pub(super) fn targets() -> Vec<String> {
    let mut result = ssh_targets();
    if let Ok(devices) = super::tailscale::devices() {
        result.extend(
            devices
                .iter()
                .filter(|device| device.online == Some(true))
                .filter_map(|device| device.target()),
        );
    }
    result.into_iter().take(128).collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn suggestions_include_literal_ssh_aliases_without_patterns() {
        assert_eq!(
            aliases("Host build laptop # note\nHost *.example !skip\n Host tailnet\n"),
            vec!["build", "laptop", "tailnet"]
        );
    }
    #[test]
    fn include_patterns_are_bounded_to_the_selected_directory() {
        assert!(wildcard("*.conf", "home.conf"));
        assert!(!wildcard("*.conf", "home.txt"));
        assert!(wildcard("host-?", "host-a"));
        assert!(!wildcard("host-?", "host-aa"));
    }
}
