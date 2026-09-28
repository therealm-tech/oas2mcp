//! Operation filtering: decide which OpenAPI operations are advertised as MCP
//! tools. Large APIs (e.g. GitLab, ~1700 operations) produce a `tools/list`
//! payload far too big to fit a model's context, so callers restrict the set by
//! operation name (regex) and/or tag.

use regex::Regex;

/// Filtering rules as collected from the CLI / environment. Regexes are already
/// compiled — clap rejects invalid patterns while parsing the arguments.
#[derive(Debug, Default, Clone)]
pub struct FilterConfig {
    /// Name regexes to allow.
    pub include_regexes: Vec<Regex>,
    /// Name regexes to deny.
    pub exclude_regexes: Vec<Regex>,
    /// Tags to allow (matched case-insensitively).
    pub include_tags: Vec<String>,
    /// Tags to deny (matched case-insensitively).
    pub exclude_tags: Vec<String>,
}

/// Selects which operations become tools.
///
/// Two axes — operation name (regex) and tag — each with an allowlist
/// and a denylist. An operation is kept when it passes **both** the include and
/// the exclude test; a denylist match always wins.
#[derive(Debug, Default, Clone)]
pub struct OperationFilter {
    include_regexes: Vec<Regex>,
    exclude_regexes: Vec<Regex>,
    include_tags: Vec<String>,
    exclude_tags: Vec<String>,
}

impl OperationFilter {
    /// Build a filter from its (already validated) configuration.
    pub fn new(config: FilterConfig) -> Self {
        Self {
            include_regexes: config.include_regexes,
            exclude_regexes: config.exclude_regexes,
            include_tags: config.include_tags,
            exclude_tags: config.exclude_tags,
        }
    }

    /// Decide whether an operation with the given tool `name` and `tags` is kept.
    pub fn keeps(&self, name: &str, tags: &[String]) -> bool {
        // Denylist wins: a match on any axis drops the operation.
        if matches_name(&self.exclude_regexes, name) || has_tag(&self.exclude_tags, tags) {
            return false;
        }

        // Allowlist: when none is configured, everything not denied is kept.
        let has_allow = !self.include_regexes.is_empty() || !self.include_tags.is_empty();
        if !has_allow {
            return true;
        }

        matches_name(&self.include_regexes, name) || has_tag(&self.include_tags, tags)
    }
}

fn matches_name(regexes: &[Regex], name: &str) -> bool {
    regexes.iter().any(|re| re.is_match(name))
}

fn has_tag(wanted: &[String], tags: &[String]) -> bool {
    wanted
        .iter()
        .any(|w| tags.iter().any(|t| t.eq_ignore_ascii_case(w)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a filter from name regexes and tags, panicking on a bad regex.
    fn filter(
        include: Vec<&str>,
        exclude: Vec<&str>,
        include_tags: Vec<&str>,
        exclude_tags: Vec<&str>,
    ) -> OperationFilter {
        let compile = |patterns: Vec<&str>| {
            patterns
                .into_iter()
                .map(|p| Regex::new(p).expect("valid regex"))
                .collect()
        };
        OperationFilter::new(FilterConfig {
            include_regexes: compile(include),
            exclude_regexes: compile(exclude),
            include_tags: include_tags.into_iter().map(Into::into).collect(),
            exclude_tags: exclude_tags.into_iter().map(Into::into).collect(),
        })
    }

    #[test]
    fn empty_filter_keeps_everything() {
        let f = OperationFilter::default();
        assert!(f.keeps("anything", &[]));
    }

    #[test]
    fn tags_allow_and_deny_case_insensitively() {
        let f = filter(vec![], vec![], vec!["projects"], vec!["admin"]);
        assert!(f.keeps("anyName", &["Projects".into()]));
        assert!(!f.keeps("anyName", &["Other".into()]));
        // Deny wins even when an include tag also matches.
        assert!(!f.keeps("anyName", &["Projects".into(), "Admin".into()]));
    }

    #[test]
    fn name_or_tag_satisfies_the_allowlist() {
        let f = filter(vec!["^keepMe$"], vec![], vec!["wanted"], vec![]);
        assert!(f.keeps("keepMe", &[]));
        assert!(f.keeps("other", &["wanted".into()]));
        assert!(!f.keeps("other", &["nope".into()]));
    }

    #[test]
    fn include_regex_acts_as_allowlist() {
        let f = filter(
            vec![r"^(get|post)ApiV4Projects.*MergeRequests$"],
            vec![],
            vec![],
            vec![],
        );
        assert!(f.keeps("getApiV4ProjectsIdMergeRequests", &[]));
        assert!(f.keeps("postApiV4ProjectsIdMergeRequests", &[]));
        // Anchored: a trailing segment must not match.
        assert!(!f.keeps("getApiV4ProjectsIdMergeRequestsNotes", &[]));
        // Wrong verb.
        assert!(!f.keeps("deleteApiV4ProjectsIdMergeRequests", &[]));
    }

    #[test]
    fn exclude_regex_wins_over_include() {
        let f = filter(vec!["^getApiV4"], vec![r"(?i)deprecated"], vec![], vec![]);
        assert!(f.keeps("getApiV4Version", &[]));
        assert!(!f.keeps("getApiV4VersionDeprecated", &[]));
    }
}
