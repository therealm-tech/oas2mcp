# Contributing

Issues and pull requests are welcome on
[GitHub](https://github.com/therealm-tech/oas2mcp). For running and configuring
`oas2mcp` itself, see the [documentation](https://oas2mcp.therealm.tech/);
for how it is built, see
[ARCHITECTURE.md](ARCHITECTURE.md).

## Development setup

- **Rust** through [`rustup`](https://rustup.rs): the toolchain is pinned in
  [`rust-toolchain.toml`](rust-toolchain.toml) and installed on the first
  `cargo` invocation.
- **[pre-commit](https://pre-commit.com)** and the binaries its hooks call:
  `hadolint`, `actionlint`, `shellcheck`, `helm`, `helm-docs` and `trivy`.
- **Docker and Python 3**, only for the end-to-end suite.
- **Node.js**, at the version in [`docs/.nvmrc`](docs/.nvmrc), for the
  documentation site and the pre-commit hooks that check it.

**macOS**

```bash
brew install pre-commit hadolint actionlint shellcheck helm norwoodj/tap/helm-docs trivy
```

**Linux**

```bash
pipx install pre-commit
```

and the other binaries from their release pages:
[hadolint](https://github.com/hadolint/hadolint/releases),
[actionlint](https://github.com/rhysd/actionlint/releases),
[shellcheck](https://github.com/koalaman/shellcheck#installing),
[helm](https://helm.sh/docs/intro/install/),
[helm-docs](https://github.com/norwoodj/helm-docs/releases),
[trivy](https://trivy.dev/latest/getting-started/installation/) — the `pre-commit`
job in [`quality.yaml`](.github/workflows/quality.yaml) installs exactly these.

Install the documentation site's dependencies, which its pre-commit hooks run
from:

```bash
npm ci --prefix docs
```

A working setup builds and passes the unit tests:

```bash
cargo test
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

Install the hooks once, then every commit runs them:

```bash
pre-commit install
```

Run them all, or one while iterating:

```bash
pre-commit run --all-files
```

```bash
pre-commit run cargo-clippy --all-files
```

| Hook | Checks | Fix |
| --- | --- | --- |
| `trailing-whitespace`, `end-of-file-fixer` | whitespace | fixes itself; re-stage |
| `check-yaml`, `check-merge-conflict`, `check-added-large-files` | file sanity | by hand |
| `detect-private-key` | committed keys (test fixtures excluded) | remove the key |
| `cargo-fmt` | Rust formatting | `cargo fmt --all` |
| `cargo-clippy` | Rust lints, warnings denied | by hand |
| `shellcheck` | shell scripts | by hand |
| `actionlint` | GitHub workflows | by hand |
| `helm-lint` | the chart renders | by hand |
| `helm-docs` | the chart README matches `values.yaml` | fixes itself; re-stage |
| `hadolint` | the `Dockerfile` | by hand |
| `trivy-config` | `Dockerfile` and rendered chart misconfigurations, HIGH and CRITICAL | by hand |
| `biome` | documentation site formatting and lints | `npm --prefix docs exec biome check --write .` |
| `astro-check` | documentation site types and content frontmatter | by hand |

The same hooks run in CI, so `--no-verify` or `SKIP=` only moves the failure
somewhere slower. If a rule is wrong for this repository, change
[`.pre-commit-config.yaml`](.pre-commit-config.yaml) in the same pull request
and say why.

## Continuous integration

| Workflow | Triggers on | What it does | Reproduce locally |
| --- | --- | --- | --- |
| [`quality`](.github/workflows/quality.yaml) | pull requests, pushes to `main` | pre-commit, `cargo test`, the docs site tests, the end-to-end suite | `pre-commit run --all-files`, `cargo test`, `npm --prefix docs test`, [e2e](tests/e2e/README.md) |
| [`build`](.github/workflows/build.yaml) | pull requests and pushes to `main` touching the build inputs, manual | multi-arch image build and Trivy image scan; pushes only on manual dispatch or a release | `docker build .` and the Trivy command below |
| [`security`](.github/workflows/security.yaml) | pull requests, pushes to `main`, daily, manual | Trivy filesystem scan; daily and manual runs also scan the image of the latest release | the Trivy commands below |
| [`docs`](.github/workflows/docs.yaml) | pull requests and pushes to `main` touching `docs/` or the logo, a stable release, manual | builds every version of the documentation site, checking its internal links; on `main`, deploys it to GitHub Pages | `npm --prefix docs run build:versions` |
| [`chart`](.github/workflows/chart.yaml) | `chart-X.Y.Z` tag, manual | on a tag, checks it matches `Chart.yaml`; publishes the chart to `ghcr.io/therealm-tech/charts` | — (publish only) |
| [`release`](.github/workflows/release.yaml) | `vX.Y.Z` tag | checks the tag matches `Cargo.toml`, builds and pushes the image, creates the GitHub Release and rebuilds the documentation (not for a pre-release) | — (publish only) |

The publishing jobs use the workflow's `GITHUB_TOKEN` for the registry; nothing
else is needed.

The chart is versioned and released independently of the app. A `vX.Y.Z` tag
whose version differs from `Cargo.toml` fails `release` before anything is
published: the tag names the image, but `Cargo.toml` is what
`oas2mcp --version` reports. A `chart-X.Y.Z` tag that differs from the
`version` in `Chart.yaml` fails `chart` the same way.

A pre-release tag — any version with a `-`, such as `v0.9.0-rc1` — publishes
the image under that version and stops there: it does not move `latest` and
creates no GitHub Release, since an rc exists to be tested. The notes of a
stable release start from the previous stable tag, so they cover everything
since that release, rc tags included.

### Security scanning

[Trivy](https://trivy.dev) runs in four places. The CI scans fail on a **HIGH**
or **CRITICAL** finding that has a fix available:

- **the `trivy-config` pre-commit hook** — `Dockerfile` and Helm chart
  misconfigurations, at commit time: they only change with the code.
- **security / fs** — a filesystem scan of the repository: crate advisories
  from `Cargo.lock` and leaked secrets.
- **security / image** — daily, the image of the latest release as published
  on GHCR, for each architecture. A new CVE lands against an image nobody
  rebuilt; a red scheduled run is the notification.
- **build / scan the image** — scans the container image the commit actually
  produces, before anything is pushed. This runs on releases too: a
  HIGH/CRITICAL finding fails the build, which blocks the `manifest` job, so no
  usable tag is ever published.

The image scans cover the base layer only — the runtime image holds a compiled
binary, so Trivy sees no Rust dependencies there; those are covered by the
`Cargo.lock` scan. The runtime image is distroless; see
[ARCHITECTURE.md](ARCHITECTURE.md#design-decisions).

Each CI scan runs twice, deliberately: once reporting **every** severity to the
repository's **Security** tab — misconfigurations included — then once more
gating the build on HIGH and CRITICAL. Advisories with no released fix are
reported but never fail the build.

Trivy renders the chart itself, but only when handed the values its templates
require (`--helm-values`, `TRIVY_HELM_VALUES` in CI). Without them it logs a
render error, scans no chart at all, and still reports success — so keep them
set. The misconfiguration checks also read their parameters from
[`.trivy/`](.trivy) (`--config-data`, `TRIVY_CONFIG_DATA`), such as the
registries images may come from.

Reproduce the scans locally:

```bash
# What security / fs gates on:
trivy fs . --scanners vuln,secret \
  --severity HIGH,CRITICAL --ignore-unfixed \
  --skip-files tests/fixtures/test_rsa_key.pem

# What security / image gates on, against the latest release:
trivy image ghcr.io/therealm-tech/oas2mcp:<version> --platform linux/amd64 \
  --severity HIGH,CRITICAL --ignore-unfixed

# What the build workflow gates on, against a locally built image:
docker build -t oas2mcp:dev .
trivy image oas2mcp:dev --severity HIGH,CRITICAL --ignore-unfixed
```

## Cutting a release

The app and the chart have separate release lifecycles.

The helper script bumps the version files, runs the checks, commits, tags and
pushes — which is what triggers the workflows. Release either side, or both at
once:

```bash
# The application: bumps Cargo.toml + Cargo.lock, tags v0.4.0.
scripts/release.sh 0.4.0

# The chart: bumps Chart.yaml + the generated chart README, tags chart-0.5.0.
scripts/release.sh --chart 0.5.0

# Both: as above, and `appVersion` is pointed at the app version being
# released, since the chart now targets that image.
scripts/release.sh 0.4.0 --chart 0.5.0
```

A chart-only release points `appVersion` at the latest `vX.Y.Z` tag, so a chart
published on its own still ships against the newest app image instead of
quietly lagging behind it. The script refuses to run on a dirty tree, off
`main`, out of sync with `origin/main`, or when a tag already exists. Useful
flags: `--skip-tests`, `--no-push` (commit and tag locally only), `-y` (no
confirmation prompt). Bumping the chart needs `helm` and `helm-docs` on `PATH`.

Doing it by hand works too, as long as `Cargo.toml` (for the app) or
`Chart.yaml` (for the chart) already carries the same version — otherwise the
workflow fails its version check:

```bash
# Release the application (image + GitHub Release):
git tag v0.1.0 && git push origin v0.1.0

# Release the Helm chart (OCI push), independently:
git tag chart-0.1.0 && git push origin chart-0.1.0
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
