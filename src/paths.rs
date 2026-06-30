//! Small filesystem-path helpers shared across widgets.

/// Expand a leading `~/` to `$HOME`. Returns the input unchanged otherwise.
pub fn expand_tilde(path: &str) -> String {
    match (path.strip_prefix("~/"), std::env::var("HOME")) {
        (Some(rest), Ok(home)) => format!("{}/{rest}", home.trim_end_matches('/')),
        _ => path.to_owned(),
    }
}
