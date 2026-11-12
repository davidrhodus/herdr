//! Preserve only the caller's SSH lookup and agent context in the login job.
//! Passwords, keys, askpass helpers, and unrelated environment never enter it.
pub(super) fn capture() -> Vec<(String, String)> {
    selected(
        std::env::vars_os()
            .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?))),
    )
}
fn selected(values: impl IntoIterator<Item = (String, String)>) -> Vec<(String, String)> {
    let mut values: Vec<_> = values
        .into_iter()
        .filter(|(key, value)| {
            matches!(key.as_str(), "PATH" | "SSH_AUTH_SOCK")
                && !value.is_empty()
                && !value.contains(['\0', '\r', '\n'])
        })
        .collect();
    if !values.iter().any(|(key, _)| key == "PATH") {
        values.push((
            "PATH".into(),
            "/usr/bin:/bin:/usr/sbin:/sbin:/opt/homebrew/bin:/usr/local/bin".into(),
        ));
    }
    values.sort();
    values
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_agent_and_path_without_credentials_or_askpass() {
        let env = selected([
            (
                "SSH_AUTH_SOCK".into(),
                "/tmp/agent with spaces/$agent".into(),
            ),
            ("PATH".into(), "/custom/bin:/usr/bin".into()),
            ("SSH_ASKPASS".into(), "/unrelated/helper".into()),
            ("TOKEN".into(), "private".into()),
            ("DYLD_INSERT_LIBRARIES".into(), "/unrelated/library".into()),
        ]);
        assert_eq!(
            env,
            vec![
                ("PATH".into(), "/custom/bin:/usr/bin".into()),
                (
                    "SSH_AUTH_SOCK".into(),
                    "/tmp/agent with spaces/$agent".into()
                )
            ]
        );
    }
    #[test]
    fn rejects_empty_and_line_break_values_and_keeps_a_default_path() {
        let env = selected([
            ("PATH".into(), "".into()),
            ("SSH_AUTH_SOCK".into(), "/tmp/socket\nInvalid=1".into()),
        ]);
        assert_eq!(env.len(), 1);
        assert_eq!(env[0].0, "PATH");
    }
}
