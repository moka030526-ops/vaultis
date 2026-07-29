#!/usr/bin/env bash
#
# Cut a vaultis release. The binaries are built and published by CI, not here.
#
#   scripts/release.sh [VERSION] [--dry-run] [--yes] [--no-watch]
#                      [--allow-prerelease]
#
# A release IS a tag push: .github/workflows/release.yml fires on `v*`, builds
# vaultis.exe and vaultis-gui.exe on a windows-latest runner, zips them with the two
# icons and make-shortcuts.ps1, and publishes that zip plus its .sha256 as a GitHub
# Release. get_vaultis.bat then installs whatever /releases/latest points at — so a
# bad tag is not a local mistake, it is the thing every user downloads next.
#
# Which is what this script is for. Pushing the tag is one command; the checks that
# have to pass FIRST are the part worth automating:
#
#   * `gh` is installed and logged in (the tag push alone would work, but then
#     nothing here could tell you whether the build that followed it succeeded).
#   * The working tree is clean, and HEAD is exactly what `origin` already has.
#     Tagging an unpushed commit produces a release built from code nobody can see.
#   * The tag is new — not already local, on the remote, or an existing release.
#   * The tag matches the `vaultis` crate version in Cargo.toml. These drift silently
#     otherwise: nothing in the build reads the tag, so a v0.2.0 release whose crate
#     still says 0.1.0 compiles and ships perfectly happily.
#   * CHANGELOG.md has a section for the version, rather than the changes still
#     sitting under `## [Unreleased]`.
#
# Then it asks — defaulting to NO — before pushing, and follows the run to the end.
#
#   VERSION             the tag to cut, `v0.2.0` or `0.2.0`. Defaults to the version
#                       in crates/vaultis-desktop/Cargo.toml, which is the release's
#                       source of truth for it.
#   --dry-run           run the SAME workflow through workflow_dispatch instead: it
#                       builds and uploads the zip as a run artifact and publishes
#                       nothing. No tag is created. Use this to check the packaging
#                       before committing to a version number.
#   --yes               skip the confirmation prompt, for an unattended run.
#   --no-watch          push the tag and exit, instead of waiting for the build.
#   --allow-prerelease  permit a `v1.0.0-rc1`-style tag; see the check for why it is
#                       refused by default.
#
# Nothing here is undoable by itself. To retract a published release you have to
# delete the release AND both copies of the tag, and anyone who ran get_vaultis.bat
# in the meantime already has the build:
#
#   gh release delete vX.Y.Z --yes
#   git push origin :refs/tags/vX.Y.Z
#   git tag -d vX.Y.Z
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

# The crate that produces both shipped binaries — `cargo build -p vaultis --bins` in
# release.yml — so its version is the one a tag has to agree with.
VERSION_CRATE="crates/vaultis-desktop/Cargo.toml"
WORKFLOW="release.yml"
RELEASE_BRANCH="main"

version=""
dry_run=0
assume_yes=0
watch=1
allow_prerelease=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        --dry-run)
            dry_run=1
            shift
            ;;
        --yes|-y)
            assume_yes=1
            shift
            ;;
        --no-watch)
            watch=0
            shift
            ;;
        --allow-prerelease)
            allow_prerelease=1
            shift
            ;;
        -h|--help)
            # Print this file's leading comment block (everything after the shebang up
            # to the first non-comment line) as the help text, so there is one
            # description of this script rather than two that drift apart.
            awk 'NR == 1 { next } /^#/ { sub(/^# ?/, ""); print; next } { exit }' "${BASH_SOURCE[0]}"
            exit 0
            ;;
        -*)
            echo "error: unknown option '$1' (see --help)" >&2
            exit 2
            ;;
        *)
            [[ -z "$version" ]] || { echo "error: VERSION given twice ('$version', '$1')" >&2; exit 2; }
            version="$1"
            shift
            ;;
    esac
done

die() {
    echo "error: $1" >&2
    shift
    for line in "$@"; do
        echo "       $line" >&2
    done
    exit 1
}

# Ask, defaulting to NO. On a machine with no terminal to ask at (CI, a pipe) there is
# no safe assumption to make about publishing, so it stops and says how to proceed.
confirm() {
    local prompt="$1"
    if [[ "$assume_yes" == 1 ]]; then
        return 0
    fi
    if [[ ! -t 0 ]]; then
        die "no terminal to confirm at, and this publishes to the world." \
            "Re-run with --yes if that is what you want."
    fi
    local reply=""
    read -r -p "$prompt [y/N] " reply
    [[ "$reply" == "y" || "$reply" == "Y" || "$reply" == "yes" ]]
}

# --- gh -----------------------------------------------------------------------
#
# Needed for --dry-run (which has no other trigger) and for watching the build. The
# tag push itself is plain git, but a release you cannot see the result of is worse
# than one you waited thirty seconds for.
command -v gh >/dev/null 2>&1 ||
    die "the GitHub CLI (gh) is not installed." \
        "https://cli.github.com — or on Debian/Ubuntu: sudo apt install gh"
gh auth status >/dev/null 2>&1 ||
    die "gh is installed but not logged in. Run:  gh auth login"

# --- The tree has to be exactly what origin has -------------------------------
#
# Two separate mistakes, both of which produce a release built from something other
# than what was reviewed: uncommitted work that the tag silently excludes, and
# committed-but-unpushed work that the runner cannot check out at all.
git rev-parse --git-dir >/dev/null 2>&1 || die "not inside a git repository."

if [[ -n "$(git status --porcelain)" ]]; then
    git status --short >&2
    die "the working tree is not clean." \
        "A tag points at a commit, so none of the above would be in the release."
fi

branch="$(git rev-parse --abbrev-ref HEAD)"
if [[ "$branch" != "$RELEASE_BRANCH" ]]; then
    echo "warning: on branch '$branch', not '$RELEASE_BRANCH'." >&2
    confirm "Release from '$branch' anyway?" || die "stopped."
fi

echo "==> Fetching origin"
git fetch --quiet --tags origin

git rev-parse --verify --quiet "origin/$branch" >/dev/null ||
    die "origin has no '$branch' branch to compare against." \
        "Push it first:  git push -u origin $branch"

local_head="$(git rev-parse HEAD)"
remote_head="$(git rev-parse "origin/$branch")"
merge_base="$(git merge-base HEAD "origin/$branch")"

if [[ "$local_head" != "$remote_head" ]]; then
    if [[ "$local_head" == "$merge_base" ]]; then
        die "'$branch' is behind origin/$branch." \
            "Pull first:  git pull --rebase origin $branch"
    elif [[ "$remote_head" == "$merge_base" ]]; then
        git log --oneline "origin/$branch..HEAD" >&2
        die "the commits above are not pushed." \
            "CI builds from origin, so it cannot see them:  git push origin $branch"
    else
        die "'$branch' and origin/$branch have diverged." \
            "Reconcile them before tagging:  git pull --rebase origin $branch"
    fi
fi

# --- Version ------------------------------------------------------------------
#
# Read from the [package] section specifically: a `version = ` under [dependencies]
# is some other crate's, and there are plenty of those below it.
crate_version="$(
    awk '/^\[package\]/ { in_pkg = 1; next }
         /^\[/          { in_pkg = 0 }
         in_pkg && /^version[[:space:]]*=/ {
             gsub(/[",]/, "", $3); print $3; exit
         }' "$VERSION_CRATE"
)"
[[ -n "$crate_version" ]] || die "could not read a version out of $VERSION_CRATE."

if [[ -z "$version" ]]; then
    version="v$crate_version"
    echo "==> No VERSION given; using $version from $VERSION_CRATE"
fi

# Accept `0.2.0` as well as `v0.2.0` — the tag is what the workflow matches, and it
# wants the v. Normalizing beats rejecting a form that obviously means the same thing.
tag="$version"
[[ "$tag" == v* ]] || tag="v$tag"

[[ "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$ ]] ||
    die "'$tag' is not a version tag." \
        "Expected vMAJOR.MINOR.PATCH, e.g. v0.2.0."

# Compared without any prerelease/build suffix: v0.2.0-rc1 IS a prerelease of 0.2.0,
# so the crate version it has to agree with is 0.2.0. (Whether such a tag may be
# pushed at all is a separate question, decided below.)
tag_core="${tag#v}"
tag_core="${tag_core%%[-+]*}"

if [[ "$dry_run" == 0 && "$tag_core" != "$crate_version" ]]; then
    die "tag $tag does not match the crate version $crate_version." \
        "Nothing in the build reads the tag, so these drift apart silently." \
        "Bump it in $VERSION_CRATE (and its sibling crates), commit, push, re-run."
fi

# The other workspace crates are versioned in lockstep by hand. A mismatch is not
# fatal — nothing in the release reads them — but it is always a missed edit.
for other in crates/*/Cargo.toml; do
    [[ "$other" == "$VERSION_CRATE" ]] && continue
    other_version="$(
        awk '/^\[package\]/ { in_pkg = 1; next }
             /^\[/          { in_pkg = 0 }
             in_pkg && /^version[[:space:]]*=/ {
                 gsub(/[",]/, "", $3); print $3; exit
             }' "$other"
    )"
    if [[ -n "$other_version" && "$other_version" != "$crate_version" ]]; then
        echo "warning: $other is at $other_version, not $crate_version." >&2
    fi
done

# `fuzz/` is a DETACHED workspace with its own lockfile, so a version bump never reaches
# it and `cargo fuzz build` rewrites it on the next run. That matters here and nowhere
# else: the sequence this project follows is audit (which fuzzes) -> fix -> release, so a
# stale entry means the clean-tree check below trips on a one-line diff nobody made, in
# the middle of cutting a release. Warn while it is still cheap to commit.
# (audit 2026-07-29 round 2, L-3.)
fuzz_core_version="$(awk '/^name = "vaultis-core"$/ { found = 1; next }
                          found && /^version = / { gsub(/"/, "", $3); print $3; exit }' fuzz/Cargo.lock 2>/dev/null)"
if [[ -n "$fuzz_core_version" && "$fuzz_core_version" != "$crate_version" ]]; then
    echo "warning: fuzz/Cargo.lock still records vaultis-core $fuzz_core_version, not $crate_version." >&2
    echo "         Run \`cargo +nightly fuzz build\` and commit it, or the next fuzz run dirties the tree." >&2
fi

# A prerelease tag is NOT a quiet one. release.yml calls `gh release create` without
# --prerelease, and gh does not infer it from the tag name, so v1.0.0-rc1 publishes as
# a normal release — which makes it /releases/latest, which is exactly what
# get_vaultis.bat hands to every user who double-clicks it. Use --dry-run for a
# release candidate, or add --prerelease to the workflow first.
if [[ "$allow_prerelease" == 0 && "$tag" == *-* ]]; then
    die "'$tag' looks like a prerelease, and it would not behave like one." \
        "release.yml publishes without --prerelease, so this tag would become" \
        "/releases/latest and get_vaultis.bat would install it for everyone." \
        "Use --dry-run to test the packaging, or --allow-prerelease if you mean it."
fi

# --- The tag has to be new ----------------------------------------------------
#
# All three checked separately: a local tag, a remote tag, and a published release can
# each exist without the others, and each fails the push at a different point.
if [[ "$dry_run" == 0 ]]; then
    if git rev-parse --verify --quiet "refs/tags/$tag" >/dev/null; then
        die "tag $tag already exists locally." \
            "Delete it if it was never pushed:  git tag -d $tag"
    fi
    if [[ -n "$(git ls-remote --tags origin "refs/tags/$tag")" ]]; then
        die "tag $tag is already on origin." \
            "Retracting it takes both:  gh release delete $tag --yes" \
            "                           git push origin :refs/tags/$tag"
    fi
    if gh release view "$tag" >/dev/null 2>&1; then
        die "a GitHub release for $tag already exists." \
            "Re-pushing the tag would fail at the workflow's release step." \
            "Delete it first:  gh release delete $tag --yes"
    fi
fi

# --- Changelog ----------------------------------------------------------------
#
# A warning, not a wall: the release is still perfectly valid without it, and the
# notes can be edited afterwards with `gh release edit`. But the section is trivial to
# forget, and CHANGELOG.md says so itself under [Unreleased].
if [[ "$dry_run" == 0 ]] && [[ -f CHANGELOG.md ]]; then
    if ! grep -qE "^## \[v?${crate_version//./\\.}\]" CHANGELOG.md; then
        echo "warning: CHANGELOG.md has no '## [$crate_version]' section." >&2
        if grep -q '^## \[Unreleased\]' CHANGELOG.md; then
            echo "         The changes are presumably still under [Unreleased]." >&2
        fi
        confirm "Release $tag without a changelog entry?" || die "stopped."
    fi
fi

# --- Find the run this script just started ------------------------------------
#
# GitHub takes a few seconds to register a run after the event, so a single lookup
# usually finds nothing. For a tag push the run's headBranch is the tag name.
wait_for_run() {
    local ref="$1" event="$2" run_id="" i
    for ((i = 0; i < 30; i++)); do
        run_id="$(
            gh run list --workflow "$WORKFLOW" --branch "$ref" --event "$event" \
                --limit 1 --json databaseId --jq '.[0].databaseId // empty' 2>/dev/null || true
        )"
        [[ -n "$run_id" ]] && { echo "$run_id"; return 0; }
        sleep 2
    done
    return 1
}

# `gh run watch --exit-status` returns nonzero if the run failed, so a failed build
# fails this script too rather than printing a green summary over a red run.
follow() {
    local ref="$1" event="$2" run_id=""
    if ! run_id="$(wait_for_run "$ref" "$event")"; then
        echo "warning: the run has not appeared yet. Check it with:" >&2
        echo "         gh run list --workflow $WORKFLOW" >&2
        return 0
    fi
    echo "==> Watching run $run_id"
    gh run watch "$run_id" --exit-status
}

# --- Dry run: build the package, publish nothing ------------------------------
if [[ "$dry_run" == 1 ]]; then
    echo "==> Dispatching $WORKFLOW on '$branch' (no tag, no release)"
    gh workflow run "$WORKFLOW" --ref "$branch"
    if [[ "$watch" == 1 ]]; then
        follow "$branch" workflow_dispatch
        echo
        echo "The zip and its .sha256 are attached to that run as an artifact:"
        echo "  gh run download \$(gh run list --workflow $WORKFLOW --event workflow_dispatch --limit 1 --json databaseId --jq '.[0].databaseId')"
    else
        echo "==> Dispatched. Follow it with:  gh run watch"
    fi
    exit 0
fi

# --- Cut it ------------------------------------------------------------------
previous_tag="$(git tag --list 'v*' --sort=-version:refname | head -n 1)"

echo
echo "  Repository   $(gh repo view --json nameWithOwner --jq .nameWithOwner 2>/dev/null || echo '?')"
echo "  Tag          $tag"
echo "  Commit       $(git log -1 --format='%h  %s')"
if [[ -n "$previous_tag" ]]; then
    echo "  Previous     $previous_tag  ($(git rev-list --count "$previous_tag..HEAD") commits since)"
else
    # --generate-notes has nothing to diff against on the first release, so the notes
    # come out as the entire history. Editable afterwards; surprising if unmentioned.
    echo "  Previous     none — first release, so the generated notes are ALL commits"
fi
echo
echo "  This publishes a public GitHub Release. get_vaultis.bat installs"
echo "  /releases/latest, so it becomes what new users download."
echo

confirm "Push $tag and publish?" || die "stopped. Nothing was tagged."

# Annotated rather than lightweight: a release tag is an object worth having an author
# and a date of its own, and `git describe` prefers them.
git tag -a "$tag" -m "vaultis $tag"
echo "==> Pushing $tag"
if ! git push origin "$tag"; then
    git tag -d "$tag"
    die "the push failed; the local tag has been removed so you can retry cleanly."
fi

if [[ "$watch" == 1 ]]; then
    follow "$tag" push
    echo
    echo "==> Released: $(gh release view "$tag" --json url --jq .url 2>/dev/null || echo "$tag")"
else
    echo "==> Pushed. Follow the build with:  gh run watch"
fi
