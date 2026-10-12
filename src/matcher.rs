//! How a configured `app` string is compared to what the OS reports.

/// Case-insensitive name comparison. A trailing `.exe` is ignored on both sides so
/// `WindowsTerminal` and `WindowsTerminal.exe` are interchangeable.
pub fn matches(pattern: &str, candidate: &str) -> bool {
    let norm = |s: &str| {
        let s = s.trim().to_lowercase();
        match s.strip_suffix(".exe") {
            Some(stem) => stem.to_owned(),
            None => s,
        }
    };
    let pattern = norm(pattern);
    !pattern.is_empty() && pattern == norm(candidate)
}

#[cfg(test)]
mod tests {
    use super::matches;

    #[test]
    fn case_and_exe_insensitive() {
        assert!(matches("Terminal", "terminal"));
        assert!(matches("WindowsTerminal", "WindowsTerminal.exe"));
        assert!(matches("windowsterminal.EXE", "WindowsTerminal"));
        assert!(matches(" gnome-terminal ", "Gnome-terminal"));
    }

    #[test]
    fn no_partial_or_empty_matches() {
        assert!(!matches("term", "Terminal"));
        assert!(!matches("", ""));
        assert!(!matches(".exe", ".exe"));
    }
}
