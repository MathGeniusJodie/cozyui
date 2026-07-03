//! Small filesystem-path helpers shared across widgets.

/// Path of a cozyui config file: `$XDG_CONFIG_HOME/cozyui/<name>` (default
/// `~/.config/cozyui/<name>`) when that file exists, otherwise the copy next
/// to the source checkout so an unconfigured dev build keeps working.
pub fn config_file(name: &str) -> String {
    let config_home = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|dir| !dir.is_empty())
        .unwrap_or_else(|| expand_tilde("~/.config"));
    let path = format!("{}/cozyui/{name}", config_home.trim_end_matches('/'));
    if std::path::Path::new(&path).exists() {
        return path;
    }
    format!("{}/{name}", env!("CARGO_MANIFEST_DIR"))
}

/// Expand a leading `~/` to `$HOME`. Returns the input unchanged otherwise.
pub fn expand_tilde(path: &str) -> String {
    match (path.strip_prefix("~/"), std::env::var("HOME")) {
        (Some(rest), Ok(home)) => format!("{}/{rest}", home.trim_end_matches('/')),
        _ => path.to_owned(),
    }
}
