#!/usr/bin/env bash
# Cut an application release: align `Cargo.toml` with the requested version,
# commit, tag `vX.Y.Z` and push. The `release` workflow takes it from there
# (image build + GitHub Release). The Helm chart is released separately with a
# `chart-X.Y.Z` tag.
set -euo pipefail

readonly BRANCH="main"
readonly MANIFEST="Cargo.toml"

tmpfile=""
trap 'rm -f "$tmpfile"' EXIT

usage() {
    cat <<'EOF'
Usage: scripts/release.sh [options] <version>

Cut an oas2mcp application release.

Arguments:
  <version>       SemVer version to release, with or without a leading `v`
                  (e.g. `0.4.0` or `v0.4.0`).

Options:
  --skip-tests    Skip `cargo test` before tagging.
  --no-push       Commit and tag locally, but push nothing.
  -y, --yes       Do not ask for confirmation before pushing.
  -h, --help      Show this help.
EOF
}

info() {
    printf '\033[1;34m==>\033[0m %s\n' "$*"
}

die() {
    printf '\033[1;31merror:\033[0m %s\n' "$*" >&2
    exit 1
}

# Read the `version` field of the `[package]` section, and nothing else: a
# dependency further down the file also has a `version` key.
manifest_version() {
    sed -n '/^\[package\]/,/^\[/{s/^version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p;}' "$MANIFEST"
}

set_manifest_version() {
    local version="$1"

    # Not `sed -i`: the in-place flag takes an argument on BSD sed and none on
    # GNU sed, so there is no portable spelling of it.
    tmpfile="$(mktemp)"
    sed '/^\[package\]/,/^\[/ s/^version[[:space:]]*=.*/version = "'"$version"'"/' "$MANIFEST" >"$tmpfile"
    cat "$tmpfile" >"$MANIFEST"
    rm -f "$tmpfile"
    tmpfile=""
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
    local version="" skip_tests=false push=true assume_yes=false

    while [ $# -gt 0 ]; do
        case "$1" in
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

    if [ -z "$version" ]; then
        usage >&2
        die "missing <version>"
    fi

    version="${version#v}"
    if ! printf '%s' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$'; then
        die "not a SemVer version: $version"
    fi
    local tag="v$version"

    cd "$(git rev-parse --show-toplevel)"

    command -v cargo >/dev/null || die "cargo is not on PATH"
    [ -f "$MANIFEST" ] || die "$MANIFEST not found"

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

    if git rev-parse -q --verify "refs/tags/$tag" >/dev/null; then
        die "tag $tag already exists locally"
    fi
    if git ls-remote --exit-code --tags origin "refs/tags/$tag" >/dev/null 2>&1; then
        die "tag $tag already exists on origin"
    fi

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

    if [ "$skip_tests" = true ]; then
        info "skipping the test suite"
    else
        info "running the test suite"
        cargo test --all --locked
    fi

    if [ -n "$(git status --porcelain)" ]; then
        info "committing the version bump"
        git add "$MANIFEST" Cargo.lock
        git commit --quiet --message "release $tag"
    fi

    info "tagging $tag"
    git tag --annotate "$tag" --message "$tag"

    if [ "$push" = false ]; then
        info "not pushing (--no-push); undo with: git tag -d $tag"
        return 0
    fi

    if [ "$assume_yes" = false ] && ! confirm "push $BRANCH and $tag to origin?"; then
        die "aborted — undo with: git tag -d $tag && git reset --hard origin/$BRANCH"
    fi

    info "pushing $BRANCH and $tag"
    git push origin "$BRANCH"
    git push origin "$tag"

    info "released $tag — the release workflow builds the image and drafts the notes"
}

main "$@"
