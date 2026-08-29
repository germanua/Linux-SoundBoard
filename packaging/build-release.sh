#!/usr/bin/env bash
#
# One command from "the code is ready" to "every release artifact exists, is
# consistent, and is verified".
#
# Two halves. First an interactive menu that collects the release information
# and writes it into every file that carries a version, so nothing is edited by
# hand. Then an ordered, fail-fast build that produces the artifacts, verifies
# them, and tags the release.
#
# Usage:
#   packaging/build-release.sh                 interactive
#   packaging/build-release.sh --bump minor --yes
#   packaging/build-release.sh --finish-aur --tag v2.3.0
#   packaging/build-release.sh --help
#
# Two things are deliberately never automatic: pushing to the remote (armed only
# by --push, and still confirmed), and uploading the release (the exact
# `gh release create` command is printed, never run).

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
# Resolved relative to this script at runtime.
# shellcheck disable=SC1091
source "$SCRIPT_DIR/common.sh"
# Resolved relative to this script at runtime.
# shellcheck disable=SC1091
source "$SCRIPT_DIR/linux/app-meta.sh"

REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"
MANIFEST_PATH="$REPO_ROOT/src/Cargo.toml"
DIST_ROOT="$REPO_ROOT/dist"

# The single failure smoke-check.sh reports on an unmodified checkout: systemd
# resolves a bare ExecStart against its own compiled-in path, not $PATH, so the
# unit cannot validate unless the binary is installed system-wide. Downgraded to
# a warning only after proving it is environmental — see smoke_failure_is_known.
SMOKE_KNOWN_FAIL='systemd-analyze verify: engine service and target'
SYSTEMD_DEFAULT_PATH=(/usr/local/sbin /usr/local/bin /usr/sbin /usr/bin)

ARCHIVE_URL_BASE="$APP_URL/archive/refs/tags"
AUR_DIR="$REPO_ROOT/packaging/aur"
CHANGELOG="$REPO_ROOT/docs/CHANGELOG.md"
DEBIAN_CHANGELOG="$REPO_ROOT/packaging/debian/changelog"
SPEC="$REPO_ROOT/packaging/rpm/linux-soundboard.spec"
METAINFO="$REPO_ROOT/packaging/flatpak/$APP_ID.metainfo.xml"
LEGACY_DEB_CONTROL="$REPO_ROOT/packaging/deb/control"

# ── Options, with their non-interactive twins ───────────────────────────────

OPT_VERSION="${LSB_RELEASE_VERSION:-}"
OPT_BUMP="${LSB_RELEASE_BUMP:-}"
OPT_DATE="${LSB_RELEASE_DATE:-}"
OPT_PKGREL="${LSB_RELEASE_PKGREL:-}"
OPT_NOTES_FILE="${LSB_RELEASE_NOTES_FILE:-}"
OPT_SUMMARIES_FILE="${LSB_RELEASE_SUMMARIES_FILE:-}"
OPT_SUMMARIES_AUTO="${LSB_RELEASE_SUMMARIES_AUTO:-0}"
OPT_SUMMARY_MAX="${LSB_RELEASE_SUMMARY_MAX:-5}"
OPT_MAINTAINER="${LSB_RELEASE_MAINTAINER:-}"
OPT_ONLY="${LSB_RELEASE_ONLY:-}"
OPT_SKIP="${LSB_RELEASE_SKIP:-}"
OPT_FLATPAK="${LSB_RELEASE_FLATPAK:-0}"
OPT_PARTIAL_OK="${LSB_RELEASE_PARTIAL_OK:-0}"
OPT_NO_COMMIT="${LSB_RELEASE_NO_COMMIT:-0}"
OPT_NO_TAG="${LSB_RELEASE_NO_TAG:-0}"
OPT_RETAG=0
OPT_FORCE_TAG=0
OPT_PUSH="${LSB_RELEASE_PUSH:-0}"
OPT_BRANCH="${LSB_RELEASE_BRANCH:-main}"
OPT_ALLOW_DIRTY="${LSB_RELEASE_ALLOW_DIRTY:-0}"
OPT_TAG_DIRTY=0
OPT_UNDO=0
OPT_DRY_RUN="${LSB_RELEASE_DRY_RUN:-0}"
OPT_YES="${LSB_RELEASE_ASSUME_YES:-0}"
OPT_NO_REVIEW=0
OPT_SKIP_TESTS="${LSB_RELEASE_SKIP_TESTS:-0}"
OPT_SMOKE_FIX_SYSTEMD=0
OPT_KEEP_DIST="${LSB_RELEASE_KEEP_DIST:-0}"
OPT_CLEAN_DIST="${LSB_RELEASE_CLEAN_DIST:-0}"
SIGNING_KEY="${LSB_RELEASE_SIGNING_KEY:-}"
OPT_FINISH_AUR=0
OPT_TAG_NAME=""
OPT_PRUNE_LEGACY=0
OPT_FAKE_CONTAINERS="${LSB_RELEASE_FAKE_CONTAINERS:-0}"
OPT_SELF_TEST=0

# ── Run state ───────────────────────────────────────────────────────────────

OLD_VERSION=""
OLD_PKGREL=""
NEW_VERSION=""
PKGREL=""
RELEASE_DATE=""
DATE_ISO=""
DATE_RPM=""
DATE_RFC=""
MAINTAINER=""
TAG_NAME=""
SUMMARY_FILE=""

BUMP_IN_PROGRESS=0
BUMP_ALREADY_APPLIED=0
BUMPED_FILES=()
TARGETS=()
SKIPPED=()
BUILT=()
FAKED=()
EXIT_STATUS=0
WORK_DIR=""
SELF_TEST_DIR=""
CLEANUP_SYSTEMD_STUB=0
NARROWED=0
DIRTY_AT_START=()

# ── Output ──────────────────────────────────────────────────────────────────

heading() { printf '\n==> %s\n' "$1"; }
info()    { printf '    %s\n' "$1"; }
pass()    { printf '[PASS] %s\n' "$1"; }
warn()    { printf '[WARN] %s\n' "$1" >&2; }
skip()    { printf '[SKIP] %s\n' "$1"; }
note()    { printf '[NOTE] %s\n' "$1"; }

fail() {
    printf 'build-release: %s\n' "$1" >&2
    exit 1
}

rule() { printf '%s\n' '──────────────────────────────────────────────────────────────────────'; }

# ── Prompting ───────────────────────────────────────────────────────────────

# Every prompt has a flag or env twin, so a fully specified run never reaches
# these. Reading from /dev/tty rather than stdin keeps prompts working when the
# script is piped.
ask() {
    local prompt="$1" default="$2" reply=""

    if [[ "$OPT_YES" -eq 1 ]]; then
        printf '%s' "$default"
        return 0
    fi
    if [[ ! -t 0 && ! -r /dev/tty ]]; then
        fail "no terminal for '$prompt'; pass the matching flag or --yes"
    fi

    printf '%s' "$prompt" >&2
    if [[ -r /dev/tty ]]; then
        IFS= read -r reply </dev/tty || { printf '\n' >&2; abort_no_writes; }
    else
        IFS= read -r reply || { printf '\n' >&2; abort_no_writes; }
    fi
    printf '%s' "${reply:-$default}"
}

# --yes means proceed, not "take the literal N". The guard against an unattended
# push is that pushing needs --push as well, never --yes alone.
confirm() {
    local prompt="$1" reply

    if [[ "$OPT_YES" -eq 1 ]]; then
        printf '%s [y/N]: y\n' "$prompt" >&2
        return 0
    fi

    reply="$(ask "$prompt [y/N]: " "n")"
    [[ "$reply" =~ ^[Yy]([Ee][Ss])?$ ]]
}

abort_no_writes() {
    printf '\nAborted. Nothing was written.\n' >&2
    exit 3
}

# ── Small utilities ─────────────────────────────────────────────────────────

git_repo() { git -C "$REPO_ROOT" "$@"; }

tree_is_clean() { [[ -z "$(git_repo status --porcelain)" ]]; }

semver_valid() { [[ "$1" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; }

semver_bump() {
    local version="$1" kind="$2" major minor patch
    IFS=. read -r major minor patch <<<"$version"
    case "$kind" in
        major) printf '%d.0.0\n' "$((major + 1))" ;;
        minor) printf '%d.%d.0\n' "$major" "$((minor + 1))" ;;
        patch) printf '%d.%d.%d\n' "$major" "$minor" "$((patch + 1))" ;;
        *)     return 1 ;;
    esac
}

list_has() {
    local needle="$1"; shift
    local item
    for item in "$@"; do
        [[ "$item" == "$needle" ]] && return 0
    done
    return 1
}

# Writes go through a temp file in the same directory so a failed rewrite can
# never leave a half-written metadata file behind.
replace_file() {
    local target="$1" source="$2"
    cat "$source" >"$target"
    rm -f "$source"
}

# Rewrites are staged outside the repo so that a failure between writing the new
# content and swapping it in cannot leave a stray file in the working tree,
# which the tag and the dirty-tree check both depend on being clean.
bump_tmp() {
    printf '%s/%s.bump' "$WORK_DIR" "${1##*/}"
}

# A pattern that is absent is an answer, not an error: grep exits 1 and would
# abort the pipeline under `set -o pipefail` without the guard.
count_occurrences() {
    local file="$1" pattern="$2"
    { grep -oF -- "$pattern" "$file" 2>/dev/null || true; } | wc -l | tr -d ' '
}

# ── Rollback ────────────────────────────────────────────────────────────────

# Restoring the exact path list is exact rather than best-effort. `git reset
# --hard` would reach past what this script touched, which is why it is not used.
# Under --allow-dirty the tree can hold edits this script did not make, and a
# restore would delete them, so those paths are reported instead of restored.
rollback_bump() {
    [[ "$BUMP_IN_PROGRESS" -eq 1 ]] || return 0
    [[ "${#BUMPED_FILES[@]}" -gt 0 ]] || return 0

    BUMP_IN_PROGRESS=0

    local path
    local -a restorable=() preserved=()
    for path in "${BUMPED_FILES[@]}"; do
        if list_has "$path" "${DIRTY_AT_START[@]+"${DIRTY_AT_START[@]}"}"; then
            preserved+=("$path")
        else
            restorable+=("$path")
        fi
    done

    if [[ "${#preserved[@]}" -gt 0 ]]; then
        warn "not restoring ${#preserved[@]} file(s) that already had uncommitted changes before this run:"
        for path in "${preserved[@]}"; do
            warn "  $path - carries both your edits and a partial bump; resolve it by hand"
        done
    fi

    [[ "${#restorable[@]}" -gt 0 ]] || return 0
    warn "rolling back the partial bump (${#restorable[@]} file(s))"
    git_repo restore --source=HEAD --staged --worktree -- "${restorable[@]}" \
        || warn "rollback failed; inspect 'git status' before retrying"
}

# bash keeps only one EXIT trap, so every teardown runs from here and is guarded
# by its own flag. Installed once, in main.
# Reached through the EXIT trap rather than a call site.
# shellcheck disable=SC2317
on_exit() {
    local status=$?
    rollback_bump
    [[ "$CLEANUP_SYSTEMD_STUB" -eq 1 ]] && remove_systemd_stub
    [[ -n "$WORK_DIR" && -d "$WORK_DIR" ]] && rm -rf "$WORK_DIR"
    [[ -n "$SELF_TEST_DIR" && -d "$SELF_TEST_DIR" ]] && rm -rf "$SELF_TEST_DIR"
    return "$status"
}

track_bumped() {
    local path
    for path in "$@"; do
        list_has "$path" "${BUMPED_FILES[@]}" || BUMPED_FILES+=("$path")
    done
}

# ═══════════════════════════════════════════════════════════════════════════
# Phase 0 — preflight
# ═══════════════════════════════════════════════════════════════════════════

detect_container_runtime() {
    # Both docker wrappers hardcode `docker` and require rsync on the host;
    # podman is not a drop-in for them.
    command -v docker >/dev/null 2>&1 && command -v rsync >/dev/null 2>&1
}

target_is_available() {
    case "$1" in
        tarball)      command -v cargo >/dev/null 2>&1 ;;
        debappimage)  [[ "$OPT_FAKE_CONTAINERS" -eq 1 ]] || detect_container_runtime ;;
        rpm)          [[ "$OPT_FAKE_CONTAINERS" -eq 1 ]] || detect_container_runtime ;;
        flatpak)      command -v flatpak-builder >/dev/null 2>&1 ;;
        *)            return 1 ;;
    esac
}

target_requirement() {
    case "$1" in
        tarball)      printf 'host cargo' ;;
        debappimage)  printf 'docker + rsync (ubuntu:24.04)' ;;
        rpm)          printf 'docker + rsync (fedora:latest)' ;;
        flatpak)      printf 'flatpak-builder' ;;
    esac
}

target_label() {
    case "$1" in
        tarball)      printf 'tarball' ;;
        debappimage)  printf 'deb + AppImage' ;;
        rpm)          printf 'rpm' ;;
        flatpak)      printf 'flatpak' ;;
    esac
}

resolve_targets() {
    local requested=(tarball debappimage rpm)
    [[ "$OPT_FLATPAK" -eq 1 ]] && requested+=(flatpak)

    if [[ -n "$OPT_ONLY" ]]; then
        requested=()
        local name
        while IFS= read -r name; do
            case "$name" in
                tarball)          requested+=(tarball) ;;
                deb|appimage)     list_has debappimage "${requested[@]}" || requested+=(debappimage) ;;
                rpm)              requested+=(rpm) ;;
                flatpak)          requested+=(flatpak) ;;
                checksums)        : ;;  # always runs
                '')               : ;;
                *)                fail "--only: unknown target '$name'" ;;
            esac
        done < <(tr ',' '\n' <<<"$OPT_ONLY")
    fi

    if [[ -n "$OPT_SKIP" ]]; then
        local name kept=() t
        local -a drop=()
        while IFS= read -r name; do
            case "$name" in
                tarball)   drop+=(tarball) ;;
                deb)       drop+=(deb) ;;
                appimage)  drop+=(appimage) ;;
                rpm)       drop+=(rpm) ;;
                flatpak)   drop+=(flatpak) ;;
                '')        : ;;
                *)         fail "--skip: unknown target '$name'" ;;
            esac
        done < <(tr ',' '\n' <<<"$OPT_SKIP")

        # One container run emits both, so skipping one without the other is a
        # request the build cannot honour.
        if list_has deb "${drop[@]}" && ! list_has appimage "${drop[@]}"; then
            fail "--skip deb without --skip appimage: one container run emits both"
        fi
        if list_has appimage "${drop[@]}" && ! list_has deb "${drop[@]}"; then
            fail "--skip appimage without --skip deb: one container run emits both"
        fi

        for t in "${requested[@]}"; do
            case "$t" in
                debappimage) list_has deb "${drop[@]}" || kept+=("$t") ;;
                *)           list_has "$t" "${drop[@]}" || kept+=("$t") ;;
            esac
        done
        requested=("${kept[@]}")
    fi

    [[ "${#requested[@]}" -gt 0 ]] || fail "no build targets selected"
    TARGETS=("${requested[@]}")

    # A narrowed run is as incomplete as one with a skipped target: both produce
    # an artifact set no release should be tagged against.
    if [[ -n "$OPT_ONLY" || -n "$OPT_SKIP" ]]; then
        NARROWED=1
    fi
}

phase_preflight() {
    heading "Preflight"

    git_repo rev-parse --git-dir >/dev/null 2>&1 || fail "not a git repository: $REPO_ROOT"

    local branch
    branch="$(git_repo rev-parse --abbrev-ref HEAD)"
    [[ "$branch" != "HEAD" ]] || fail "HEAD is detached; check out $OPT_BRANCH first"
    if [[ "$branch" != "$OPT_BRANCH" ]]; then
        fail "on branch '$branch', expected '$OPT_BRANCH' (override with --branch)"
    fi

    local tool
    for tool in git cargo sed awk curl tar; do
        require_cmd "$tool" || exit 1
    done
    command -v sha256sum >/dev/null 2>&1 || command -v shasum >/dev/null 2>&1 \
        || fail "sha256sum or shasum is required"
    if [[ "$OPT_DRY_RUN" -ne 1 ]]; then
        require_cmd minisign || exit 1
        [[ -n "$SIGNING_KEY" ]] \
            || fail "LSB_RELEASE_SIGNING_KEY must point to the private minisign release key"
        [[ -r "$SIGNING_KEY" ]] || fail "release signing key is not readable: $SIGNING_KEY"
        [[ -r "$REPO_ROOT/release.pub" ]] || fail "release.pub is missing or unreadable"
    fi

    local script
    for script in \
        "$SCRIPT_DIR/linux/package-tarball.sh" \
        "$SCRIPT_DIR/docker/build-deb-appimage.sh" \
        "$SCRIPT_DIR/docker/build-rpm.sh" \
        "$SCRIPT_DIR/generate-checksums.sh" \
        "$SCRIPT_DIR/validate-metadata.sh" \
        "$SCRIPT_DIR/smoke-check.sh"
    do
        [[ -f "$script" ]] || fail "missing packaging script: ${script#"$REPO_ROOT"/}"
    done

    # Artifacts are built from the working tree (both docker wrappers rsync it,
    # and package-rpm.sh tars it) while the tag names HEAD. A dirty tree means
    # the release cannot be reproduced from its own tag.
    if ! tree_is_clean; then
        if [[ "$OPT_ALLOW_DIRTY" -ne 1 ]]; then
            printf '\n' >&2
            git_repo status --short >&2
            printf '\n' >&2
            fail "working tree is dirty. Artifacts build from the tree but the tag names HEAD, so the release would not be reproducible from its own tag. Commit first, or pass --allow-dirty (which implies --no-tag)."
        fi
        warn "working tree is dirty; artifacts will not match the tagged tree"
        # Recorded before anything is rewritten: rollback must never restore a
        # path over edits that were already there.
        mapfile -t DIRTY_AT_START < <(git_repo status --porcelain | sed 's/^...//')
        if [[ "$OPT_TAG_DIRTY" -ne 1 ]]; then
            OPT_NO_TAG=1
            note "--allow-dirty implies --no-tag (override with --tag-dirty)"
        fi
    fi

    # Committing is a pipeline step, so a missing identity has to surface here.
    # Discovering it after the bump would mean rolling back a completed rewrite.
    if ! git_repo var GIT_COMMITTER_IDENT >/dev/null 2>&1; then
        local suggest_name suggest_email
        suggest_name="$(git_repo log -1 --pretty=%an 2>/dev/null || printf 'Your Name')"
        suggest_email="$(git_repo log -1 --pretty=%ae 2>/dev/null || printf 'you@example.com')"
        fail "git has no author identity, so the release commit would fail after the bump. Set it with:
    git config user.name '$suggest_name'
    git config user.email '$suggest_email'"
    fi

    OLD_VERSION="$(cargo_version_from_manifest "$MANIFEST_PATH")" || exit 1
    OLD_PKGREL="$(sed -n 's/^pkgrel=\([0-9]\+\)$/\1/p' "$AUR_DIR/PKGBUILD" | head -n 1)"
    [[ -n "$OLD_PKGREL" ]] || OLD_PKGREL=1

    resolve_targets

    info "repo     $REPO_ROOT"
    info "branch   $branch"
    info "HEAD     $(git_repo rev-parse --short HEAD)  $(git_repo log -1 --pretty=%s)"
    info "tree     $(tree_is_clean && printf 'clean' || printf 'DIRTY')"
    info "version  $OLD_VERSION"
}

# ═══════════════════════════════════════════════════════════════════════════
# Phase 1 — collect
# ═══════════════════════════════════════════════════════════════════════════

# The release notes come from the CHANGELOG's ## [Unreleased] block, or from
# --notes-file when the notes are written somewhere else. Both are read in
# Keep-a-Changelog shape, so everything downstream is identical.
notes_source() {
    if [[ -n "$OPT_NOTES_FILE" ]]; then
        # Wrapped in a synthetic heading so the same parser applies.
        printf '## [Unreleased]\n'
        cat "$OPT_NOTES_FILE"
        printf '\n## [end]\n'
    else
        cat "$CHANGELOG"
    fi
}

# Emits "section<TAB>lead-in" for each bullet of the notes block.
unreleased_entries() {
    notes_source | awk '
        /^## \[Unreleased\]/ { inblock = 1; next }
        inblock && /^## \[/  { exit }
        inblock && /^### /   { section = substr($0, 5); next }
        inblock && /^- /     {
            line = substr($0, 3)
            if (substr(line, 1, 2) == "**") {
                line = substr(line, 3)
                idx = index(line, ":**")
                if (idx > 0) { lead = substr(line, 1, idx - 1) }
                else {
                    idx = index(line, "**")
                    lead = (idx > 0) ? substr(line, 1, idx - 1) : line
                }
            } else {
                # Plain bullet: keep the first sentence, which is how the older
                # entries in this changelog are written.
                idx = index(line, ". ")
                lead = (idx > 0) ? substr(line, 1, idx - 1) : line
                sub(/\.$/, "", lead)
            }
            printf "%s\t%s\n", (section == "" ? "Changed" : section), lead
        }
    '
}

# Keep-a-Changelog order, which is also the order the shipped 2.2.0 summaries
# follow.
section_rank() {
    case "$1" in
        Added)    printf '1' ;;
        Changed)  printf '2' ;;
        Deprecated) printf '3' ;;
        Removed)  printf '4' ;;
        Fixed)    printf '5' ;;
        Security) printf '6' ;;
        *)        printf '7' ;;
    esac
}

sorted_entries() {
    local section lead
    while IFS=$'\t' read -r section lead; do
        printf '%s\t%s\t%s\n' "$(section_rank "$section")" "$section" "$lead"
    done < <(unreleased_entries) | sort -s -k1,1n | cut -f2-
}

write_summary_draft() {
    local target="$1" total=0 taken=0
    local -a chosen_sections=() chosen_leads=() dropped=()
    local section lead

    while IFS=$'\t' read -r section lead; do
        total=$((total + 1))
        if [[ "$taken" -lt "$OPT_SUMMARY_MAX" ]]; then
            chosen_sections+=("$section")
            chosen_leads+=("$lead")
            taken=$((taken + 1))
        else
            dropped+=("$lead")
        fi
    done < <(sorted_entries)

    [[ "$total" -gt 0 ]] || fail "docs/CHANGELOG.md has an empty ## [Unreleased] section; there is nothing to release"

    {
        printf '# %d entr%s in [Unreleased], capped to %d.\n' \
            "$total" "$([[ "$total" -eq 1 ]] && printf 'y' || printf 'ies')" "$OPT_SUMMARY_MAX"
        printf '# Every line below is the bold lead-in, verbatim. Rewrite them.\n'
        printf '# Lines starting with # are ignored.\n\n'

        printf '# ---- deb + rpm bullets: imperative ("Apply...", "Verify..."), ~6-10 words ----\n'
        local i
        for i in "${!chosen_leads[@]}"; do
            printf '* %s\n' "${chosen_leads[$i]}"
        done

        printf '\n# ---- AppStream: third person ("Applies...", "Verifies...") ----\n'
        printf '# The headline names the release theme. It cannot be derived; write it.\n'
        printf 'headline: EDIT-ME\n'
        for i in "${!chosen_leads[@]}"; do
            printf -- '- %s\n' "${chosen_leads[$i]}"
        done

        if [[ "${#dropped[@]}" -gt 0 ]]; then
            printf '\n# dropped (%d) - move one up if it belongs in the release summary:\n' "${#dropped[@]}"
            local d
            for d in "${dropped[@]}"; do
                printf '#   %s\n' "$d"
            done
        fi
    } >"$target"
}

summaries_valid() {
    local file="$1"
    grep -q '^\* .' "$file" || { warn "no deb/rpm bullets found"; return 1; }
    grep -q '^- .'  "$file" || { warn "no AppStream items found"; return 1; }
    grep -q '^headline: .' "$file" || { warn "no AppStream headline found"; return 1; }
    if grep -q 'EDIT-ME' "$file"; then
        warn "the AppStream headline is still EDIT-ME"
        return 1
    fi
    local n
    n="$(grep -c '^\* ' "$file" || true)"
    if [[ "$n" -gt "$OPT_SUMMARY_MAX" ]]; then
        warn "$n deb/rpm bullets exceeds --summary-max $OPT_SUMMARY_MAX"
        return 1
    fi
    return 0
}

summary_deb_bullets()  { sed -n 's/^\* \(.*\)$/\1/p' "$SUMMARY_FILE"; }
summary_appstream()    { sed -n 's/^- \(.*\)$/\1/p'  "$SUMMARY_FILE"; }
summary_headline()     { sed -n 's/^headline: \(.*\)$/\1/p' "$SUMMARY_FILE" | head -n 1; }

collect_version() {
    if [[ -n "$OPT_VERSION" ]]; then
        semver_valid "$OPT_VERSION" || fail "--version: '$OPT_VERSION' is not X.Y.Z"
        NEW_VERSION="$OPT_VERSION"
        return
    fi
    if [[ -n "$OPT_BUMP" ]]; then
        NEW_VERSION="$(semver_bump "$OLD_VERSION" "$OPT_BUMP")" \
            || fail "--bump: expected patch, minor or major"
        return
    fi

    local patch minor major reply
    patch="$(semver_bump "$OLD_VERSION" patch)"
    minor="$(semver_bump "$OLD_VERSION" minor)"
    major="$(semver_bump "$OLD_VERSION" major)"

    printf '\n[1/6]  Version\n'
    printf '       current  %s\n\n' "$OLD_VERSION"
    printf '         1) patch   %s\n' "$patch"
    printf '         2) minor   %s\n' "$minor"
    printf '         3) major   %s\n' "$major"
    printf '         4) other   (type it)\n\n'

    reply="$(ask '       choice [2]: ' '2')"
    case "$reply" in
        1) NEW_VERSION="$patch" ;;
        2) NEW_VERSION="$minor" ;;
        3) NEW_VERSION="$major" ;;
        4)
            NEW_VERSION="$(ask '       version: ' '')"
            semver_valid "$NEW_VERSION" || fail "'$NEW_VERSION' is not X.Y.Z"
            ;;
        *) fail "unrecognised choice '$reply'" ;;
    esac
}

collect_date() {
    if [[ -n "$OPT_DATE" ]]; then
        RELEASE_DATE="$OPT_DATE"
    elif [[ "$OPT_YES" -eq 1 ]]; then
        RELEASE_DATE="$(date +%F)"
    else
        local today
        today="$(date +%F)"
        printf '\n[2/6]  Release date\n'
        printf '       Used four ways, in four formats:\n'
        printf '         docs/CHANGELOG.md heading    %s\n' "$today"
        printf '         AppStream <release date=>    %s\n' "$today"
        printf '         RPM %%changelog               %s\n' "$(LC_ALL=C date -d "$today" '+%a %b %d %Y')"
        printf '         debian/changelog trailer     %s\n\n' "$(LC_ALL=C date -R)"
        RELEASE_DATE="$(ask "       date [$today]: " "$today")"
    fi

    # Forcing the C locale is not optional: '+%a %b %d %Y' emits localised month
    # names on a non-English host, which rpmbuild rejects.
    DATE_ISO="$(LC_ALL=C date -d "$RELEASE_DATE" +%F)" \
        || fail "unparseable date: $RELEASE_DATE"
    DATE_RPM="$(LC_ALL=C date -d "$RELEASE_DATE" '+%a %b %d %Y')"
    if [[ "$DATE_ISO" == "$(date +%F)" ]]; then
        DATE_RFC="$(LC_ALL=C date -R)"
    else
        DATE_RFC="$(LC_ALL=C date -R -d "$RELEASE_DATE")"
    fi
}

collect_pkgrel() {
    if [[ -n "$OPT_PKGREL" ]]; then
        PKGREL="$OPT_PKGREL"
    elif [[ "$OPT_YES" -eq 1 ]]; then
        PKGREL=1
    else
        printf '\n[3/6]  Package revision  (pkgrel / Debian revision / RPM Release:)\n'
        printf '       1  first build of %s\n' "$NEW_VERSION"
        printf '       2  same source, repackaged - only if %s already shipped\n\n' "$NEW_VERSION"
        printf '       Lands in three filenames:\n'
        printf '         %s_%s-N_amd64.deb\n' "$APP_BINARY" "$NEW_VERSION"
        printf '         %s-%s-N.x86_64.rpm\n' "$APP_BINARY" "$NEW_VERSION"
        printf '         AUR pkgrel=N\n\n'
        PKGREL="$(ask '       pkgrel [1]: ' '1')"
    fi
    [[ "$PKGREL" =~ ^[0-9]+$ ]] || fail "pkgrel must be a number, got '$PKGREL'"
}

collect_notes() {
    local origin action
    if [[ -n "$OPT_NOTES_FILE" ]]; then
        [[ -f "$OPT_NOTES_FILE" ]] || fail "--notes-file: no such file: $OPT_NOTES_FILE"
        origin="$OPT_NOTES_FILE"
        action="Insert it as"
    else
        origin="docs/CHANGELOG.md ## [Unreleased]"
        action="Promote it to"
    fi

    local total added fixed
    total="$(unreleased_entries | wc -l | tr -d ' ')"
    # The summaries are derived from these bullets, so zero parseable entries is
    # a dead end however the notes arrived.
    [[ "$total" -gt 0 ]] \
        || fail "$origin has no Keep-a-Changelog bullets; there is nothing to release"

    [[ "$OPT_YES" -eq 1 ]] && return

    added="$(unreleased_entries | grep -c '^Added' || true)"
    fixed="$(unreleased_entries | grep -c '^Fixed' || true)"

    printf '\n[4/6]  Release notes\n'
    printf '       %s holds %s entries' "$origin" "$total"
    printf '  (Added %s, Fixed %s)\n\n' "$added" "$fixed"
    sorted_entries | head -n 4 | while IFS=$'\t' read -r section lead; do
        printf '         %-7s %s\n' "$section" "$lead"
    done
    [[ "$total" -gt 4 ]] && printf '                 ... and %s more\n' "$((total - 4))"
    printf '\n         1) %s ## [%s] - %s\n' "$action" "$NEW_VERSION" "$DATE_ISO"
    printf '         2) Abort - nothing has been written yet\n\n'

    local reply
    reply="$(ask '       choice [1]: ' '1')"
    [[ "$reply" == "1" ]] || abort_no_writes
}

collect_summaries() {
    SUMMARY_FILE="$WORK_DIR/summaries"

    if [[ -n "$OPT_SUMMARIES_FILE" ]]; then
        [[ -f "$OPT_SUMMARIES_FILE" ]] || fail "--summaries-file: no such file: $OPT_SUMMARIES_FILE"
        cp "$OPT_SUMMARIES_FILE" "$SUMMARY_FILE"
        summaries_valid "$SUMMARY_FILE" || fail "--summaries-file: rejected, see the warning above"
        return
    fi

    write_summary_draft "$SUMMARY_FILE"

    if [[ "$OPT_SUMMARIES_AUTO" -eq 1 || "$OPT_YES" -eq 1 ]]; then
        # The headline cannot be derived from the changelog, so the unattended
        # path substitutes a neutral one rather than shipping the marker.
        sed -i "s/^headline: EDIT-ME$/headline: $APP_NAME $NEW_VERSION:/" "$SUMMARY_FILE"
        summaries_valid "$SUMMARY_FILE" || fail "generated draft is not usable"
        note "using the mechanical draft verbatim (--summaries-auto)"
        return
    fi

    printf '\n[5/6]  Package summaries\n'
    printf '       The .deb, .rpm and AppStream entries carry short summaries, not the\n'
    printf '       full CHANGELOG. Capped at %s lines (--summary-max).\n\n' "$OPT_SUMMARY_MAX"
    printf '       The draft is mechanical. It needs your voice.\n\n'
    # The literal name is what the reader needs to see, not its value.
    # shellcheck disable=SC2016
    printf '         1) Edit the draft in $EDITOR\n'
    printf '         2) Accept the draft verbatim\n'
    printf '         3) Abort\n\n'

    local reply editor
    while :; do
        reply="$(ask '       choice [1]: ' '1')"
        case "$reply" in
            1)
                editor="${VISUAL:-${EDITOR:-vi}}"
                "$editor" "$SUMMARY_FILE" </dev/tty >/dev/tty 2>&1 || true
                if summaries_valid "$SUMMARY_FILE"; then
                    return
                fi
                info "the draft is not finished yet"
                ;;
            2)
                sed -i "s/^headline: EDIT-ME$/headline: $APP_NAME $NEW_VERSION:/" "$SUMMARY_FILE"
                summaries_valid "$SUMMARY_FILE" && return
                fail "generated draft is not usable"
                ;;
            3) abort_no_writes ;;
            *) info "choose 1, 2 or 3" ;;
        esac
    done
}

collect_maintainer() {
    if [[ -n "$OPT_MAINTAINER" ]]; then
        MAINTAINER="$OPT_MAINTAINER"
        return
    fi
    if [[ -n "${DEBFULLNAME:-}" && -n "${DEBEMAIL:-}" ]]; then
        MAINTAINER="$DEBFULLNAME <$DEBEMAIL>"
        return
    fi
    # The changelog trailer is the most recently maintained copy of this string,
    # so it stays in sync without a second place to update.
    MAINTAINER="$(sed -n 's/^ -- \(.*\)  [A-Z][a-z][a-z], .*$/\1/p' "$DEBIAN_CHANGELOG" | head -n 1)"
    [[ -n "$MAINTAINER" ]] || fail "could not read the maintainer from $DEBIAN_CHANGELOG; pass --maintainer"
}

report_targets() {
    printf '\n[6/6]  Targets\n\n'
    local t available=0 unavailable=0
    for t in tarball debappimage rpm flatpak; do
        local state
        if ! list_has "$t" "${TARGETS[@]}"; then
            state="not requested"
        elif [[ "$OPT_FAKE_CONTAINERS" -eq 1 ]]; then
            state="FAKED (--fake-containers)"
            available=$((available + 1))
        elif target_is_available "$t"; then
            state="ready"
            available=$((available + 1))
        else
            state="MISSING: $(target_requirement "$t")"
            unavailable=$((unavailable + 1))
        fi
        printf '       %-16s %-32s %s\n' "$(target_label "$t")" "$(target_requirement "$t")" "$state"
    done
    printf '       %-16s %-32s %s\n' "checksums" "sha256sum" "ready"

    [[ "$unavailable" -eq 0 ]] && return 0

    printf '\n'
    warn "$unavailable of ${#TARGETS[@]} build targets cannot run on this host."
    info "A partial artifact set is not a release: the tag will not be created."

    if [[ "$OPT_DRY_RUN" -eq 1 ]]; then
        note "dry run: nothing is built, so this does not block"
        return 0
    fi
    if [[ "$OPT_PARTIAL_OK" -eq 1 || "$OPT_FORCE_TAG" -eq 1 ]]; then
        note "continuing anyway"
        return 0
    fi
    if [[ "$OPT_YES" -eq 1 ]]; then
        # Unattended runs get a stated reason rather than a prompt that silently
        # takes its own default.
        fail "$unavailable target(s) cannot build here. Pass --partial-ok to build what is possible (no tag is created), or run where every target is available."
    fi

    printf '\n         1) Continue - build what is possible, no tag (exits 2)\n'
    printf '         2) Abort\n\n'
    local reply
    reply="$(ask '       choice [2]: ' '2')"
    [[ "$reply" == "1" ]] || abort_no_writes
}

phase_collect() {
    collect_version
    collect_date
    collect_pkgrel

    # Checked before the notes are gathered, not inside phase_bump: a completed
    # bump leaves [Unreleased] empty by design, and asking for release notes
    # again would reject a perfectly good resume.
    if bump_already_applied; then
        BUMP_ALREADY_APPLIED=1
        note "$NEW_VERSION-$PKGREL is already applied; this run resumes at validate"
    else
        collect_notes
        collect_summaries
    fi

    collect_maintainer
    report_targets

    TAG_NAME="v$NEW_VERSION"
}

# ═══════════════════════════════════════════════════════════════════════════
# Phase 2 — confirm
# ═══════════════════════════════════════════════════════════════════════════

bump_file_list() {
    printf '%s\n' \
        "src/Cargo.toml|version, metadata.deb revision" \
        "Cargo.lock|cargo update --offline -p $APP_BINARY" \
        "packaging/rpm/linux-soundboard.spec|Version:, Release:, %changelog" \
        "packaging/debian/changelog|new top stanza $NEW_VERSION-$PKGREL" \
        "packaging/aur/PKGBUILD|pkgver, pkgrel, sha256sums='SKIP'" \
        "packaging/aur/.SRCINFO|makepkg --printsrcinfo" \
        "packaging/flatpak/$APP_ID.metainfo.xml|new <release> block" \
        "docs/CHANGELOG.md|promote [Unreleased]" \
        "docs/INSTALL.md|3 filenames" \
        "docs/TROUBLESHOOTING.md|2 filenames"
    [[ -f "$LEGACY_DEB_CONTROL" ]] && printf '%s\n' "packaging/deb/control|Version: (legacy)"
}

phase_confirm() {
    local skipped_note="" t
    for t in "${TARGETS[@]}"; do
        target_is_available "$t" || skipped_note+="$(target_label "$t"), "
    done

    printf '\n'
    rule
    printf '  Review\n'
    rule
    printf '  version    %s  ->  %s\n' "$OLD_VERSION" "$NEW_VERSION"
    printf '  pkgrel     %s\n' "$PKGREL"
    printf '  date       %s\n' "$DATE_ISO"
    printf '  commit     release: bump to %s\n' "$NEW_VERSION"
    if [[ "$OPT_NO_TAG" -eq 1 ]]; then
        printf '  tag        (disabled)\n'
    else
        printf '  tag        %s   annotated, "%s %s"\n' "$TAG_NAME" "$APP_NAME" "$NEW_VERSION"
    fi
    printf '  maintainer %s\n' "$MAINTAINER"
    if [[ -n "$skipped_note" ]]; then
        printf '  targets    %s skipped: %s\n' 'SOME' "${skipped_note%, }"
    else
        printf '  targets    %s\n' "${TARGETS[*]}"
    fi
    if [[ "$OPT_PUSH" -eq 1 ]]; then
        printf '  push       armed - will be confirmed separately\n'
    else
        printf '  push       no - the exact commands will be printed\n'
    fi

    if [[ "$BUMP_ALREADY_APPLIED" -eq 1 ]]; then
        printf '\n  The bump is already applied; this run only builds, verifies and tags.\n'
        [[ "$OPT_DRY_RUN" -eq 1 ]] && return 0
        confirm "  Continue?" || abort_no_writes
        return 0
    fi

    printf '\n  files this will rewrite\n'
    local entry path
    while IFS= read -r entry; do
        path="${entry%%|*}"
        # The metainfo path is long enough to break the column; wrap instead of
        # letting it push the description off the edge.
        if [[ "${#path}" -gt 38 ]]; then
            printf '    %s\n    %-38s %s\n' "$path" "" "${entry#*|}"
        else
            printf '    %-38s %s\n' "$path" "${entry#*|}"
        fi
    done < <(bump_file_list)

    printf '\n  Nothing has been written yet.\n'
    if [[ "$OPT_DRY_RUN" -eq 1 ]]; then
        note "dry run: the bump will be applied, shown as a diff, then rolled back"
        return
    fi
    confirm "  Write these changes?" || abort_no_writes
}

# ═══════════════════════════════════════════════════════════════════════════
# Phase 3 — bump
# ═══════════════════════════════════════════════════════════════════════════

bump_cargo_toml() {
    local tmp; tmp="$(bump_tmp "$MANIFEST_PATH")"
    awk -v new="$NEW_VERSION" -v rev="$PKGREL" '
        !seen_version && /^version = "/ { sub(/"[^"]*"/, "\"" new "\""); seen_version = 1 }
        !seen_rev && /^revision = "/    { sub(/"[^"]*"/, "\"" rev "\""); seen_rev = 1 }
        { print }
    ' "$MANIFEST_PATH" >"$tmp"
    replace_file "$MANIFEST_PATH" "$tmp"
    track_bumped src/Cargo.toml

    local check
    check="$(cargo_version_from_manifest "$MANIFEST_PATH")"
    [[ "$check" == "$NEW_VERSION" ]] || fail "src/Cargo.toml still reads $check after the rewrite"
}

bump_cargo_lock() {
    # `cargo update -p` touches only this package's entry; generate-lockfile
    # would re-resolve all 263 and could drift transitive deps inside a release.
    # --offline keeps it from reaching the network to do so.
    ( cd "$REPO_ROOT" && cargo update --offline -p "$APP_BINARY" >/dev/null 2>&1 ) \
        || fail "cargo update --offline -p $APP_BINARY failed"
    track_bumped Cargo.lock

    local stat
    stat="$(git_repo diff --numstat -- Cargo.lock | awk '{print $1, $2}')"
    [[ "$stat" == "1 1" ]] \
        || fail "Cargo.lock changed by more than the package version (numstat: ${stat:-none}); cargo re-resolved dependencies"
}

bump_spec() {
    local entry tmp; tmp="$(bump_tmp "$SPEC")"

    entry="* $DATE_RPM $MAINTAINER - $NEW_VERSION-$PKGREL"
    local bullet
    while IFS= read -r bullet; do
        entry+=$'\n'"- $bullet"
    done < <(summary_deb_bullets)
    entry+=$'\n'

    # The eight spaces after the field name are load-bearing: validate-metadata.sh
    # matches Version: with a fixed-width lookbehind.
    # The entry travels through the environment because awk -v interprets
    # backslash escapes in its value.
    SPEC_ENTRY="$entry" awk -v version="$NEW_VERSION" -v release="$PKGREL" '
        BEGIN { entry = ENVIRON["SPEC_ENTRY"] }
        /^Version:        / { print "Version:        " version; next }
        /^Release:        / { print "Release:        " release; next }
        { print }
        /^%changelog$/ && !inserted { print entry; inserted = 1 }
    ' "$SPEC" >"$tmp"
    replace_file "$SPEC" "$tmp"
    track_bumped packaging/rpm/linux-soundboard.spec
}

bump_debian_changelog() {
    local tmp; tmp="$(bump_tmp "$DEBIAN_CHANGELOG")"
    {
        printf '%s (%s-%s) unstable; urgency=medium\n\n' "$APP_BINARY" "$NEW_VERSION" "$PKGREL"
        summary_deb_bullets | sed 's/^/  * /'
        # Debian requires exactly one space before `--` and two before the date.
        printf '\n -- %s  %s\n\n' "$MAINTAINER" "$DATE_RFC"
        cat "$DEBIAN_CHANGELOG"
    } >"$tmp"
    replace_file "$DEBIAN_CHANGELOG" "$tmp"
    track_bumped packaging/debian/changelog
}

bump_pkgbuild() {
    local pkgbuild="$AUR_DIR/PKGBUILD"
    sed -i \
        -e "s/^pkgver=.*/pkgver=$NEW_VERSION/" \
        -e "s/^pkgrel=.*/pkgrel=$PKGREL/" \
        -e "s/^sha256sums=.*/sha256sums=('SKIP')/" \
        "$pkgbuild"
    track_bumped packaging/aur/PKGBUILD
}

regenerate_srcinfo() {
    command -v makepkg >/dev/null 2>&1 || {
        warn "makepkg is not installed; packaging/aur/.SRCINFO cannot be regenerated"
        warn "run this on an Arch host: (cd packaging/aur && makepkg --printsrcinfo > .SRCINFO)"
        return 1
    }
    ( cd "$AUR_DIR" && makepkg --printsrcinfo >.SRCINFO.new ) \
        || { rm -f "$AUR_DIR/.SRCINFO.new"; return 1; }
    replace_file "$AUR_DIR/.SRCINFO" "$AUR_DIR/.SRCINFO.new"
    track_bumped packaging/aur/.SRCINFO
}

bump_metainfo() {
    local block headline tmp; tmp="$(bump_tmp "$METAINFO")"

    headline="$(summary_headline)"
    block="    <release version=\"$NEW_VERSION\" date=\"$DATE_ISO\">"$'\n'
    block+="      <description>"$'\n'
    block+="        <p>$headline</p>"$'\n'
    block+="        <ul>"$'\n'
    local item
    while IFS= read -r item; do
        block+="          <li>$item</li>"$'\n'
    done < <(summary_appstream)
    block+="        </ul>"$'\n'
    block+="      </description>"$'\n'
    block+="    </release>"

    RELEASE_BLOCK="$block" awk '
        BEGIN { block = ENVIRON["RELEASE_BLOCK"] }
        { print }
        /^  <releases>$/ && !inserted { print block; inserted = 1 }
    ' "$METAINFO" >"$tmp"
    replace_file "$METAINFO" "$tmp"
    track_bumped "packaging/flatpak/$APP_ID.metainfo.xml"
}

bump_changelog() {
    local tmp; tmp="$(bump_tmp "$CHANGELOG")"

    if [[ -n "$OPT_NOTES_FILE" ]]; then
        # The notes live outside the changelog, so a new section is inserted
        # below [Unreleased] instead of promoting it. Anything still sitting in
        # [Unreleased] stays there: it was deliberately not part of this release.
        local body
        body="$(cat "$OPT_NOTES_FILE")"
        NOTES_BODY="$body" awk -v version="$NEW_VERSION" -v date="$DATE_ISO" '
            BEGIN { body = ENVIRON["NOTES_BODY"] }
            /^## \[Unreleased\]$/ && !seen { seen = 1; print; next }
            seen && !inserted && /^## \[/ {
                print "## [" version "] - " date
                print ""
                print body
                print ""
                inserted = 1
            }
            { print }
            END {
                if (seen && !inserted) {
                    print "## [" version "] - " date
                    print ""
                    print body
                }
            }
        ' "$CHANGELOG" >"$tmp"
        grep -qF "## [$NEW_VERSION] - $DATE_ISO" "$tmp" \
            || fail "docs/CHANGELOG.md has no ## [Unreleased] heading to insert below"
    else
        # Promote: the existing body stays attached to the new version heading
        # and a fresh empty [Unreleased] lands on top.
        awk -v version="$NEW_VERSION" -v date="$DATE_ISO" '
            /^## \[Unreleased\]$/ && !promoted {
                print "## [Unreleased]"
                print ""
                print "## [" version "] - " date
                promoted = 1
                next
            }
            { print }
        ' "$CHANGELOG" >"$tmp"
    fi

    replace_file "$CHANGELOG" "$tmp"
    track_bumped docs/CHANGELOG.md
}

# Docs are prose. Each pattern is a complete filename shape with an asserted
# occurrence count, so a drifted document aborts the bump instead of being
# silently mangled — and the historical "v2.1.1"/"v2.2.0" references in
# TROUBLESHOOTING.md can never match.
bump_doc_filenames() {
    local rel_path="$1"; shift
    local file="$REPO_ROOT/$rel_path"
    local pattern replacement expected found

    # Tracked up front: a count assertion that fails on the second pattern must
    # still leave the first pattern's rewrite inside the rollback set.
    track_bumped "$rel_path"

    while [[ $# -gt 0 ]]; do
        pattern="$1"; replacement="$2"; expected="$3"; shift 3
        found="$(count_occurrences "$file" "$pattern")"
        if [[ "$found" -ne "$expected" ]]; then
            fail "$rel_path: expected $expected occurrence(s) of '$pattern', found $found; the docs have drifted"
        fi
        [[ "$expected" -eq 0 ]] && continue
        local tmp; tmp="$(bump_tmp "$file")"
        PATTERN="$pattern" REPLACEMENT="$replacement" \
            awk '
                BEGIN { p = ENVIRON["PATTERN"]; r = ENVIRON["REPLACEMENT"]; n = length(p) }
                {
                    out = ""
                    line = $0
                    while ((i = index(line, p)) > 0) {
                        out = out substr(line, 1, i - 1) r
                        line = substr(line, i + n)
                    }
                    print out line
                }
            ' "$file" >"$tmp"
        replace_file "$file" "$tmp"
    done
}

bump_docs() {
    local old_tar="$APP_BINARY-$OLD_VERSION-linux-x86_64"
    local new_tar="$APP_BINARY-$NEW_VERSION-linux-x86_64"
    local old_deb="${APP_BINARY}_${OLD_VERSION}-${OLD_PKGREL}_amd64.deb"
    local new_deb="${APP_BINARY}_${NEW_VERSION}-${PKGREL}_amd64.deb"
    local old_rpm="$APP_BINARY-$OLD_VERSION-$OLD_PKGREL.x86_64.rpm"
    local new_rpm="$APP_BINARY-$NEW_VERSION-$PKGREL.x86_64.rpm"

    bump_doc_filenames docs/INSTALL.md \
        "$old_tar" "$new_tar" 3 \
        "$old_deb" "$new_deb" 1 \
        "$old_rpm" "$new_rpm" 1

    bump_doc_filenames docs/TROUBLESHOOTING.md \
        "$old_tar" "$new_tar" 0 \
        "$old_deb" "$new_deb" 1 \
        "$old_rpm" "$new_rpm" 1
}

bump_legacy_deb_control() {
    [[ -f "$LEGACY_DEB_CONTROL" ]] || return 0

    if [[ "$OPT_PRUNE_LEGACY" -eq 1 ]]; then
        git_repo rm -q -- packaging/deb/control
        track_bumped packaging/deb/control
        note "removed packaging/deb/control (legacy, unused by any build)"
        return 0
    fi

    # No validator checks this file's version, so drift here is silent. Bumping
    # it is one line and keeps the tree honest until it is removed for good.
    sed -i "s/^Version: .*/Version: $NEW_VERSION/" "$LEGACY_DEB_CONTROL"
    track_bumped packaging/deb/control
}

bump_already_applied() {
    local cargo_v deb_v spec_v
    cargo_v="$(cargo_version_from_manifest "$MANIFEST_PATH" 2>/dev/null || printf '')"
    deb_v="$(sed -n "1s/^$APP_BINARY (\([^)]*\)).*/\1/p" "$DEBIAN_CHANGELOG")"
    spec_v="$(sed -n 's/^\* .* - \(.*\)$/\1/p' "$SPEC" | head -n 1)"

    local want="$NEW_VERSION-$PKGREL"
    if [[ "$cargo_v" == "$NEW_VERSION" && "$deb_v" == "$want" && "$spec_v" == "$want" ]]; then
        return 0
    fi
    # A tree where only some of the three agree was not produced by this script's
    # rollback, so it cannot be resumed safely.
    if [[ "$cargo_v" == "$NEW_VERSION" || "$deb_v" == "$want" || "$spec_v" == "$want" ]]; then
        fail "partial bump detected (Cargo.toml=$cargo_v debian=$deb_v spec=$spec_v); resolve by hand"
    fi
    return 1
}

phase_bump() {
    if [[ "$BUMP_ALREADY_APPLIED" -eq 1 ]]; then
        heading "Bump"
        skip "already applied"
        return
    fi

    heading "Bump"

    BUMP_IN_PROGRESS=1

    bump_cargo_toml       ; info "src/Cargo.toml"
    bump_cargo_lock       ; info "Cargo.lock"
    bump_spec             ; info "packaging/rpm/linux-soundboard.spec"
    bump_debian_changelog ; info "packaging/debian/changelog"
    bump_pkgbuild         ; info "packaging/aur/PKGBUILD"
    regenerate_srcinfo    || fail "could not regenerate packaging/aur/.SRCINFO"
    info "packaging/aur/.SRCINFO"
    bump_metainfo         ; info "packaging/flatpak/$APP_ID.metainfo.xml"
    bump_changelog        ; info "docs/CHANGELOG.md"
    bump_docs             ; info "docs/INSTALL.md, docs/TROUBLESHOOTING.md"
    bump_legacy_deb_control

    # Gate the bump in its own phase: a malformed insertion must not survive to
    # the build, let alone the tag.
    heading "Bump gate"
    bash "$SCRIPT_DIR/validate-metadata.sh" >/dev/null \
        || fail "validate-metadata.sh rejected the bump; run it for details"
    pass "validate-metadata.sh"

    if command -v appstreamcli >/dev/null 2>&1; then
        appstreamcli validate --no-net "$METAINFO" >/dev/null 2>&1 \
            || fail "appstreamcli rejected the metainfo insertion"
        pass "appstreamcli validate"
    else
        skip "appstreamcli not installed"
    fi
}

# ═══════════════════════════════════════════════════════════════════════════
# Phase 4 / 5 — review and commit
# ═══════════════════════════════════════════════════════════════════════════

phase_review() {
    [[ "$BUMP_ALREADY_APPLIED" -eq 1 ]] && return 0

    heading "Review"
    git_repo --no-pager diff --stat -- "${BUMPED_FILES[@]}" || true

    if [[ "$OPT_DRY_RUN" -eq 1 ]]; then
        printf '\n'
        git_repo --no-pager diff -- "${BUMPED_FILES[@]}" || true
        return 0
    fi

    if [[ "$OPT_NO_REVIEW" -ne 1 && "$OPT_YES" -ne 1 ]]; then
        if confirm "  Show the full diff?"; then
            git_repo --no-pager diff -- "${BUMPED_FILES[@]}" || true
        fi
    fi
    if [[ "$OPT_NO_COMMIT" -eq 1 ]]; then
        confirm "  Stage this bump?" || abort_no_writes
    else
        confirm "  Commit this bump?" || abort_no_writes
    fi
}

phase_commit() {
    [[ "$BUMP_ALREADY_APPLIED" -eq 1 ]] && return 0

    git_repo add -- "${BUMPED_FILES[@]}"

    if [[ "$OPT_NO_COMMIT" -eq 1 ]]; then
        heading "Stopping"
        # Staged rather than committed, so the rollback must not undo it either.
        BUMP_IN_PROGRESS=0
        note "--no-commit: the bump is staged but not committed"
        note "a tag must point at a commit containing the bump, so later phases are refused"
        exit 0
    fi

    heading "Release commit"
    git_repo commit -q -m "release: bump to $NEW_VERSION"

    # Past this point the changes are recorded; the rollback must not fire.
    BUMP_IN_PROGRESS=0

    info "$(git_repo rev-parse --short HEAD)  release: bump to $NEW_VERSION"
}

# ═══════════════════════════════════════════════════════════════════════════
# Phase 6 — validate
# ═══════════════════════════════════════════════════════════════════════════

systemd_default_path_has_binary() {
    local dir
    for dir in "${SYSTEMD_DEFAULT_PATH[@]}"; do
        [[ -x "$dir/$APP_BINARY" ]] && return 0
    done
    return 1
}

# The exemption is re-proved on every run rather than assumed: if the binary is
# resolvable and the check still fails, that is a real regression.
smoke_failure_is_known() {
    local line="$1"
    [[ "$line" == "$SMOKE_KNOWN_FAIL" ]] || return 1
    systemd_default_path_has_binary && return 1
    return 0
}

install_systemd_stub() {
    local built="$REPO_ROOT/target/release/$APP_BINARY"
    [[ -x "$built" ]] || { warn "--smoke-fix-systemd: no built binary at target/release/$APP_BINARY yet"; return 1; }
    # The stub is removed again on exit, so installing over an existing file
    # would delete a real system-wide install of the app.
    if [[ -e "/usr/local/bin/$APP_BINARY" ]]; then
        warn "--smoke-fix-systemd: /usr/local/bin/$APP_BINARY already exists; leaving it untouched"
        return 1
    fi
    info "installing $APP_BINARY to /usr/local/bin so systemd-analyze can resolve ExecStart"
    sudo install -Dm755 "$built" "/usr/local/bin/$APP_BINARY" || return 1
    return 0
}

# Reached through on_exit rather than a call site.
# shellcheck disable=SC2317
remove_systemd_stub() {
    [[ -e "/usr/local/bin/$APP_BINARY" ]] || return 0
    sudo rm -f "/usr/local/bin/$APP_BINARY" || warn "could not remove /usr/local/bin/$APP_BINARY"
}

run_smoke_check() {
    local output ec=0 smoke_failures=() line unexpected=0 known=0

    output="$(bash "$SCRIPT_DIR/smoke-check.sh" 2>&1)" || ec=$?

    while IFS= read -r line; do
        smoke_failures+=("${line#\[FAIL\] }")
    done < <(printf '%s\n' "$output" | grep '^\[FAIL\] ' || true)

    for line in "${smoke_failures[@]+"${smoke_failures[@]}"}"; do
        if smoke_failure_is_known "$line"; then
            known=$((known + 1))
            warn "smoke-check: $line"
            info "  environmental: systemd resolves a bare ExecStart against its own"
            info "  default path, and $APP_BINARY is not installed there."
            info "  Make it green with --smoke-fix-systemd (needs sudo)."
        else
            unexpected=$((unexpected + 1))
            printf '[FAIL] smoke-check: %s\n' "$line" >&2
        fi
    done

    if [[ "$unexpected" -gt 0 ]]; then
        fail "smoke-check.sh reported $unexpected unexpected failure(s); run it for details"
    fi
    if [[ "$ec" -ne 0 && "${#smoke_failures[@]}" -eq 0 ]]; then
        fail "smoke-check.sh exited $ec without reporting a failure line"
    fi

    printf '%s\n' "$output" | tail -n 1 | sed 's/^/       /'
    if [[ "$known" -gt 0 ]]; then
        pass "smoke-check.sh (with $known known-environmental failure)"
    else
        pass "smoke-check.sh"
    fi
}

phase_validate() {
    heading "Validate"

    bash "$SCRIPT_DIR/validate-metadata.sh" >/dev/null \
        || fail "validate-metadata.sh failed; run it for details"
    pass "validate-metadata.sh"

    if [[ "$OPT_SMOKE_FIX_SYSTEMD" -eq 1 ]]; then
        install_systemd_stub && CLEANUP_SYSTEMD_STUB=1
    fi
    run_smoke_check

    if [[ "$OPT_SKIP_TESTS" -eq 1 ]]; then
        skip "cargo fmt / clippy / test (--skip-tests)"
        return
    fi

    ( cd "$REPO_ROOT" && cargo fmt --all --check >/dev/null ) \
        || fail "cargo fmt --all --check failed"
    pass "cargo fmt"

    ( cd "$REPO_ROOT" && cargo clippy --workspace --all-targets --all-features --locked -- -D warnings >/dev/null 2>&1 ) \
        || fail "cargo clippy reported warnings"
    pass "cargo clippy"

    ( cd "$REPO_ROOT" && cargo test --workspace --locked >/dev/null 2>&1 ) \
        || fail "cargo test failed"
    pass "cargo test"
}

# ═══════════════════════════════════════════════════════════════════════════
# Phase 7 — build
# ═══════════════════════════════════════════════════════════════════════════

# generate-checksums.sh globs dist/ by extension, not by version, and every
# packager calls it. A previous release left in place would be published as an
# asset of this one, with a correct hash and a false name.
sweep_dist() {
    [[ -d "$DIST_ROOT" ]] || return 0
    if [[ "$OPT_KEEP_DIST" -eq 1 ]]; then
        note "keeping existing dist/ artifacts (--keep-dist)"
        return 0
    fi

    local stale=() name
    while IFS= read -r name; do
        [[ "$name" == *"$NEW_VERSION"* ]] && continue
        stale+=("$name")
    done < <(find "$DIST_ROOT" -maxdepth 1 -type f \
        \( -name '*.tar.gz' -o -name '*.deb' -o -name '*.rpm' -o -name '*.AppImage' \) \
        -printf '%f\n' | sort)

    [[ "${#stale[@]}" -gt 0 ]] || return 0

    if [[ "$OPT_CLEAN_DIST" -eq 1 ]]; then
        local f
        for f in "${stale[@]}"; do
            rm -f "$DIST_ROOT/$f"
            info "removed stale artifact $f"
        done
        return 0
    fi

    local quarantine
    quarantine="$DIST_ROOT/.previous-$(date +%Y%m%d-%H%M%S)"
    mkdir -p "$quarantine"
    local f
    for f in "${stale[@]}"; do
        mv "$DIST_ROOT/$f" "$quarantine/"
        info "quarantined stale artifact $f"
    done
    note "moved ${#stale[@]} stale artifact(s) to ${quarantine#"$REPO_ROOT"/}"
}

fake_artifacts_for() {
    local target="$1" arch stage
    arch="$(uname -m)"
    mkdir -p "$DIST_ROOT"

    case "$target" in
        tarball)
            stage="$DIST_ROOT/.fake/$APP_BINARY-$NEW_VERSION-linux-$arch"
            mkdir -p "$stage"
            printf 'fake\n' >"$stage/$APP_BINARY"
            tar -czf "$DIST_ROOT/$APP_BINARY-$NEW_VERSION-linux-$arch.tar.gz" \
                -C "$DIST_ROOT/.fake" "$APP_BINARY-$NEW_VERSION-linux-$arch"
            rm -rf "$DIST_ROOT/.fake"
            FAKED+=("$APP_BINARY-$NEW_VERSION-linux-$arch.tar.gz")
            ;;
        debappimage)
            # A real ar archive with the three standard members, so `file` and
            # the naming checks exercise the same code path as a genuine build.
            stage="$DIST_ROOT/.fake"; mkdir -p "$stage"
            printf '2.0\n' >"$stage/debian-binary"
            tar -czf "$stage/control.tar.gz" -C "$stage" debian-binary
            cp "$stage/control.tar.gz" "$stage/data.tar.gz"
            ( cd "$stage" && ar rc "$DIST_ROOT/${APP_BINARY}_${NEW_VERSION}-${PKGREL}_amd64.deb" \
                debian-binary control.tar.gz data.tar.gz )
            rm -rf "$stage"
            cp /bin/true "$DIST_ROOT/$APP_BINARY-$NEW_VERSION-$arch.AppImage"
            cp /bin/true "$DIST_ROOT/$APP_BINARY-$arch.AppImage"
            FAKED+=("${APP_BINARY}_${NEW_VERSION}-${PKGREL}_amd64.deb"
                    "$APP_BINARY-$NEW_VERSION-$arch.AppImage"
                    "$APP_BINARY-$arch.AppImage")
            ;;
        rpm)
            # rpmbuild cannot be faked into producing a valid package; the type
            # check degrades and says so.
            printf 'fake rpm\n' >"$DIST_ROOT/$APP_BINARY-$NEW_VERSION-$PKGREL.x86_64.rpm"
            FAKED+=("$APP_BINARY-$NEW_VERSION-$PKGREL.x86_64.rpm")
            ;;
        flatpak) : ;;
    esac
}

run_target() {
    local target="$1"

    if [[ "$OPT_FAKE_CONTAINERS" -eq 1 ]]; then
        warn "$(target_label "$target"): fabricating artifacts (--fake-containers)"
        fake_artifacts_for "$target"
        BUILT+=("$target")
        return 0
    fi

    case "$target" in
        tarball)
            bash "$SCRIPT_DIR/linux/package-tarball.sh" || return 1
            ;;
        debappimage)
            bash "$SCRIPT_DIR/docker/build-deb-appimage.sh" || return 1
            ;;
        rpm)
            bash "$SCRIPT_DIR/docker/build-rpm.sh" || return 1
            ;;
        flatpak)
            warn "flatpak build is not wired to a packaging script yet; skipping"
            return 2
            ;;
    esac
    BUILT+=("$target")
}

phase_build() {
    heading "Build"
    sweep_dist
    mkdir -p "$DIST_ROOT"

    local target rc
    for target in "${TARGETS[@]}"; do
        if ! target_is_available "$target"; then
            # A missing tool is not a failed build: record it, keep going, and
            # refuse the tag at the end.
            skip "$(target_label "$target") - $(target_requirement "$target") not available"
            SKIPPED+=("$target")
            continue
        fi

        heading "Building $(target_label "$target")"
        rc=0
        run_target "$target" || rc=$?
        if [[ "$rc" -eq 2 ]]; then
            SKIPPED+=("$target")
        elif [[ "$rc" -ne 0 ]]; then
            # The tool was present and failed. That is a real error.
            fail "$(target_label "$target") build failed (exit $rc)"
        fi
    done

    [[ "${#SKIPPED[@]}" -gt 0 ]] && EXIT_STATUS=2
    return 0
}

# ═══════════════════════════════════════════════════════════════════════════
# Phase 8 / 9 — checksums and verification
# ═══════════════════════════════════════════════════════════════════════════

phase_checksums() {
    heading "Checksums"
    LSB_RELEASE_SIGNING_KEY="$SIGNING_KEY" \
    LSB_RELEASE_PUBLIC_KEY="$REPO_ROOT/release.pub" \
    LSB_RELEASE_TAG="$TAG_NAME" \
        bash "$SCRIPT_DIR/generate-checksums.sh" "$DIST_ROOT" \
        || fail "generate-checksums.sh failed"
}

expected_artifacts() {
    local arch t
    arch="$(uname -m)"
    for t in "${BUILT[@]+"${BUILT[@]}"}"; do
        case "$t" in
            tarball)     printf '%s\n' "$APP_BINARY-$NEW_VERSION-linux-$arch.tar.gz" ;;
            debappimage) printf '%s\n' \
                            "${APP_BINARY}_${NEW_VERSION}-${PKGREL}_amd64.deb" \
                            "$APP_BINARY-$NEW_VERSION-$arch.AppImage" \
                            "$APP_BINARY-$arch.AppImage" ;;
            rpm)         printf '%s\n' "$APP_BINARY-$NEW_VERSION-$PKGREL.x86_64.rpm" ;;
        esac
    done
}

# Which packager produced an artifact, and for the container targets which base
# image. Recorded per artifact so "was this AppImage built against Ubuntu 24.04's
# glibc?" is still answerable months later.
builder_for() {
    case "$1" in
        *.tar.gz)
            printf 'packaging/linux/package-tarball.sh (host)' ;;
        *.deb|*.AppImage)
            printf 'packaging/docker/build-deb-appimage.sh (%s)' "${DEB_BUILD_IMAGE:-ubuntu:24.04}" ;;
        *.rpm)
            printf 'packaging/docker/build-rpm.sh (%s)' "${RPM_BUILD_IMAGE:-fedora:latest}" ;;
        *)
            printf 'unknown' ;;
    esac
}

min_size_for() {
    # Floors are deliberately loose: they exist to catch a truncated or empty
    # output, not to police build size.
    case "$1" in
        *.AppImage) printf '%d' $((10 * 1024 * 1024)) ;;
        *.deb)      printf '%d' $((1024 * 1024)) ;;
        *.rpm)      printf '%d' $((1024 * 1024)) ;;
        *.tar.gz)   printf '%d' $((1024 * 1024)) ;;
        *)          printf '%d' 1 ;;
    esac
}

verify_type() {
    local path="$1" name="$2" described
    command -v file >/dev/null 2>&1 || { skip "file(1) not installed; type check skipped"; return 0; }
    described="$(file -b "$path")"
    case "$name" in
        *.tar.gz)   [[ "$described" == *gzip* ]] || return 1 ;;
        *.deb)      [[ "$described" == *"Debian binary package"* || "$described" == *archive* ]] || return 1 ;;
        *.rpm)      [[ "$described" == *RPM* ]] || return 1 ;;
        *.AppImage) [[ "$described" == *ELF* ]] || return 1 ;;
    esac
    return 0
}

phase_verify() {
    heading "Verify"

    local name path
    local failures=0
    local expected=()
    mapfile -t expected < <(expected_artifacts)

    if [[ "${#expected[@]}" -eq 0 ]]; then
        warn "no artifacts were built; nothing to verify"
        return 0
    fi

    for name in "${expected[@]}"; do
        path="$DIST_ROOT/$name"
        if [[ ! -f "$path" ]]; then
            printf '[FAIL] missing artifact: %s\n' "$name" >&2
            failures=$((failures + 1))
            continue
        fi

        # The stable AppImage carries no version by design.
        if [[ "$name" != "$APP_BINARY-$(uname -m).AppImage" && "$name" != *"$NEW_VERSION"* ]]; then
            printf '[FAIL] %s does not carry version %s\n' "$name" "$NEW_VERSION" >&2
            failures=$((failures + 1))
        fi

        local size floor
        size="$(stat -c%s "$path")"
        floor="$(min_size_for "$name")"
        if list_has "$name" "${FAKED[@]+"${FAKED[@]}"}"; then
            :
        elif [[ "$size" -lt "$floor" ]]; then
            printf '[FAIL] %s is %s bytes, below the %s byte floor\n' "$name" "$size" "$floor" >&2
            failures=$((failures + 1))
        fi

        if list_has "$name" "${FAKED[@]+"${FAKED[@]}"}"; then
            warn "$name is FAKE (--fake-containers); type check not meaningful"
        elif ! verify_type "$path" "$name"; then
            printf '[FAIL] %s is not the expected file type (%s)\n' "$name" "$(file -b "$path")" >&2
            failures=$((failures + 1))
        fi
    done

    # Tarballs are the one format whose internal version is readable without
    # dpkg or rpm being installed.
    local tarball
    tarball="$DIST_ROOT/$APP_BINARY-$NEW_VERSION-linux-$(uname -m).tar.gz"
    if [[ -f "$tarball" ]] && ! list_has "$(basename "$tarball")" "${FAKED[@]+"${FAKED[@]}"}"; then
        local top
        # head closes the pipe early, which would surface as a pipefail failure.
        top="$(tar -tzf "$tarball" 2>/dev/null | head -n 1 || true)"
        [[ "$top" == "$APP_BINARY-$NEW_VERSION-linux-$(uname -m)/" ]] \
            || { printf '[FAIL] tarball top-level directory is %s\n' "$top" >&2; failures=$((failures + 1)); }
    fi

    if command -v dpkg-deb >/dev/null 2>&1; then
        local deb="$DIST_ROOT/${APP_BINARY}_${NEW_VERSION}-${PKGREL}_amd64.deb"
        if [[ -f "$deb" ]] && ! list_has "$(basename "$deb")" "${FAKED[@]+"${FAKED[@]}"}"; then
            local dv
            dv="$(dpkg-deb -f "$deb" Version)"
            [[ "$dv" == "$NEW_VERSION-$PKGREL" ]] \
                || { printf '[FAIL] .deb Version is %s\n' "$dv" >&2; failures=$((failures + 1)); }
        fi
    else
        skip "dpkg-deb not installed; in-package .deb version not checked"
    fi

    if command -v rpm >/dev/null 2>&1; then
        local rpmfile="$DIST_ROOT/$APP_BINARY-$NEW_VERSION-$PKGREL.x86_64.rpm"
        if [[ -f "$rpmfile" ]] && ! list_has "$(basename "$rpmfile")" "${FAKED[@]+"${FAKED[@]}"}"; then
            local rv
            rv="$(rpm -qp --queryformat '%{VERSION}-%{RELEASE}' "$rpmfile" 2>/dev/null)"
            [[ "$rv" == "$NEW_VERSION-$PKGREL" ]] \
                || { printf '[FAIL] .rpm version is %s\n' "$rv" >&2; failures=$((failures + 1)); }
        fi
    else
        skip "rpm not installed; in-package .rpm version not checked"
    fi

    # Checksum coverage is the check that actually protects install.sh.
    local sums="$DIST_ROOT/SHA256SUMS.txt"
    [[ -f "$sums" ]] || fail "SHA256SUMS.txt is missing; install.sh cannot verify downloads without it"

    # Compared as exact strings both ways: every artifact is covered, and the
    # list names nothing that is not a current artifact.
    local listed=()
    mapfile -t listed < <(sed 's/^[0-9a-f]\{64\}  //' "$sums")

    for name in "${expected[@]}"; do
        list_has "$name" "${listed[@]+"${listed[@]}"}" \
            || { printf '[FAIL] %s is not listed in SHA256SUMS.txt\n' "$name" >&2; failures=$((failures + 1)); }
    done

    local entry
    for entry in "${listed[@]+"${listed[@]}"}"; do
        [[ -f "$DIST_ROOT/$entry" ]] \
            || { printf '[FAIL] SHA256SUMS.txt lists a missing file: %s\n' "$entry" >&2; failures=$((failures + 1)); }
        list_has "$entry" "${expected[@]}" \
            || { printf '[FAIL] SHA256SUMS.txt lists a stale or unexpected artifact: %s\n' "$entry" >&2; failures=$((failures + 1)); }
    done

    ( cd "$DIST_ROOT" && sha256sum -c --quiet SHA256SUMS.txt ) \
        || { printf '[FAIL] sha256sum -c failed\n' >&2; failures=$((failures + 1)); }

    local signature="$DIST_ROOT/SHA256SUMS.txt.minisig"
    local trusted_comment=""
    if [[ ! -f "$signature" ]]; then
        printf '[FAIL] SHA256SUMS.txt.minisig is missing\n' >&2
        failures=$((failures + 1))
    elif ! trusted_comment="$(minisign -V -H -Q -p "$REPO_ROOT/release.pub" \
        -m "$sums" -x "$signature" 2>/dev/null)"; then
        printf '[FAIL] SHA256SUMS.txt.minisig is invalid\n' >&2
        failures=$((failures + 1))
    elif [[ "$trusted_comment" != "Linux Soundboard release $TAG_NAME" ]]; then
        printf '[FAIL] checksum signature is not bound to %s\n' "$TAG_NAME" >&2
        failures=$((failures + 1))
    fi

    [[ "$failures" -eq 0 ]] || fail "$failures verification failure(s); refusing to tag"
    pass "all artifacts present, named, sized, typed, checksummed and signed"

    write_manifest
}

write_manifest() {
    local manifest="$DIST_ROOT/RELEASE-MANIFEST.txt"
    local name path

    # .txt is outside the checksum globs, so the manifest cannot list itself.
    {
        printf '%s %s\n' "$APP_NAME" "$NEW_VERSION"
        printf '  built      %s\n' "$(date -Is)"
        printf '  commit     %s  %s\n' "$(git_repo rev-parse --short HEAD)" "$(git_repo log -1 --pretty=%s)"
        printf '  tree       %s\n' "$(tree_is_clean && printf 'clean' || printf 'DIRTY - artifacts do not match the tagged tree')"
        printf '  pkgrel     %s\n' "$PKGREL"
        printf '\n'

        while IFS= read -r name; do
            path="$DIST_ROOT/$name"
            printf '  %s\n' "$name"
            if [[ -f "$path" ]]; then
                printf '    sha256   %s\n' "$(sha256sum "$path" | awk '{print $1}')"
                printf '    size     %s\n' "$(stat -c%s "$path")"
            fi
            if list_has "$name" "${FAKED[@]+"${FAKED[@]}"}"; then
                printf '    FAKE     fabricated by --fake-containers, not a real build\n'
            else
                printf '    built by %s\n' "$(builder_for "$name")"
            fi
            printf '\n'
        done < <(expected_artifacts)

        local t
        for t in "${SKIPPED[@]+"${SKIPPED[@]}"}"; do
            printf '  %s\n    SKIPPED  %s not available\n\n' "$(target_label "$t")" "$(target_requirement "$t")"
        done
    } >"$manifest"

    info "manifest ${manifest#"$REPO_ROOT"/}"
}

# ═══════════════════════════════════════════════════════════════════════════
# Phase 10 / 11 — tag and push
# ═══════════════════════════════════════════════════════════════════════════

phase_tag() {
    if [[ "$OPT_NO_TAG" -eq 1 ]]; then
        skip "tag (--no-tag)"
        return 0
    fi
    if [[ "$OPT_FORCE_TAG" -ne 1 ]]; then
        if [[ "${#SKIPPED[@]}" -gt 0 ]]; then
            warn "not tagging: ${#SKIPPED[@]} target(s) were skipped, so the artifact set is incomplete"
            info "re-run where every target can build, or pass --force-tag"
            EXIT_STATUS=2
            return 0
        fi
        if [[ "$NARROWED" -eq 1 ]]; then
            warn "not tagging: --only/--skip narrowed the build, so the artifact set is incomplete"
            info "run without them, or pass --force-tag"
            EXIT_STATUS=2
            return 0
        fi
    fi

    heading "Tag"

    if git_repo rev-parse -q --verify "refs/tags/$TAG_NAME" >/dev/null; then
        local at head
        at="$(git_repo rev-parse "$TAG_NAME^{}")"
        head="$(git_repo rev-parse HEAD)"
        if [[ "$at" == "$head" ]]; then
            note "$TAG_NAME already points at HEAD"
            return 0
        fi
        if [[ "$OPT_RETAG" -ne 1 ]]; then
            fail "$TAG_NAME exists and points at ${at:0:7}, not HEAD (${head:0:7}). Pass --retag to move it."
        fi
        if git_repo ls-remote --exit-code --tags origin "$TAG_NAME" >/dev/null 2>&1; then
            fail "$TAG_NAME is already on the remote; it must not be moved"
        fi
        git_repo tag -d "$TAG_NAME" >/dev/null
        note "deleted the previous local $TAG_NAME"
    fi

    git_repo tag -a "$TAG_NAME" -m "$APP_NAME $NEW_VERSION"
    info "created $TAG_NAME  ($APP_NAME $NEW_VERSION)"
}

phase_push() {
    if ! git_repo rev-parse -q --verify "refs/tags/$TAG_NAME" >/dev/null; then
        return 0
    fi
    if [[ "$OPT_PUSH" -ne 1 ]]; then
        return 0
    fi
    if [[ "$OPT_FAKE_CONTAINERS" -eq 1 ]]; then
        fail "--push refuses to run with --fake-containers"
    fi

    heading "Push"
    local remote_url
    remote_url="$(git_repo remote get-url origin)"
    printf '    branch %s and tag %s\n' "$OPT_BRANCH" "$TAG_NAME"
    printf '    remote %s\n\n' "$remote_url"

    if ! confirm "    Push to the remote? This is publicly visible and hard to undo."; then
        note "not pushed"
        EXIT_STATUS=2
        return 0
    fi

    # Branch first: a tag pushed alone would reference a commit the remote does
    # not have.
    git_repo push origin "$OPT_BRANCH" || fail "pushing $OPT_BRANCH failed"
    git_repo push origin "$TAG_NAME"   || fail "pushing $TAG_NAME failed"
    pass "pushed $OPT_BRANCH and $TAG_NAME"
}

# ═══════════════════════════════════════════════════════════════════════════
# Phase 12 — AUR checksum and .SRCINFO
# ═══════════════════════════════════════════════════════════════════════════

phase_aur() {
    if ! git_repo rev-parse -q --verify "refs/tags/$TAG_NAME" >/dev/null; then
        return 0
    fi

    heading "AUR checksum"

    local url="$ARCHIVE_URL_BASE/$TAG_NAME.tar.gz"
    local archive="$WORK_DIR/$TAG_NAME.tar.gz"

    # The archive is generated on demand and does not exist until the tag is on
    # the remote, so a 404 here is expected rather than exceptional.
    if ! curl -fsSL --retry 3 --retry-delay 2 "$url" -o "$archive" 2>/dev/null; then
        warn "$url is not available yet"
        info "the tag has to be pushed before GitHub will serve its archive"
        info "resume with: packaging/build-release.sh --finish-aur --tag $TAG_NAME"
        info "packaging/aur/PKGBUILD still carries sha256sums=('SKIP')"
        EXIT_STATUS=2
        return 0
    fi

    local sum
    sum="$(sha256sum "$archive" | awk '{print $1}')"
    info "sha256 $sum"

    sed -i "s/^sha256sums=.*/sha256sums=('$sum')/" "$AUR_DIR/PKGBUILD"
    regenerate_srcinfo || fail "could not regenerate packaging/aur/.SRCINFO"

    bash "$SCRIPT_DIR/validate-metadata.sh" >/dev/null \
        || fail "validate-metadata.sh failed after the AUR update"

    if git_repo diff --quiet -- packaging/aur; then
        note "packaging/aur is already up to date"
        return 0
    fi

    git_repo add -- packaging/aur/PKGBUILD packaging/aur/.SRCINFO
    git_repo commit -q -m "packaging: set the AUR checksum for $NEW_VERSION"
    info "$(git_repo rev-parse --short HEAD)  packaging: set the AUR checksum for $NEW_VERSION"
    note "this commit is not pushed; push it with: git push origin $OPT_BRANCH"
}

finish_aur_only() {
    TAG_NAME="${OPT_TAG_NAME:-}"
    [[ -n "$TAG_NAME" ]] || fail "--finish-aur needs --tag vX.Y.Z"
    NEW_VERSION="${TAG_NAME#v}"
    semver_valid "$NEW_VERSION" || fail "--tag: '$TAG_NAME' is not vX.Y.Z"

    git_repo rev-parse -q --verify "refs/tags/$TAG_NAME" >/dev/null \
        || fail "$TAG_NAME does not exist locally"

    WORK_DIR="$(mktemp -d)"
    phase_aur
    exit "$EXIT_STATUS"
}

# ═══════════════════════════════════════════════════════════════════════════
# Phase 13 — summary
# ═══════════════════════════════════════════════════════════════════════════

phase_summary() {
    local arch name
    arch="$(uname -m)"

    printf '\n'
    rule
    printf '  %s %s  -  release build\n' "$APP_NAME" "$NEW_VERSION"
    rule

    printf '\n  artifact                                           size  state\n'
    while IFS= read -r name; do
        if [[ -f "$DIST_ROOT/$name" ]]; then
            local state="ok"
            list_has "$name" "${FAKED[@]+"${FAKED[@]}"}" && state="FAKE"
            # du renders sizes through LC_NUMERIC, which yields "4,0K" here.
            printf '  %-46s %8s  %s\n' "$name" "$(LC_ALL=C du -h "$DIST_ROOT/$name" | cut -f1)" "$state"
        else
            printf '  %-46s %8s  %s\n' "$name" "-" "missing"
        fi
    done < <(expected_artifacts)

    local t
    for t in "${SKIPPED[@]+"${SKIPPED[@]}"}"; do
        printf '  %-46s %8s  %s\n' "$(target_label "$t")" "-" "skipped"
    done
    if [[ -f "$DIST_ROOT/SHA256SUMS.txt" ]]; then
        printf '  %-46s %8s  %s\n' "SHA256SUMS.txt" \
            "$(grep -c . "$DIST_ROOT/SHA256SUMS.txt" || true) rows" "ok"
    fi
    if [[ -f "$DIST_ROOT/SHA256SUMS.txt.minisig" ]]; then
        printf '  %-46s %8s  %s\n' "SHA256SUMS.txt.minisig" \
            "$(stat -c%s "$DIST_ROOT/SHA256SUMS.txt.minisig") B" "ok"
    fi

    # Everything below hangs off the tag: the AUR archive URL, the push, and the
    # release itself. Without one there is nothing here a reader could run.
    local have_tag=0
    git_repo rev-parse -q --verify "refs/tags/$TAG_NAME" >/dev/null && have_tag=1

    local -a notes=()
    if [[ "${#SKIPPED[@]}" -gt 0 ]]; then
        notes+=("$(printf '%d target(s) skipped. No tag was created - a partial artifact set is not a release.' "${#SKIPPED[@]}")")
    fi
    if [[ "$have_tag" -eq 1 ]] && grep -q "sha256sums=('SKIP')" "$AUR_DIR/PKGBUILD" 2>/dev/null; then
        notes+=("$(printf 'AUR sha256 PENDING - needs the pushed tag: build-release.sh --finish-aur --tag %s' "$TAG_NAME")")
    fi
    if [[ "${#notes[@]}" -gt 0 ]]; then
        printf '\n'
        local n
        for n in "${notes[@]}"; do printf '  %s\n' "$n"; done
    fi

    printf '\n  manifest   dist/RELEASE-MANIFEST.txt\n'

    if [[ "$have_tag" -eq 0 ]]; then
        printf '\n  Not publishable: %s does not exist, so these artifacts belong to no release.\n' "$TAG_NAME"
        printf '  Re-run without --only/--skip once every target can build.\n'
        printf '\n  exit %d\n\n' "$EXIT_STATUS"
        return
    fi

    printf '\n  To publish this release:\n'
    if ! git_repo ls-remote --exit-code --tags origin "$TAG_NAME" >/dev/null 2>&1; then
        printf '    git push origin %s\n' "$OPT_BRANCH"
        printf '    git push origin %s\n' "$TAG_NAME"
    fi
    printf '    gh release create %s \\\n' "$TAG_NAME"
    printf '      --title "%s %s" \\\n' "$APP_NAME" "$NEW_VERSION"
    printf '      --notes-file <release notes> \\\n'
    while IFS= read -r name; do
        [[ -f "$DIST_ROOT/$name" ]] && printf '      dist/%s \\\n' "$name"
    done < <(expected_artifacts)
    printf '      dist/SHA256SUMS.txt \\\n'
    printf '      dist/SHA256SUMS.txt.minisig\n'

    # The AUR lives in its own git remote, so nothing this script does reaches
    # it. It is still part of the release; see packaging/aur/README.md.
    printf '\n  Then the AUR (separate repository, after the assets are live):\n'
    printf '    build-release.sh --finish-aur --tag %s      # real sha256, .SRCINFO\n' "$TAG_NAME"
    printf '    cp packaging/aur/{PKGBUILD,.SRCINFO,linux-soundboard.install} <aur clone>/\n'
    printf '    makepkg --cleanbuild --syncdeps && namcap PKGBUILD *.pkg.tar.zst\n'
    printf '    git -C <aur clone> commit -am %s && git -C <aur clone> push\n' "$NEW_VERSION"

    printf '\n  exit %d\n\n' "$EXIT_STATUS"
}

# ═══════════════════════════════════════════════════════════════════════════
# --undo and --self-test
# ═══════════════════════════════════════════════════════════════════════════

do_undo() {
    local head tag_version
    head="$(git_repo rev-parse --short HEAD)"

    # reset --hard reaches the whole tree, not just the release commit.
    tree_is_clean \
        || fail "working tree has uncommitted changes and --undo runs 'git reset --hard', which would discard them. Commit or stash first."

    local subject
    subject="$(git_repo log -1 --pretty=%s)"
    case "$subject" in
        "release: bump to "*|"packaging: set the AUR checksum for "*) : ;;
        *) fail "HEAD ($head: $subject) is not a release commit; refusing to undo" ;;
    esac

    tag_version="${subject##* }"
    local tag="v$tag_version"

    printf '  HEAD    %s  %s\n' "$head" "$subject"
    if git_repo rev-parse -q --verify "refs/tags/$tag" >/dev/null; then
        if git_repo ls-remote --exit-code --tags origin "$tag" >/dev/null 2>&1; then
            fail "$tag is already on the remote; it cannot be undone from here"
        fi
        printf '  tag     %s (local only)\n' "$tag"
    fi
    printf '\n  This runs: git reset --hard HEAD~1, and deletes %s if it exists.\n' "$tag"
    confirm "  Undo the release commit?" || abort_no_writes

    git_repo rev-parse -q --verify "refs/tags/$tag" >/dev/null && git_repo tag -d "$tag" >/dev/null
    git_repo reset --hard HEAD~1
    pass "undone"
    exit 0
}

do_self_test() {
    SELF_TEST_DIR="$(mktemp -d)"

    local clone="$SELF_TEST_DIR/repo"

    heading "Self test"
    info "cloning into $clone"
    git clone -q "$REPO_ROOT" "$clone"

    # The clone carries only committed files, so the working tree is laid over
    # it: the test then covers what a release of the tree as it stands now would
    # do, including files that are still uncommitted here.
    tar -C "$REPO_ROOT" --exclude=./.git --exclude=./target --exclude=./dist -cf - . \
        | tar -C "$clone" -xf -
    # The scratch clone needs its own identity: the real repository may rely on
    # one this test must not depend on or inherit.
    git -C "$clone" config user.name "release self-test"
    git -C "$clone" config user.email "self-test@localhost"
    minisign -G -W -f -p "$clone/release.pub" -s "$SELF_TEST_DIR/release.key" >/dev/null
    git -C "$clone" add -A
    git -C "$clone" commit -q -m "self-test baseline" 2>/dev/null || true

    # Everything runs for real on the throwaway copy; this repository is never
    # touched. Artifacts are fabricated so the run does not wait on a full build.
    local rc=0
    ( cd "$clone" && LSB_RELEASE_SIGNING_KEY="$SELF_TEST_DIR/release.key" \
        bash packaging/build-release.sh \
        --bump patch --yes --no-review --summaries-auto \
        --fake-containers --skip-tests ) || rc=$?

    # 2 means "completed with something outstanding" - here, the AUR hash that
    # cannot exist for an unpushed tag. That is the expected outcome.
    [[ "$rc" -eq 0 || "$rc" -eq 2 ]] || fail "self test failed (exit $rc)"

    heading "Self test result"
    git -C "$SELF_TEST_DIR/repo" --no-pager show --stat HEAD
    printf '\n'
    git -C "$SELF_TEST_DIR/repo" tag --list 'v*' | tail -n 3 | sed 's/^/    tag /'
    pass "self test completed; the clone is discarded"
    exit 0
}

# ═══════════════════════════════════════════════════════════════════════════
# Arguments
# ═══════════════════════════════════════════════════════════════════════════

usage() {
    cat <<EOF
$APP_NAME release builder

Usage: packaging/build-release.sh [options]

Release information (each prompt has a flag and an env twin)
  --version X.Y.Z            LSB_RELEASE_VERSION
  --bump patch|minor|major   LSB_RELEASE_BUMP
  --date YYYY-MM-DD          LSB_RELEASE_DATE          default: today
  --pkgrel N                 LSB_RELEASE_PKGREL        default: 1
  --notes-file PATH          LSB_RELEASE_NOTES_FILE
  --summaries-file PATH      LSB_RELEASE_SUMMARIES_FILE
  --summaries-auto           LSB_RELEASE_SUMMARIES_AUTO
  --summary-max N            LSB_RELEASE_SUMMARY_MAX   default: 5
  --maintainer "N <e>"       DEBFULLNAME + DEBEMAIL

Targets
  --only LIST                tarball,deb,appimage,rpm,flatpak
  --skip LIST                same vocabulary, subtractive
  --with-flatpak             opt in; skipped when flatpak-builder is absent
  --partial-ok               do not prompt about unavailable targets

Git and outward-facing steps
  --no-commit                stage the bump and stop
  --no-tag                   build and verify only
  --retag                    move an existing unpushed tag
  --force-tag                tag despite skipped targets
  --push                     arm the push; still confirmed
  --branch NAME              default: main
  --allow-dirty              implies --no-tag unless --tag-dirty
  --tag-dirty                tag from a dirty tree
  --undo                     unwind the release commit and tag

Behaviour
  --dry-run                  apply the bump, show the diff, roll it back
  -y, --yes                  take every default; never implies --push
  --no-review                skip the diff prompt
  --skip-tests               drop cargo fmt, clippy and test
  --smoke-fix-systemd        install the built binary so systemd-analyze passes
  --keep-dist                do not sweep stale artifacts out of dist/
  --clean-dist               delete stale artifacts instead of quarantining
  --finish-aur --tag vX.Y.Z  run the AUR checksum phase alone
  --prune-legacy-deb-control remove packaging/deb/control in the release commit
  --fake-containers          fabricate every artifact to exercise the flow without
                             running the real packagers
  --self-test                run the whole pipeline against a scratch clone
  -h, --help

Signing
  LSB_RELEASE_SIGNING_KEY    private minisign key; required except for --dry-run

Exit codes
  0  every requested phase completed
  1  hard failure
  2  completed with skipped targets, an unpushed tag, or a pending AUR hash
  3  aborted at a prompt
EOF
}

parse_args() {
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --version)            OPT_VERSION="${2:?--version needs X.Y.Z}"; shift 2 ;;
            --bump)               OPT_BUMP="${2:?--bump needs patch|minor|major}"; shift 2 ;;
            --date)               OPT_DATE="${2:?--date needs YYYY-MM-DD}"; shift 2 ;;
            --pkgrel)             OPT_PKGREL="${2:?--pkgrel needs a number}"; shift 2 ;;
            --notes-file)         OPT_NOTES_FILE="${2:?--notes-file needs a path}"; shift 2 ;;
            --summaries-file)     OPT_SUMMARIES_FILE="${2:?--summaries-file needs a path}"; shift 2 ;;
            --summaries-auto)     OPT_SUMMARIES_AUTO=1; shift ;;
            --summary-max)        OPT_SUMMARY_MAX="${2:?--summary-max needs a number}"; shift 2 ;;
            --maintainer)         OPT_MAINTAINER="${2:?--maintainer needs a string}"; shift 2 ;;
            --only)               OPT_ONLY="${2:?--only needs a list}"; shift 2 ;;
            --skip)               OPT_SKIP="${2:?--skip needs a list}"; shift 2 ;;
            --with-flatpak)       OPT_FLATPAK=1; shift ;;
            --partial-ok)         OPT_PARTIAL_OK=1; shift ;;
            --no-commit)          OPT_NO_COMMIT=1; shift ;;
            --no-tag)             OPT_NO_TAG=1; shift ;;
            --retag)              OPT_RETAG=1; shift ;;
            --force-tag)          OPT_FORCE_TAG=1; shift ;;
            --push)               OPT_PUSH=1; shift ;;
            --branch)             OPT_BRANCH="${2:?--branch needs a name}"; shift 2 ;;
            --allow-dirty)        OPT_ALLOW_DIRTY=1; shift ;;
            --tag-dirty)          OPT_TAG_DIRTY=1; shift ;;
            --undo)               OPT_UNDO=1; shift ;;
            --dry-run)            OPT_DRY_RUN=1; shift ;;
            -y|--yes)             OPT_YES=1; shift ;;
            --no-review)          OPT_NO_REVIEW=1; shift ;;
            --skip-tests)         OPT_SKIP_TESTS=1; shift ;;
            --smoke-fix-systemd)  OPT_SMOKE_FIX_SYSTEMD=1; shift ;;
            --keep-dist)          OPT_KEEP_DIST=1; shift ;;
            --clean-dist)         OPT_CLEAN_DIST=1; shift ;;
            --finish-aur)         OPT_FINISH_AUR=1; shift ;;
            --tag)                OPT_TAG_NAME="${2:?--tag needs vX.Y.Z}"; shift 2 ;;
            --prune-legacy-deb-control) OPT_PRUNE_LEGACY=1; shift ;;
            --fake-containers)    OPT_FAKE_CONTAINERS=1; shift ;;
            --self-test)          OPT_SELF_TEST=1; shift ;;
            -h|--help)            usage; exit 0 ;;
            *)                    printf 'Unknown argument: %s\n\n' "$1" >&2; usage >&2; exit 1 ;;
        esac
    done

    [[ -n "$OPT_VERSION" && -n "$OPT_BUMP" ]] && fail "--version and --bump are mutually exclusive"
    [[ "$OPT_KEEP_DIST" -eq 1 && "$OPT_CLEAN_DIST" -eq 1 ]] && fail "--keep-dist and --clean-dist are mutually exclusive"
    return 0
}

# ═══════════════════════════════════════════════════════════════════════════

main() {
    parse_args "$@"
    trap on_exit EXIT

    [[ "$OPT_SELF_TEST" -eq 1 ]] && do_self_test
    [[ "$OPT_UNDO" -eq 1 ]] && do_undo
    [[ "$OPT_FINISH_AUR" -eq 1 ]] && finish_aur_only

    WORK_DIR="$(mktemp -d)"

    phase_preflight
    phase_collect
    phase_confirm
    phase_bump

    if [[ "$OPT_DRY_RUN" -eq 1 ]]; then
        phase_review
        heading "Dry run"
        note "rolling back the bump; nothing is kept"
        rollback_bump
        exit 0
    fi

    phase_review
    phase_commit
    phase_validate
    phase_build
    phase_checksums
    phase_verify
    phase_tag
    phase_push
    phase_aur
    phase_summary

    exit "$EXIT_STATUS"
}

main "$@"
