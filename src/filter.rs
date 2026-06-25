//! Operation filtering: decide which OpenAPI operations are advertised as MCP
//! tools. Large APIs (e.g. GitLab, ~1700 operations) produce a `tools/list`
//! payload far too big to fit a model's context, so callers restrict the set by
//! operation name (glob or regex) and/or tag.

use regex::Regex;

/// Filtering rules as collected from the CLI / environment. Regexes are already
/// compiled — clap rejects invalid patterns while parsing the arguments.
#[derive(Debug, Default, Clone)]
pub struct FilterConfig {
    /// Name globs to allow (`*`/`?`).
    pub include_globs: Vec<String>,
    /// Name globs to deny.
    pub exclude_globs: Vec<String>,
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
/// Two axes — operation name (glob or regex) and tag — each with an allowlist
/// and a denylist. An operation is kept when it passes **both** the include and
/// the exclude test; a denylist match always wins.
#[derive(Debug, Default, Clone)]
pub struct OperationFilter {
    include_globs: Vec<String>,
    exclude_globs: Vec<String>,
    include_regexes: Vec<Regex>,
    exclude_regexes: Vec<Regex>,
    include_tags: Vec<String>,
    exclude_tags: Vec<String>,
}

impl OperationFilter {
    /// Build a filter from its (already validated) configuration.
    pub fn new(config: FilterConfig) -> Self {
        Self {
            include_globs: config.include_globs,
            exclude_globs: config.exclude_globs,
            include_regexes: config.include_regexes,
            exclude_regexes: config.exclude_regexes,
            include_tags: config.include_tags,
            exclude_tags: config.exclude_tags,
        }
    }

    /// Decide whether an operation with the given tool `name` and `tags` is kept.
    pub fn keeps(&self, name: &str, tags: &[String]) -> bool {
        // Denylist wins: a match on any axis drops the operation.
        if self.matches_name(&self.exclude_globs, &self.exclude_regexes, name)
            || has_tag(&self.exclude_tags, tags)
        {
            return false;
        }

        // Allowlist: when none is configured, everything not denied is kept.
        let has_allow = !self.include_globs.is_empty()
            || !self.include_regexes.is_empty()
            || !self.include_tags.is_empty();
        if !has_allow {
            return true;
        }

        self.matches_name(&self.include_globs, &self.include_regexes, name)
            || has_tag(&self.include_tags, tags)
    }

    fn matches_name(&self, globs: &[String], regexes: &[Regex], name: &str) -> bool {
        globs.iter().any(|p| glob_match(p, name)) || regexes.iter().any(|re| re.is_match(name))
    }
}

fn has_tag(wanted: &[String], tags: &[String]) -> bool {
    wanted
        .iter()
        .any(|w| tags.iter().any(|t| t.eq_ignore_ascii_case(w)))
}

/// Match a shell-style glob against `text`. Supports `*` (any run, including
/// empty) and `?` (exactly one character); all other characters are literal.
/// Matching is case-sensitive — operation names mirror `operationId`.
fn glob_match(pattern: &str, text: &str) -> bool {
    let pat: Vec<char> = pattern.chars().collect();
    let txt: Vec<char> = text.chars().collect();

    // Iterative two-pointer with backtracking on the last `*`.
    let (mut p, mut t) = (0usize, 0usize);
    let (mut star, mut mark) = (None, 0usize);

    while t < txt.len() {
        if p < pat.len() && (pat[p] == '?' || pat[p] == txt[t]) {
            p += 1;
            t += 1;
        } else if p < pat.len() && pat[p] == '*' {
            star = Some(p);
            mark = t;
            p += 1;
        } else if let Some(sp) = star {
            // Backtrack: let the last `*` swallow one more character.
            p = sp + 1;
            mark += 1;
            t = mark;
        } else {
            return false;
        }
    }

    // Consume any trailing `*`s in the pattern.
    while p < pat.len() && pat[p] == '*' {
        p += 1;
    }
    p == pat.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a filter from globs/tags only (no regex), panicking on error.
    fn filter(
        include_globs: Vec<&str>,
        exclude_globs: Vec<&str>,
        include_tags: Vec<&str>,
        exclude_tags: Vec<&str>,
    ) -> OperationFilter {
        OperationFilter::new(FilterConfig {
            include_globs: include_globs.into_iter().map(Into::into).collect(),
            exclude_globs: exclude_globs.into_iter().map(Into::into).collect(),
            include_tags: include_tags.into_iter().map(Into::into).collect(),
            exclude_tags: exclude_tags.into_iter().map(Into::into).collect(),
            ..Default::default()
        })
    }

    #[test]
    fn glob_literal_and_wildcards() {
        assert!(glob_match("getPet", "getPet"));
        assert!(!glob_match("getPet", "getPets"));
        assert!(glob_match(
            "getApiV4Projects*",
            "getApiV4ProjectsIdMergeRequests"
        ));
        assert!(glob_match("*Projects*", "getApiV4ProjectsId"));
        assert!(glob_match("get?et", "getPet"));
        assert!(!glob_match("get?et", "getPets"));
        assert!(glob_match("*", "anything"));
        assert!(glob_match("a*b*c", "axxbyyc"));
        assert!(!glob_match("a*b*c", "axxbyy"));
    }

    #[test]
    fn empty_filter_keeps_everything() {
        let f = OperationFilter::default();
        assert!(f.keeps("anything", &[]));
    }

    #[test]
    fn include_globs_act_as_allowlist() {
        let f = filter(vec!["getApiV4Projects*"], vec![], vec![], vec![]);
        assert!(f.keeps("getApiV4ProjectsId", &[]));
        assert!(!f.keeps("getApiV4Groups", &[]));
    }

    #[test]
    fn exclude_globs_win_over_include() {
        let f = filter(vec!["getApiV4*"], vec!["*Deprecated"], vec![], vec![]);
        assert!(f.keeps("getApiV4Version", &[]));
        assert!(!f.keeps("getApiV4VersionDeprecated", &[]));
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
        let f = filter(vec!["keepMe"], vec![], vec!["wanted"], vec![]);
        assert!(f.keeps("keepMe", &[]));
        assert!(f.keeps("other", &["wanted".into()]));
        assert!(!f.keeps("other", &["nope".into()]));
    }

    #[test]
    fn include_regex_acts_as_allowlist() {
        let f = OperationFilter::new(FilterConfig {
            include_regexes: vec![Regex::new(r"^(get|post)ApiV4Projects.*MergeRequests$").unwrap()],
            ..Default::default()
        });
        assert!(f.keeps("getApiV4ProjectsIdMergeRequests", &[]));
        assert!(f.keeps("postApiV4ProjectsIdMergeRequests", &[]));
        // Anchored: a trailing segment must not match.
        assert!(!f.keeps("getApiV4ProjectsIdMergeRequestsNotes", &[]));
        // Wrong verb.
        assert!(!f.keeps("deleteApiV4ProjectsIdMergeRequests", &[]));
    }

    #[test]
    fn exclude_regex_wins_over_include() {
        let f = OperationFilter::new(FilterConfig {
            include_globs: vec!["*".into()],
            exclude_regexes: vec![Regex::new(r"(?i)deprecated").unwrap()],
            ..Default::default()
        });
        assert!(f.keeps("getApiV4Version", &[]));
        assert!(!f.keeps("getApiV4VersionDeprecated", &[]));
    }
}
