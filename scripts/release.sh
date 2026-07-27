#!/usr/bin/env bash
# Cut a release: align the version files, commit, tag and push. The `release`
# workflow builds the image and drafts the notes off the `vX.Y.Z` tag; the
# `chart` workflow publishes the chart off the `chart-X.Y.Z` tag.
#
# The app and the chart version independently, so either can be released alone.
set -euo pipefail

readonly BRANCH="main"
readonly MANIFEST="Cargo.toml"
readonly CHART="charts/oas2mcp/Chart.yaml"

tmpfile=""
trap 'rm -f "$tmpfile"' EXIT

usage() {
    cat <<'EOF'
Usage: scripts/release.sh [options] [<version>]

Cut an oas2mcp release. Give an app version, a chart version, or both.

Arguments:
  <version>            App version to release, with or without a leading `v`
                       (e.g. `0.4.0` or `v0.4.0`). Bumps `Cargo.toml` and tags
                       `vX.Y.Z`.

Options:
  --chart <version>    Also release the Helm chart at this version: bumps the
                       chart `version` in `Chart.yaml` and tags `chart-X.Y.Z`.
                       When an app version is released too, `appVersion` is
                       pointed at it.
  --skip-tests         Skip the test suite before tagging.
  --no-push            Commit and tag locally, but push nothing.
  -y, --yes            Do not ask for confirmation before pushing.
  -h, --help           Show this help.

Examples:
  scripts/release.sh 0.4.0                  # app only
  scripts/release.sh --chart 0.5.0          # chart only
  scripts/release.sh 0.4.0 --chart 0.5.0    # both, appVersion becomes 0.4.0
EOF
}

info() {
    printf '\033[1;34m==>\033[0m %s\n' "$*"
}

die() {
    printf '\033[1;31merror:\033[0m %s\n' "$*" >&2
    exit 1
}

require_semver() {
    printf '%s' "$1" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$' ||
        die "not a SemVer version: $1"
}

require_tag_absent() {
    local tag="$1"

    if git rev-parse -q --verify "refs/tags/$tag" >/dev/null; then
        die "tag $tag already exists locally"
    fi
    if git ls-remote --exit-code --tags origin "refs/tags/$tag" >/dev/null 2>&1; then
        die "tag $tag already exists on origin"
    fi
}

# Rewrite a file through a sed script. Not `sed -i`: the in-place flag takes an
# argument on BSD sed and none on GNU sed, so there is no portable spelling.
rewrite() {
    local file="$1" script="$2"

    tmpfile="$(mktemp)"
    sed "$script" "$file" >"$tmpfile"
    cat "$tmpfile" >"$file"
    rm -f "$tmpfile"
    tmpfile=""
}

# Read the `version` field of the `[package]` section, and nothing else: a
# dependency further down the file also has a `version` key.
manifest_version() {
    sed -n '/^\[package\]/,/^\[/{s/^version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p;}' "$MANIFEST"
}

set_manifest_version() {
    rewrite "$MANIFEST" '/^\[package\]/,/^\[/ s/^version[[:space:]]*=.*/version = "'"$1"'"/'
}

chart_version() {
    sed -n 's/^version:[[:space:]]*"\{0,1\}\([^"[:space:]]*\)"\{0,1\}.*/\1/p' "$CHART"
}

set_chart_version() {
    rewrite "$CHART" 's/^version:.*/version: '"$1"'/'
}

set_chart_app_version() {
    rewrite "$CHART" 's/^appVersion:.*/appVersion: "'"$1"'"/'
}

confirm() {
    local prompt="$1" answer

    if [ ! -t 0 ]; then
        die "$prompt (no tty to ask on — rerun with --yes)"
    fi

    read -r -p "$prompt [y/N] " answer
    case "$answer" in
        y | Y | yes | YES) return 0 ;;
        *) return 1 ;;
    esac
}

main() {
    local version="" chart="" skip_tests=false push=true assume_yes=false

    while [ $# -gt 0 ]; do
        case "$1" in
            --chart)
                [ $# -ge 2 ] || die "--chart needs a version"
                chart="$2"
                shift
                ;;
            --chart=*) chart="${1#--chart=}" ;;
            --skip-tests) skip_tests=true ;;
            --no-push) push=false ;;
            -y | --yes) assume_yes=true ;;
            -h | --help)
                usage
                return 0
                ;;
            -*) die "unknown option: $1" ;;
            *)
                [ -z "$version" ] || die "unexpected argument: $1"
                version="$1"
                ;;
        esac
        shift
    done

    if [ -z "$version" ] && [ -z "$chart" ]; then
        usage >&2
        die "nothing to release: give <version>, --chart <version>, or both"
    fi

    local tags=()
    if [ -n "$version" ]; then
        version="${version#v}"
        require_semver "$version"
        tags+=("v$version")
    fi
    if [ -n "$chart" ]; then
        chart="${chart#v}"
        require_semver "$chart"
        tags+=("chart-$chart")
    fi

    cd "$(git rev-parse --show-toplevel)"

    if [ -n "$version" ]; then
        command -v cargo >/dev/null || die "cargo is not on PATH"
        [ -f "$MANIFEST" ] || die "$MANIFEST not found"
    fi
    if [ -n "$chart" ]; then
        [ -f "$CHART" ] || die "$CHART not found"
        # The chart README badges are generated from Chart.yaml; without
        # helm-docs the commit would fail the `helm-docs` pre-commit hook.
        command -v helm >/dev/null || die "helm is not on PATH"
        command -v helm-docs >/dev/null || die "helm-docs is not on PATH"
    fi

    # A dirty tree would smuggle unrelated changes into the release commit.
    [ -z "$(git status --porcelain)" ] || die "the working tree is not clean"

    local current_branch
    current_branch="$(git rev-parse --abbrev-ref HEAD)"
    [ "$current_branch" = "$BRANCH" ] || die "not on $BRANCH (on $current_branch)"

    info "fetching origin"
    git fetch --quiet --tags origin "$BRANCH"

    if [ "$(git rev-parse HEAD)" != "$(git rev-parse FETCH_HEAD)" ]; then
        die "$BRANCH is not in sync with origin/$BRANCH — pull or push first"
    fi

    local tag
    for tag in "${tags[@]}"; do
        require_tag_absent "$tag"
    done

    if [ -n "$version" ]; then
        local current
        current="$(manifest_version)"
        [ -n "$current" ] || die "could not read the version from $MANIFEST"

        if [ "$current" = "$version" ]; then
            info "$MANIFEST is already at $version"
        else
            info "bumping $MANIFEST from $current to $version"
            set_manifest_version "$version"
            [ "$(manifest_version)" = "$version" ] || die "failed to write the version to $MANIFEST"
            # Refresh the workspace entry in Cargo.lock without re-resolving
            # dependencies, so `cargo build --locked` still works.
            cargo update --quiet --workspace
        fi
    fi

    if [ -n "$chart" ]; then
        local current_chart
        current_chart="$(chart_version)"
        [ -n "$current_chart" ] || die "could not read the version from $CHART"

        if [ "$current_chart" = "$chart" ]; then
            info "$CHART is already at $chart"
        else
            info "bumping $CHART from $current_chart to $chart"
            set_chart_version "$chart"
            [ "$(chart_version)" = "$chart" ] || die "failed to write the version to $CHART"
        fi

        # An app release means the chart now targets that image; a chart-only
        # release leaves appVersion alone, pointing at the app it shipped with.
        if [ -n "$version" ]; then
            info "pointing appVersion at $version"
            set_chart_app_version "$version"
        fi

        info "regenerating the chart docs"
        helm-docs --chart-search-root=charts
    fi

    if [ "$skip_tests" = true ]; then
        info "skipping the test suite"
    else
        if [ -n "$version" ]; then
            info "running the test suite"
            cargo test --all --locked
        fi
        if [ -n "$chart" ]; then
            info "linting the chart"
            helm lint charts/oas2mcp -f charts/oas2mcp/values-lint.yaml
        fi
    fi

    local subject
    if [ -n "$version" ] && [ -n "$chart" ]; then
        subject="release v$version (chart $chart)"
    elif [ -n "$version" ]; then
        subject="release v$version"
    else
        subject="release chart $chart"
    fi

    if [ -n "$(git status --porcelain)" ]; then
        info "committing the version bump"
        git add --all -- "$MANIFEST" Cargo.lock charts
        git commit --quiet --message "$subject"
    fi

    for tag in "${tags[@]}"; do
        info "tagging $tag"
        git tag --annotate "$tag" --message "$tag"
    done

    if [ "$push" = false ]; then
        info "not pushing (--no-push); undo with: git tag -d ${tags[*]}"
        return 0
    fi

    if [ "$assume_yes" = false ] && ! confirm "push $BRANCH and ${tags[*]} to origin?"; then
        die "aborted — undo with: git tag -d ${tags[*]} && git reset --hard origin/$BRANCH"
    fi

    info "pushing $BRANCH and ${tags[*]}"
    git push origin "$BRANCH"
    for tag in "${tags[@]}"; do
        git push origin "$tag"
    done

    info "released ${tags[*]} — the workflows take it from here"
}

main "$@"
