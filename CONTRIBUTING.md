# Contributing

Report a bug or request a feature on the
[issue tracker](https://github.com/therealm-tech/oas2mcp/issues), and send
changes as pull requests. A vulnerability goes through
[SECURITY.md](SECURITY.md) instead, never a public issue. For running and
configuring `oas2mcp` itself, see the
[documentation](https://oas2mcp.therealm.tech/); for how it is built, see
[ARCHITECTURE.md](ARCHITECTURE.md).

## Development setup

[mise](https://mise.jdx.dev) installs every tool this repository uses — the
Rust toolchain, Node.js, Python, Task, pre-commit and the binaries its hooks
call — at the versions pinned in [`mise.toml`](mise.toml). Install it and
activate it in your shell
([instructions](https://mise.jdx.dev/getting-started.html)):

**macOS**

```bash
brew install mise
```

**Linux**

```bash
curl https://mise.run | sh
```

Then, from a fresh clone:

```bash
mise trust
```

```bash
mise install
```

```bash
task setup
```

`task setup` installs the documentation site's dependencies and the git hooks;
`task --list` shows the other tasks. Docker is the one prerequisite mise does
not cover, needed only by the end-to-end suite.

A working setup builds and passes the unit tests:

```bash
task test
```

## Running the tests

The unit and in-process tests, which need nothing but the toolchain:

```bash
cargo test
```

A subset, by name filter:

```bash
cargo test protected_resource
```

The [end-to-end suite](tests/e2e/README.md) drives the real binary against a
Keycloak container and a sandbox API; it needs Docker and Python 3. Its README
has the full sequence.

Measure the coverage of the unit, in-process and
[docs site](#writing-the-documentation) tests, with no threshold to meet:

```bash
task coverage
```

The Rust HTML report lands in `target/llvm-cov/html/`, and the docs site's
LCOV report in `docs/coverage/lcov.info`.

A new behaviour comes with tests, and a fix with a test that fails without it.

## Writing the documentation

The user documentation is a [Starlight](https://starlight.astro.build) site in
[`docs/`](docs), published to <https://oas2mcp.therealm.tech/>. Pages
are Markdown under [`docs/src/content/docs/`](docs/src/content/docs); a new page
under `guides/` or `reference/` joins the sidebar by itself, placed by the
`sidebar.order` in its frontmatter. Link another page by its absolute path,
`/guides/metrics/`: the build prefixes it with the version it is under, and a
relative link fails the build.

The site is versioned. The latest release is served at the root, `main` under
`/next/`, and each older minor release, at its last patch, under `/vX.Y/`. Every
version is built from the current site — configuration, theme, components —
around the pages of its tag, so a fix to the site reaches every version while
the pages of a release stay as they were released. A change to the pages
therefore shows under `/next/` until the next release.

Preview it with live reload on <http://localhost:4321/>:

```bash
npm --prefix docs run dev
```

Build a single version, the working tree at the root, which also fails on a
broken internal link or anchor:

```bash
npm --prefix docs run build
```

Build every version as CI does, from the release tags you have fetched:

```bash
git fetch --tags
```

```bash
npm --prefix docs run build:versions
```

Run the site's own tests, for the version planning and the link rewriting:

```bash
npm --prefix docs test
```

## Pre-commit hooks

`task setup` installs the hooks, which every commit then runs. They must pass
before a commit is pushed.

Run them all, or one while iterating:

```bash
task lint
```

```bash
pre-commit run cargo-clippy --all-files
```

[`.pre-commit-config.yaml`](.pre-commit-config.yaml) lists the hooks. The ones
whose failure needs explaining:

| Hook | When it fails |
| --- | --- |
| `no-co-authors` | the commit message has a `Co-Authored-By:` trailer or a `Generated with` line: rewrite it. It runs at `commit-msg` only, so neither `task lint` nor CI runs it; in a clone whose hooks predate it, run `pre-commit install` once |
| `helm-docs` | it regenerated the chart README from `values.yaml`: stage the result |
| `trivy-config` | fix the `Dockerfile` or chart finding, whatever its severity; [`.trivyignore`](.trivyignore) holds the accepted ones, each with its reason |
| `biome` | `npm --prefix docs exec biome check --write .` applies the safe fixes. Like `astro-check`, it needs the site's dependencies, which `task lint` installs |
| `astro-check` | fix the type or content frontmatter error it reports |
| `zizmor` | a workflow breaks GitHub's schema or a security rule (a `${{ }}` in a `run:` script, a broad permission, an unpinned action): fix the workflow, following the audit link it prints |

The same hooks, `commit-msg` aside, run in CI, so `--no-verify` or `SKIP=` only
moves the failure somewhere slower. If a rule is wrong for this repository,
change [`.pre-commit-config.yaml`](.pre-commit-config.yaml) in the same pull
request and say why.

## Continuous integration

| Workflow | Purpose | Reproduce locally |
| --- | --- | --- |
| [`quality`](.github/workflows/quality.yaml) | pre-commit, unit tests, docs site tests, end-to-end suite | `task lint`, `task test`, `task docs:test`, `task e2e` |
| [`coverage`](.github/workflows/coverage.yaml) | test coverage report, not required | `task coverage` |
| [`build`](.github/workflows/build.yaml) | multi-arch image build and Trivy image scan | `docker build .`, then the image scan below |
| [`security`](.github/workflows/security.yaml) | Trivy scan of the code, and daily of the latest release image | the scans below |
| [`docs`](.github/workflows/docs.yaml) | build every version of the documentation site, deploy it from `main` | `task docs` |
| [`release`](.github/workflows/release.yaml) | publish the image and the GitHub Release from a `vX.Y.Z` tag | — (publish only) |
| [`chart`](.github/workflows/chart.yaml) | publish the chart from a `chart-X.Y.Z` tag | — (publish only) |
| [`scorecard`](.github/workflows/scorecard.yaml) | OpenSSF Scorecard grade of the repository, weekly and on `main` | — (graded by the Scorecard API) |
| [`renovate`](.github/workflows/renovate.yaml) | daily dependency update pull requests, configured in [`renovate.json`](renovate.json) | dispatch it on your branch: it dry-runs that branch's `renovate.json` |

`main` only takes changes through a pull request, and a pull request merges
once these checks are green: `pre-commit`, `test`, `docs-test` and `e2e` from
`quality`, and `fs` from `security`. Every other check is informational, and
`scorecard` never runs on a pull request. What may surprise you:

- `build` and `docs` only run when their inputs change; the path filters are
  at the top of each file.
- Renaming, adding or removing a job of `quality` or `security` changes the
  required checks: the ruleset on `main` is updated once the pull request's
  CI has reported the new names, before it merges, or every pull request
  waits on a check that never comes.
- `security` also runs daily against the published image of the latest
  release, so it can go red with no change of yours: a new CVE landed on an
  image nobody rebuilt.
- The Trivy scans fail on a HIGH or CRITICAL finding with a fix available;
  every severity still goes to the repository's **Security** tab.
- The publishing jobs need no secret: the workflow's `GITHUB_TOKEN` pushes to
  GHCR, and its OIDC token signs the attestations.
- `renovate` opens its pull requests with the `RENOVATE_TOKEN` secret rather
  than `GITHUB_TOKEN`, whose pull requests would trigger no CI.

Reproduce the scans:

```bash
# What security / fs gates on:
trivy fs . --scanners vuln,secret \
  --severity HIGH,CRITICAL --ignore-unfixed

# What security / image gates on, against the latest release:
trivy image ghcr.io/therealm-tech/oas2mcp:<version> --platform linux/amd64 \
  --severity HIGH,CRITICAL --ignore-unfixed

# What the build workflow gates on, against a locally built image:
docker build -t oas2mcp:dev .
trivy image oas2mcp:dev --severity HIGH,CRITICAL --ignore-unfixed
```

## Submitting a change

- Commit subjects are imperative and lowercase; follow `git log --oneline`.
- Update the [documentation](docs/src/content/docs), the README, this file or
  [ARCHITECTURE.md](ARCHITECTURE.md) in the same pull request as the change
  that affects them.
- Label the pull request so it lands in the right section of the release notes
  ([`.github/release.yaml`](.github/release.yaml)): one category label, plus
  `breaking` when upgrading forces users to change anything (a flag, an
  environment variable, a chart value, a default). Say in the description what
  breaks and how to migrate.

## Releasing

`task release` is the only way to cut a release: it runs
[`scripts/release.sh`](scripts/release.sh), which bumps the version files,
regenerates what derives from them, commits and tags on an up-to-date `main`,
then asks before pushing. The app (`vX.Y.Z`) and the chart (`chart-X.Y.Z`)
version independently; release either, or both:

```bash
task release -- 1.1.0
```

```bash
task release -- --chart 1.2.0
```

```bash
task release -- 1.1.0 --chart 1.2.0
```

Add `--dry-run` to see what would change. A chart release points `appVersion`
at the app released in the same run, or else at the latest `vX.Y.Z`
(`--app-version` picks another), and pins the `helm install` commands of the
docs to a stable chart. Run `task release -- --help` for the rest.

Pushing is what publishes: the tags trigger
[`release`](.github/workflows/release.yaml) and
[`chart`](.github/workflows/chart.yaml). Any answer but `y`, or no terminal,
leaves the commit and tags local and prints the push command. The push
lands on `main` without a pull request, which only a repository admin may
do: the ruleset on `main` lets admins bypass it for this. Both workflows
refuse a tag that disagrees with the committed version, so a hand-made tag
fails before anything is published.

A pre-release, any version with a `-` such as `1.1.0-rc1`, publishes and stops
there: the image does not move `latest` and gets no GitHub Release, and the
docs keep installing the last stable chart.
