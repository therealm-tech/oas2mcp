#!/usr/bin/env bash
# Cut a release: bump the version files, commit, tag, then ask before pushing.
# The `release` workflow publishes from the `vX.Y.Z` tag, the `chart` workflow
# from the `chart-X.Y.Z` tag; the app and the chart version independently.
set -euo pipefail

readonly BRANCH="main"
readonly MANIFEST="Cargo.toml"
readonly CHART="charts/oas2mcp/Chart.yaml"
readonly CHART_INSTALL_DOCS=(
    "README.md"
    "docs/src/content/docs/guides/kubernetes.md"
)

tmpfile=""
trap 'rm -f "$tmpfile"' EXIT

usage() {
    cat <<'EOF'
Usage: scripts/release.sh [options] [<version>]

Cut an oas2mcp release: bump the version files, commit, tag, then ask before
pushing. Give an app version, a chart version, or both.

Arguments:
  <version>              App version, with or without a leading `v`. Bumps
                         Cargo.toml and Cargo.lock, tags `vX.Y.Z`.

Options:
  --chart <version>      Release the Helm chart at this version: bumps
                         `version` in Chart.yaml, regenerates the chart README,
                         pins the `helm install` commands of the docs (stable
                         versions only) and tags `chart-X.Y.Z`.
  --app-version <ver>    With --chart alone, point `appVersion` at this
                         released `vX.Y.Z` instead of the latest stable one.
  --dry-run              Print what would change, then stop.
  -h, --help             Show this help.

Without a terminal, or on any answer but `y`, nothing is pushed and the push
command is printed instead.

Examples:
  scripts/release.sh 1.1.0                         # app only
  scripts/release.sh --chart 1.2.0                 # chart, appVersion = latest vX.Y.Z
  scripts/release.sh 1.1.0 --chart 1.2.0           # both, appVersion = 1.1.0
  scripts/release.sh --chart 1.2.1 --app-version 1.0.2
  scripts/release.sh 1.1.0-rc1 --dry-run
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

require_command() {
    command -v "$1" >/dev/null || die "$1 is not on PATH; run \`mise install\`"
}

# Not `sed -i`: the in-place flag takes an argument on BSD sed and none on GNU
# sed, so there is no portable spelling.
rewrite() {
    local file="$1" script="$2"

    tmpfile="$(mktemp)"
    sed "$script" "$file" >"$tmpfile"
    cat "$tmpfile" >"$file"
    rm -f "$tmpfile"
    tmpfile=""
}

# The `[package]` section only: dependencies further down have a `version` too.
manifest_version() {
    sed -n '/^\[package\]/,/^\[/{s/^version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p;}' "$MANIFEST"
}

chart_version() {
    sed -n 's/^version:[[:space:]]*"\{0,1\}\([^"[:space:]]*\)"\{0,1\}.*/\1/p' "$CHART"
}

chart_app_version() {
    sed -n 's/^appVersion:[[:space:]]*"\{0,1\}\([^"[:space:]]*\)"\{0,1\}.*/\1/p' "$CHART"
}

set_chart_version() {
    rewrite "$CHART" 's/^version:.*/version: '"$1"'/'
}

set_chart_app_version() {
    rewrite "$CHART" 's/^appVersion:.*/appVersion: "'"$1"'"/'
}

set_chart_install_version() {
    local file

    for file in "${CHART_INSTALL_DOCS[@]}"; do
        rewrite "$file" 's|\(oci://ghcr\.io/therealm-tech/charts/oas2mcp --version \)[^[:space:]]*|\1'"$1"'|g'
        grep -qF "oci://ghcr.io/therealm-tech/charts/oas2mcp --version $1" "$file" ||
            die "failed to write the chart version to $file"
    done
}

# A loop, not `grep | head`: under `pipefail` a grep matching nothing fails
# the substitution, and `set -e` kills the script without a message.
latest_app_version() {
    local tag

    while IFS= read -r tag; do
        # appVersion must name a stable image; rc tags are skipped.
        if printf '%s' "$tag" | grep -Eq '^v[0-9]+\.[0-9]+\.[0-9]+$'; then
            printf '%s' "${tag#v}"
            return 0
        fi
    done <<<"$(git tag --list 'v*' --sort=-v:refname)"
}

main() {
    local version="" chart="" app_version="" dry_run=false

    while [ $# -gt 0 ]; do
        case "$1" in
            --chart | --app-version)
                [ $# -ge 2 ] || die "$1 needs a version"
                if [ "$1" = --chart ]; then chart="$2"; else app_version="$2"; fi
                shift
                ;;
            --chart=*) chart="${1#--chart=}" ;;
            --app-version=*) app_version="${1#--app-version=}" ;;
            --dry-run) dry_run=true ;;
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
    if [ -n "$app_version" ]; then
        [ -n "$chart" ] || die "--app-version only applies with --chart"
        [ -z "$version" ] || die "--app-version conflicts with <version>, which appVersion follows"
    fi

    local tags=()
    if [ -n "$version" ]; then
        version="${version#v}"
        require_semver "$version"
        tags+=("v$version")
    fi
    if [ -n "$chart" ]; then
        chart="${chart#chart-}"
        chart="${chart#v}"
        require_semver "$chart"
        tags+=("chart-$chart")
    fi
    if [ -n "$app_version" ]; then
        app_version="${app_version#v}"
        require_semver "$app_version"
    fi

    cd "$(git rev-parse --show-toplevel)"

    if [ -n "$version" ]; then
        require_command cargo
        require_command cargo-set-version
    fi
    if [ -n "$chart" ]; then
        require_command helm-docs
    fi

    # A dirty tree would smuggle unrelated changes into the release commit.
    [ -z "$(git status --porcelain)" ] || die "the working tree is not clean"

    local current_branch
    current_branch="$(git rev-parse --abbrev-ref HEAD)"
    [ "$current_branch" = "$BRANCH" ] || die "not on $BRANCH (on $current_branch)"

    info "fetching origin"
    git fetch --quiet --tags origin "$BRANCH"
    [ "$(git rev-list --count "HEAD..origin/$BRANCH")" -eq 0 ] ||
        die "$BRANCH is behind origin/$BRANCH; pull first"
    [ "$(git rev-list --count "origin/$BRANCH..HEAD")" -eq 0 ] ||
        die "$BRANCH has commits origin/$BRANCH lacks; land them through a pull request first"

    local tag
    for tag in "${tags[@]}"; do
        require_tag_absent "$tag"
    done

    local app_target=""
    if [ -n "$chart" ]; then
        if [ -n "$version" ]; then
            app_target="$version"
        elif [ -n "$app_version" ]; then
            git rev-parse -q --verify "refs/tags/v$app_version" >/dev/null ||
                die "--app-version $app_version: no v$app_version tag; release the app first"
            app_target="$app_version"
        else
            app_target="$(latest_app_version)"
            [ -n "$app_target" ] ||
                die "no vX.Y.Z tag to take appVersion from; release the app first"
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

    local current_version="" current_chart="" current_app=""
    if [ -n "$version" ]; then
        current_version="$(manifest_version)"
        [ -n "$current_version" ] || die "could not read the version from $MANIFEST"
    fi
    if [ -n "$chart" ]; then
        current_chart="$(chart_version)"
        current_app="$(chart_app_version)"
        [ -n "$current_chart" ] || die "could not read the version from $CHART"
    fi

    if [ "$dry_run" = true ]; then
        [ -z "$version" ] || info "$MANIFEST: $current_version -> $version"
        if [ -n "$chart" ]; then
            info "$CHART: version $current_chart -> $chart, appVersion $current_app -> $app_target"
            if [[ "$chart" != *-* ]]; then
                info "helm install commands: --version $chart"
            fi
        fi
        info "commit: $subject"
        info "tags: ${tags[*]}"
        info "dry run: nothing changed"
        return 0
    fi

    if [ -n "$version" ] && [ "$current_version" != "$version" ]; then
        info "bumping $MANIFEST from $current_version to $version"
        cargo set-version --package oas2mcp "$version"
        [ "$(manifest_version)" = "$version" ] || die "failed to write the version to $MANIFEST"
    fi

    if [ -n "$chart" ]; then
        if [ "$current_chart" != "$chart" ]; then
            info "bumping $CHART from $current_chart to $chart"
            set_chart_version "$chart"
            [ "$(chart_version)" = "$chart" ] || die "failed to write the version to $CHART"
        fi

        if [ "$current_app" != "$app_target" ]; then
            info "pointing appVersion at $app_target"
            set_chart_app_version "$app_target"
            [ "$(chart_app_version)" = "$app_target" ] || die "failed to write appVersion to $CHART"
        fi

        # An rc must not become what the docs tell people to install.
        if [[ "$chart" != *-* ]]; then
            info "pinning the install commands to chart $chart"
            set_chart_install_version "$chart"
        fi

        info "regenerating the chart README"
        helm-docs --chart-search-root=charts
    fi

    if [ -n "$(git status --porcelain)" ]; then
        info "committing: $subject"
        git add --all -- "$MANIFEST" Cargo.lock charts "${CHART_INSTALL_DOCS[@]}"
        git commit --quiet --message "$subject"
    fi

    for tag in "${tags[@]}"; do
        info "tagging $tag"
        git tag --annotate "$tag" --message "$tag"
    done

    local push=(git push --atomic origin "$BRANCH" "${tags[@]}")
    local reply=""
    if [ -t 0 ]; then
        printf 'push %s and %s to origin? this publishes the release. [y/N] ' "$BRANCH" "${tags[*]}"
        read -r reply || reply=""
    else
        info "no terminal to ask on"
    fi

    case "$reply" in
        y | Y | yes | YES) ;;
        *)
            info "not pushed; the commit and tags stay local. Publish with:"
            printf '  %s\n' "${push[*]}"
            return 0
            ;;
    esac

    info "pushing $BRANCH and ${tags[*]}"
    "${push[@]}"
    info "pushed ${tags[*]}; the release and chart workflows publish from the tags"
}

main "$@"
