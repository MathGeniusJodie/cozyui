//! Small filesystem-path helpers shared across widgets.

/// Path of a cozyui config/state file. Prefers `$XDG_CONFIG_HOME/cozyui/<name>`
/// (default `~/.config/cozyui/<name>`); when that file doesn't exist yet it
/// falls back to a copy next to the source checkout so an unconfigured dev
/// build keeps working. When neither exists, the XDG directory is created and
/// its path returned, so first writes land somewhere durable instead of a
/// baked-in build-machine path.
pub fn config_file(name: &str) -> String {
    let dir = config_dir();
    let path = format!("{dir}/{name}");
    if std::path::Path::new(&path).exists() {
        return path;
    }
    let dev_path = format!("{}/{name}", env!("CARGO_MANIFEST_DIR"));
    if std::path::Path::new(&dev_path).exists() {
        return dev_path;
    }
    if let Err(err) = std::fs::create_dir_all(&dir) {
        eprintln!("paths: failed to create config dir {dir}: {err}");
    }
    path
}

/// `$XDG_CONFIG_HOME/cozyui` (default `~/.config/cozyui`).
fn config_dir() -> String {
    let config_home = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|dir| !dir.is_empty())
        .unwrap_or_else(|| expand_tilde("~/.config"));
    format!("{}/cozyui", config_home.trim_end_matches('/'))
}

/// Expand a leading `~/` to `$HOME`. Returns the input unchanged otherwise.
pub fn expand_tilde(path: &str) -> String {
    match (path.strip_prefix("~/"), std::env::var("HOME")) {
        (Some(rest), Ok(home)) => format!("{}/{rest}", home.trim_end_matches('/')),
        _ => path.to_owned(),
    }
}

/// Read `conf_name` (via [`config_file`]) and return its first non-blank,
/// non-comment line trimmed of whitespace, tilde-expanded; falls back to
/// `default` (also tilde-expanded) when the file is missing or has no such
/// line. Callers typically cache the result themselves in a `OnceLock` since
/// this hits the filesystem on every call.
pub fn config_first_line(conf_name: &str, default: &str) -> String {
    let configured = std::fs::read_to_string(config_file(conf_name))
        .ok()
        .and_then(|text| {
            text.lines()
                .map(str::trim)
                .find(|line| !line.is_empty() && !line.starts_with('#'))
                .map(str::to_owned)
        })
        .unwrap_or_else(|| default.to_owned());
    expand_tilde(&configured)
}
