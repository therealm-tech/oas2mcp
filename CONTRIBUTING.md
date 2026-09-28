# Contributing

Issues and pull requests are welcome on
[GitHub](https://github.com/therealm-tech/oas2mcp). For running and configuring
`oas2mcp` itself, see the [README](README.md); for how it is built, see
[ARCHITECTURE.md](ARCHITECTURE.md).

## Development setup

- **Rust** through [`rustup`](https://rustup.rs): the toolchain is pinned in
  [`rust-toolchain.toml`](rust-toolchain.toml) and installed on the first
  `cargo` invocation.
- **[pre-commit](https://pre-commit.com)** and the binaries its hooks call:
  `hadolint`, `actionlint`, `shellcheck`, `helm` and `helm-docs`.
- **Docker and Python 3**, only for the end-to-end suite.

**macOS**

```bash
brew install pre-commit hadolint actionlint shellcheck helm norwoodj/tap/helm-docs
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
[helm-docs](https://github.com/norwoodj/helm-docs/releases) — the `pre-commit`
job in [`quality.yaml`](.github/workflows/quality.yaml) installs exactly these.

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

The same hooks run in CI, so `--no-verify` or `SKIP=` only moves the failure
somewhere slower. If a rule is wrong for this repository, change
[`.pre-commit-config.yaml`](.pre-commit-config.yaml) in the same pull request
and say why.

## Continuous integration

| Workflow | Triggers on | What it does | Reproduce locally |
| --- | --- | --- | --- |
| [`quality`](.github/workflows/quality.yaml) | pull requests, pushes to `main` | pre-commit, `cargo test`, the end-to-end suite, Trivy filesystem scan | `pre-commit run --all-files`, `cargo test`, [e2e](tests/e2e/README.md), the Trivy command below |
| [`build`](.github/workflows/build.yaml) | pull requests and pushes to `main` touching the build inputs, manual | multi-arch image build and Trivy image scan; pushes only on manual dispatch or a release | `docker build .` and the Trivy command below |
| [`chart`](.github/workflows/chart.yaml) | `chart-X.Y.Z` tag, manual | publishes the chart to `ghcr.io/therealm-tech/charts` | — (publish only) |
| [`release`](.github/workflows/release.yaml) | `vX.Y.Z` tag | checks the tag matches `Cargo.toml`, builds and pushes the image, creates the GitHub Release | — (publish only) |

The publishing jobs use the workflow's `GITHUB_TOKEN` for the registry; nothing
else is needed.

The chart is versioned and released independently of the app. A `vX.Y.Z` tag
whose version differs from `Cargo.toml` fails `release` before anything is
published: the tag names the image, but `Cargo.toml` is what
`oas2mcp --version` reports.

### Security scanning

[Trivy](https://trivy.dev) runs in two places, and both fail the build on a
**HIGH** or **CRITICAL** finding that has a fix available:

- **quality / trivy** — a filesystem scan of the repository: crate advisories
  from `Cargo.lock`, leaked secrets, and `Dockerfile` and Helm chart
  misconfiguration.
- **build / scan the image** — scans the container image the commit actually
  produces, which is what catches CVEs in the base layer. This runs on releases
  too: a HIGH/CRITICAL finding fails the build, which blocks the `manifest`
  job, so no usable tag is ever published. Note it covers the base layer only —
  the runtime image holds a compiled binary, so Trivy sees no Rust dependencies
  there; those are covered by the `Cargo.lock` scan above.

The runtime image is distroless, so the image scan mostly covers the base
layer; see [ARCHITECTURE.md](ARCHITECTURE.md#design-decisions).

Each runs twice, deliberately: once reporting **every** severity to the
repository's **Security** tab, then once more gating the build on HIGH and
CRITICAL. Advisories with no released fix are excluded from both.

Trivy renders the chart itself, but only when handed the values its templates
require (`TRIVY_HELM_VALUES`). Without them it logs a render error, scans no
chart at all, and still reports success — so keep that variable set.

Reproduce either scan locally:

```bash
# What the quality workflow gates on:
TRIVY_HELM_VALUES=charts/oas2mcp/values-lint.yaml \
  trivy fs . --scanners vuln,secret,misconfig \
    --severity HIGH,CRITICAL --ignore-unfixed \
    --skip-files tests/fixtures/test_rsa_key.pem

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

Doing it by hand works too, as long as `Cargo.toml` already carries the same
version — otherwise the `release` workflow fails the version check:

```bash
# Release the application (image + GitHub Release):
git tag v0.1.0 && git push origin v0.1.0

# Release the Helm chart (OCI push), independently:
git tag chart-0.1.0 && git push origin chart-0.1.0
```

## Submitting a change

- Commit subjects are imperative and lowercase; follow `git log --oneline`.
- Update the README, this file or [ARCHITECTURE.md](ARCHITECTURE.md) in the same
  pull request as the change that affects them.
- Label the pull request so it lands in the right section of the release notes
  ([`.github/release.yaml`](.github/release.yaml)): one category label, plus
  `breaking` when upgrading forces users to change anything (a flag, an
  environment variable, a chart value, a default). Say in the description what
  breaks and how to migrate.
