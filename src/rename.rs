//! Tool-name rewriting: turn an OpenAPI operation name into a short, readable
//! MCP tool name.
//!
//! Names taken straight from a real-world document are frequently unusable.
//! GitLab's `postApiV4ProjectsIdMergeRequestsNoteableIdDiscussionsDiscussionIdNotes`
//! is 70 characters on its own, while both Anthropic and OpenAI cap tool names
//! at 64 (`^[a-zA-Z0-9_-]{1,64}$`) — and a gateway aggregating several MCP
//! servers usually prefixes every tool with its backend name (Envoy AI Gateway
//! emits `<backend>__<tool>`), eating into that budget before the name is even
//! seen. Short names are also simply better: a model picks tools by name, and
//! `post_projmrdiscNotes` is easier to select than a wall of CamelCase.
//!
//! The rewrite is a chain of regex rules, then sanitisation, then a hard length
//! cap. **Operation filtering deliberately runs before all of this**, on the raw
//! name — see [`crate::filter`] and `tools::build_tools`.

use regex::Regex;

/// Default maximum length of a tool name, matching the limit Anthropic and
/// OpenAI both enforce on tool names.
pub const DEFAULT_MAX_NAME_LEN: usize = 64;

/// Hex characters of the disambiguating hash appended to a truncated name.
const HASH_LEN: usize = 6;

/// One rewrite rule: a compiled pattern and what to replace every match with.
///
/// Written on the command line as `<regex>=<replacement>`, split on the **first**
/// `=`. That keeps the split rule trivial; a pattern that needs to match a
/// literal `=` writes it as the hex escape `\x3D`, which the regex crate
/// understands and which contains no `=` character for the split to trip on.
/// (`[=]` and `\=` are valid regexes for the same thing, but both spell the
/// character out, so the split would cut the rule in half.)
#[derive(Debug, Clone)]
pub struct RenameRule {
    pattern: Regex,
    replacement: String,
}

impl RenameRule {
    /// Parse a `<regex>=<replacement>` rule. Used as a `clap` `value_parser`, so
    /// an invalid pattern fails at startup rather than on the first request.
    pub fn parse(raw: &str) -> Result<Self, String> {
        let (pattern, replacement) = raw
            .split_once('=')
            .ok_or_else(|| format!("rename rule `{raw}` is not in `<regex>=<replacement>` form"))?;
        if pattern.is_empty() {
            return Err(format!("rename rule `{raw}` has an empty pattern"));
        }
        let pattern = Regex::new(pattern).map_err(|err| format!("invalid regex: {err}"))?;
        Ok(Self {
            pattern,
            replacement: replacement.to_string(),
        })
    }

    /// Apply the rule to every match in `name`.
    pub(crate) fn apply<'a>(&self, name: &'a str) -> std::borrow::Cow<'a, str> {
        self.pattern.replace_all(name, self.replacement.as_str())
    }
}

/// Renaming rules as collected from the CLI / environment. Regexes are already
/// compiled — clap rejects invalid patterns while parsing the arguments.
#[derive(Debug, Clone)]
pub struct RenameConfig {
    /// Rewrite rules, applied in declaration order.
    pub rules: Vec<RenameRule>,
    /// Maximum length of the resulting name.
    pub max_len: usize,
}

impl Default for RenameConfig {
    fn default() -> Self {
        Self {
            rules: Vec::new(),
            max_len: DEFAULT_MAX_NAME_LEN,
        }
    }
}

/// Rewrites an operation name into the MCP tool name that is advertised.
///
/// The pipeline is, in order: the rules chain, sanitisation to the allowed
/// character set, then the length cap. Every rule is applied to the output of
/// the previous one — a match does not stop the chain, which is what makes a
/// list of abbreviation rules useful.
#[derive(Debug, Clone, Default)]
pub struct ToolRenamer {
    rules: Vec<RenameRule>,
    max_len: usize,
}

impl ToolRenamer {
    /// Build a renamer from its (already validated) configuration.
    pub fn new(config: RenameConfig) -> Self {
        Self {
            rules: config.rules,
            max_len: config.max_len,
        }
    }

    /// Rewrite a raw operation name into the final tool name.
    pub fn rename(&self, raw: &str) -> String {
        let mut name = raw.to_string();
        for rule in &self.rules {
            name = rule.apply(&name).into_owned();
        }

        self.capped(sanitize_name(&name))
    }

    /// Enforce the length cap: truncate and append a short hash of the **full**
    /// name, so two long names cannot collapse onto the same tool.
    fn capped(&self, name: String) -> String {
        // `max_len` only comes from the CLI, where it defaults to 64; 0 would
        // leave no name at all, so treat it as "no cap configured".
        let max_len = if self.max_len == 0 {
            DEFAULT_MAX_NAME_LEN
        } else {
            self.max_len
        };
        // `sanitize_name` guarantees ASCII, so byte slicing is character slicing.
        if name.len() <= max_len {
            return name;
        }

        let hash = short_hash(&name);
        let keep = max_len.saturating_sub(HASH_LEN + 1);
        let truncated = if keep == 0 {
            // Absurdly small cap: the hash alone is all that fits.
            hash[..hash.len().min(max_len)].to_string()
        } else {
            format!("{}_{hash}", name[..keep].trim_end_matches('_'))
        };
        tracing::warn!(
            name = %name,
            truncated = %truncated,
            max_len,
            "tool name exceeds the maximum length; truncated with a hash suffix"
        );
        truncated
    }
}

/// Turn an arbitrary string into a valid MCP tool name (`[A-Za-z0-9_-]+`).
/// Length is **not** enforced here — that is the renamer's cap, which keeps the
/// truncation visible in the logs instead of silently dropping characters.
pub fn sanitize_name(raw: &str) -> String {
    let mut name: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    name = name.trim_matches('_').to_string();
    if name.is_empty() {
        name = "operation".to_string();
    }
    name
}

/// A short, stable digest of `name` (FNV-1a, truncated to [`HASH_LEN`] hex
/// characters). Stability across runs is the point: a tool name that changes
/// between two restarts of the same server would break every client that
/// cached it.
fn short_hash(name: &str) -> String {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    let mut hash = OFFSET;
    for byte in name.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    format!("{:0width$x}", hash & 0x00ff_ffff, width = HASH_LEN)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A renamer over the given `<regex>=<replacement>` rules, with the default
    /// length cap.
    fn renamer(rules: &[&str]) -> ToolRenamer {
        ToolRenamer::new(RenameConfig {
            rules: rules
                .iter()
                .map(|raw| RenameRule::parse(raw).expect("valid rule"))
                .collect(),
            ..Default::default()
        })
    }

    /// The abbreviation rules documented in the README, against GitLab's names.
    const GITLAB_RULES: &[&str] = &[
        r"^(get|post|put|delete|patch)ApiV4=${1}_",
        "Projects?Id=proj",
        "MergeRequests?(Iid)?=mr",
        "NoteableId=",
        "Discussions?(DiscussionId)?=disc",
    ];

    #[test]
    fn a_rule_splits_on_the_first_equals() {
        let rule = RenameRule::parse("a=b=c").expect("valid rule");
        assert_eq!(rule.pattern.as_str(), "a");
        assert_eq!(rule.replacement, "b=c");
    }

    #[test]
    fn a_literal_equals_is_written_as_a_hex_escape() {
        // `\x3D` matches `=` without spelling it out, so the first `=` of the
        // value is still the separator.
        let rule = RenameRule::parse(r"x\x3Dy=z").expect("valid rule");
        assert_eq!(rule.pattern.as_str(), r"x\x3Dy");
        assert_eq!(rule.replacement, "z");
        assert_eq!(rule.apply("ax=yb"), "azb");

        // The character class spelling works the same way, as long as the `=`
        // itself stays escaped.
        let rule = RenameRule::parse(r"[\x3D]=eq").expect("valid rule");
        assert_eq!(rule.apply("a=b"), "aeqb");
    }

    #[test]
    fn a_malformed_rule_is_rejected() {
        // No separator at all.
        assert!(RenameRule::parse("noSeparator").is_err());
        // An empty pattern would match at every position.
        assert!(RenameRule::parse("=replacement").is_err());
        // An invalid regex fails here rather than at the first request.
        assert!(RenameRule::parse("(unclosed=x").is_err());
    }

    #[test]
    fn an_empty_replacement_deletes_the_match() {
        assert_eq!(
            renamer(&["NoteableId="]).rename("getNoteableIdNotes"),
            "getNotes"
        );
    }

    #[test]
    fn rules_chain_in_declaration_order() {
        // The second rule sees the output of the first, and a match does not
        // stop the chain.
        assert_eq!(renamer(&["a=b", "b=c"]).rename("aXa"), "cXc");
        // Order matters: the reverse chain leaves the `a`s alone.
        assert_eq!(renamer(&["b=c", "a=b"]).rename("aXa"), "bXb");
    }

    #[test]
    fn replacements_expand_capture_groups() {
        // Numbered, braced (needed when the replacement continues with a word
        // character), and named groups.
        assert_eq!(
            renamer(&[r"^(get)Api=$1"]).rename("getApiThing"),
            "getThing"
        );
        assert_eq!(
            renamer(&[r"^(get)Api=${1}_"]).rename("getApiThing"),
            "get_Thing"
        );
        assert_eq!(
            renamer(&[r"^(?<verb>get)Api=${verb}_"]).rename("getApiThing"),
            "get_Thing"
        );
    }

    #[test]
    fn the_result_is_sanitized_after_renaming() {
        // A replacement may introduce characters MCP does not allow in a name.
        assert_eq!(
            renamer(&["Api=/api."]).rename("getApiThing"),
            "get_api_Thing"
        );
    }

    #[test]
    fn an_unconfigured_renamer_only_sanitizes() {
        let renamer = ToolRenamer::default();
        assert_eq!(renamer.rename("getPet"), "getPet");
        assert_eq!(renamer.rename("get /pets/{petId}"), "get__pets__petId");
        assert_eq!(renamer.rename("//"), "operation");
    }

    #[test]
    fn sanitizing_no_longer_truncates() {
        // The cap is the renamer's job, so it can be logged; sanitisation on its
        // own must not silently drop the tail of a name.
        let long = "a".repeat(200);
        assert_eq!(sanitize_name(&long).len(), 200);
    }

    #[test]
    fn a_long_name_is_truncated_with_a_stable_hash() {
        let renamer = ToolRenamer::new(RenameConfig {
            max_len: 20,
            ..Default::default()
        });
        let name = renamer.rename("getApiV4ProjectsIdMergeRequestsNotes");
        assert_eq!(name.len(), 20);
        // Deterministic: the same input yields the same name on every run.
        assert_eq!(renamer.rename("getApiV4ProjectsIdMergeRequestsNotes"), name);

        // Two names sharing the kept prefix stay distinct, which a plain
        // truncation could not guarantee.
        let other = renamer.rename("getApiV4ProjectsIdMergeRequestsAwardEmoji");
        assert_ne!(name, other);
        assert!(name.starts_with("getApiV4Proje"), "{name}");
        assert!(other.starts_with("getApiV4Proje"), "{other}");
    }

    #[test]
    fn the_hash_covers_the_whole_name() {
        // Two names differing only past the truncation point must not collapse:
        // the digest is taken over the full name, not the kept prefix.
        let renamer = ToolRenamer::new(RenameConfig {
            max_len: 16,
            ..Default::default()
        });
        assert_ne!(
            renamer.rename("sameForeverAndThenSomeA"),
            renamer.rename("sameForeverAndThenSomeB")
        );
    }

    #[test]
    fn a_name_at_the_cap_is_left_alone() {
        let renamer = ToolRenamer::new(RenameConfig {
            max_len: 10,
            ..Default::default()
        });
        assert_eq!(renamer.rename("exactlyTen"), "exactlyTen");
    }

    #[test]
    fn an_absurdly_small_cap_still_yields_a_name() {
        let renamer = ToolRenamer::new(RenameConfig {
            max_len: 4,
            ..Default::default()
        });
        let name = renamer.rename("getApiV4Projects");
        assert_eq!(name.len(), 4);
        assert!(!name.is_empty());
    }

    #[test]
    fn the_gitlab_rules_fit_the_gateway_budget() {
        // A gateway that namespaces tools with its backend name (Envoy AI
        // Gateway emits `<backend>__<tool>`) spends part of the 64-character
        // limit before the tool name is seen: with `gitlab__` that leaves 56.
        const BUDGET: usize = 64 - "gitlab__".len();

        let renamer = renamer(GITLAB_RULES);
        let cases = [
            (
                "postApiV4ProjectsIdMergeRequestsNoteableIdDiscussionsDiscussionIdNotes",
                "post_projmrdiscNotes",
            ),
            (
                "getApiV4ProjectsIdMergeRequestsMergeRequestIidDiscussions",
                "get_projmrmrdisc",
            ),
            (
                "putApiV4ProjectsIdMergeRequestsNoteableIdDiscussionsDiscussionIdNotesNoteId",
                "put_projmrdiscNotesNoteId",
            ),
            ("getApiV4ProjectsIdMergeRequests", "get_projmr"),
            ("deleteApiV4ProjectsId", "delete_proj"),
        ];

        for (raw, expected) in cases {
            let renamed = renamer.rename(raw);
            assert_eq!(renamed, expected, "{raw}");
            assert!(
                renamed.len() <= BUDGET,
                "`{renamed}` ({} chars) does not fit the {BUDGET}-char budget",
                renamed.len()
            );
            // No hash suffix: the rules did the work, the cap never fired.
            assert!(!renamed.contains("__"), "{renamed}");
        }
    }
}
