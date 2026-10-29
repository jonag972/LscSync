//! Version numbers of releases ("0.2.0", or tags such as "v0.2.0").

/// Major, minor and patch numbers; anything after them ("-beta") is ignored.
pub fn parse(version: &str) -> Option<[u32; 3]> {
    let core = version.trim().trim_start_matches('v');
    let core = core.split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|part| part.parse::<u32>().ok());
    let version = [
        parts.next()??,
        parts.next().unwrap_or(Some(0))?,
        parts.next().unwrap_or(Some(0))?,
    ];
    parts.next().is_none().then_some(version)
}

/// Whether `candidate` is a later release than `current`. False when either
/// one is unreadable.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse(candidate), parse(current)) {
        (Some(candidate), Some(current)) => candidate > current,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tags_and_versions() {
        assert_eq!(parse("v0.2.0"), Some([0, 2, 0]));
        assert_eq!(parse("1.10.3"), Some([1, 10, 3]));
        assert_eq!(parse("v2"), Some([2, 0, 0]));
        assert_eq!(parse("0.3.0-beta.1"), Some([0, 3, 0]));
        assert_eq!(parse("latest"), None);
        assert_eq!(parse("1.2.3.4"), None);
    }

    #[test]
    fn compares_numerically() {
        assert!(is_newer("v0.10.0", "0.9.0"));
        assert!(is_newer("v1.0.0", "0.2.0"));
        assert!(!is_newer("v0.2.0", "0.2.0"));
        assert!(!is_newer("v0.1.1", "0.2.0"));
        assert!(!is_newer("nightly", "0.2.0"));
    }
}
