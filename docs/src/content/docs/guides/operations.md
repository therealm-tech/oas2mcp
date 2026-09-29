---
title: Selecting and renaming tools
description: Expose only the operations you need, under names a model can use.
sidebar:
  order: 4
---

A large API turns into a huge tool set: GitLab's OpenAPI document defines ~1700
operations, whose `tools/list` payload is on the order of **half a million
tokens** — it does not fit a model's context, and most MCP clients choke well
before that. Use `--include-regex`/`--exclude-regex` (operation names) and
`--tag`/`--exclude-tag` (OpenAPI tags) to advertise only the operations you
actually need.

An operation is kept when it passes **both** tests: it matches the allowlist
(any `--include-regex` **or** any `--tag`; an empty allowlist means
"everything") and it does not match the denylist (`--exclude-regex` /
`--exclude-tag`, which always win). Name patterns match the operation name —
the `operationId`, or the `<method>_<path>` fallback — with the
[`regex`](https://docs.rs/regex) crate syntax (case-insensitive via a leading
`(?i)`), unanchored unless you anchor them with `^`/`$`.

```bash
# Expose only the Projects and Merge requests endpoints of GitLab:
oas2mcp \
  --openapi-url https://gitlab.com/gitlab-org/gitlab/-/raw/master/doc/api/openapi/openapi_v3.yaml \
  --tag Projects --tag 'Merge requests'
# ~1700 operations → 114 tools (a ~9× smaller tools/list)

# Or select by name and drop the deprecated ones:
oas2mcp --openapi-file api.yaml --include-regex '^getApiV4Projects' --exclude-regex 'Deprecated$'

# Read-only Projects/Groups endpoints, via a regex:
oas2mcp --openapi-file api.yaml --include-regex '^getApiV4(Projects|Groups)'
```

The startup log reports how many operations were kept versus filtered.

## Renaming the exposed tools

The names a real document produces are often unusable as they are. GitLab's
`postApiV4ProjectsIdMergeRequestsNoteableIdDiscussionsDiscussionIdNotes` is 70
characters, while Anthropic and OpenAI both cap tool names at 64
(`^[a-zA-Z0-9_-]{1,64}$`) — and a gateway aggregating several MCP servers
usually prefixes every tool with its backend name (Envoy AI Gateway emits
`<backend>__<tool>`), spending part of that budget before the name is even seen.
Short names are also simply easier for a model to pick from.

`--rename` takes `<regex>=<replacement>` rules, **split on the first `=`**, and
applies them in declaration order — each rule rewrites the output of the
previous one, so a list of abbreviations composes. Every match is replaced
(`replace_all` semantics) and the replacement expands capture groups:

```bash
oas2mcp --openapi-file gitlab.yaml \
  --rename '^(get|post|put|delete|patch)ApiV4=${1}_' \
  --rename 'Projects?Id=proj' \
  --rename 'MergeRequests?(Iid)?=mr' \
  --rename 'NoteableId=' \
  --rename 'Discussions?(DiscussionId)?=disc'

# postApiV4ProjectsIdMergeRequestsNoteableIdDiscussionsDiscussionIdNotes (70)
#   → post_projmrdiscNotes (20)
# getApiV4ProjectsIdMergeRequestsMergeRequestIidDiscussions (57)
#   → get_projmrmrdisc (16)
```

Two things to know about the syntax:

- Write `${1}` rather than `$1` whenever the next character is a letter, digit
  or `_`: the [`regex`](https://docs.rs/regex) crate takes the longest possible
  group name, so `$1_` looks up a group called `1_` and expands to nothing.
- A pattern that must match a literal `=` writes it as the hex escape `\x3D`.
  `[=]` and `\=` mean the same thing to the regex engine, but they spell the
  character out, so the split-on-first-`=` rule would cut the rule in half.

**Filters keep matching the name *before* renaming** — the `operationId`, or the
`<method>_<path>` fallback. That is deliberate: an existing curated
`--include-regex`/`--exclude-regex` allowlist goes on working untouched when you add or edit
rename rules. `--inbound-role-mapper` matches that same
name, so editing a rename rule never changes who may use which tool.

Whatever the rules leave behind is sanitised to `[A-Za-z0-9_-]` and then capped
at `--max-name-len` (64 by default; set it to 56 if a `gitlab__` gateway prefix
has to fit too). A name over the cap is truncated and given a short hash of the
full name — so two long names cannot collapse onto one tool — and both the cap
and every rewritten name are logged, at `warn` and `debug` respectively:

```text
DEBUG rewrote the tool name old=postApiV4ProjectsIdMergeRequests… new=post_projmrdiscNotes
```

A renamed tool also carries its origin in its description (`OpenAPI operationId:
postApiV4Projects…`), so a trace can be mapped back to the document. If two
operations still end up with the same name, both are kept and the later one gets
a `_2` suffix, with a warning naming both.
