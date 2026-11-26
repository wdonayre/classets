//! Skip patterns: files a device never syncs (temp files, editor lock files, OS clutter).
//!
//! Patterns use a small glob subset: `*` matches any run of characters within one path
//! segment, and a trailing `/` means "this directory and everything in it".

/// Patterns every device skips, before any per-device patterns from the web app.
pub const DEFAULT_SKIP: &[&str] = &[
    "*.tmp",
    "~$*",
    ".DS_Store",
    "Thumbs.db",
    "desktop.ini",
    "node_modules/",
    ".git/",
];

/// True when `path` (relative, `/`-separated) matches any of `patterns`.
pub fn is_skipped(path: &str, patterns: &[&str]) -> bool {
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    patterns.iter().any(|p| matches_pattern(&segments, p))
}

fn matches_pattern(segments: &[&str], pattern: &str) -> bool {
    if let Some(dir) = pattern.strip_suffix('/') {
        // Directory pattern: any directory segment (not the file name itself) matches.
        let dirs = &segments[..segments.len().saturating_sub(1)];
        dirs.iter().any(|s| glob(dir, s))
    } else {
        segments.last().is_some_and(|name| glob(pattern, name))
    }
}

/// `*`-only glob match within one segment.
fn glob(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == text;
    }
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if !text.starts_with(first) || !text.ends_with(last) || text.len() < first.len() + last.len() {
        return false;
    }
    let mut rest = &text[first.len()..text.len() - last.len()];
    for mid in &parts[1..parts.len() - 1] {
        match rest.find(mid) {
            Some(i) => rest = &rest[i + mid.len()..],
            None => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_defaults() {
        assert!(is_skipped("Clients/draft.tmp", DEFAULT_SKIP));
        assert!(is_skipped("Clients/~$proposal.docx", DEFAULT_SKIP));
        assert!(is_skipped("Photos/.DS_Store", DEFAULT_SKIP));
        assert!(is_skipped("code/site/node_modules/react/index.js", DEFAULT_SKIP));
    }

    #[test]
    fn keeps_normal_files() {
        assert!(!is_skipped("Clients/Alab Apparel/lookbook-cover.jpg", DEFAULT_SKIP));
        assert!(!is_skipped("notes/tmp-plan.md", DEFAULT_SKIP));
        // A file merely named like a skipped directory is kept.
        assert!(!is_skipped("docs/node_modules", DEFAULT_SKIP));
    }

    #[test]
    fn glob_middle_star() {
        assert!(glob("a*b*c", "aXXbYYc"));
        assert!(!glob("a*b*c", "aXXcYYb"));
        assert!(glob("*.psd", "team-kit-render.psd"));
    }
}
