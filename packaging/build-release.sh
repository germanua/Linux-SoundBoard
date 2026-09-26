#!/usr/bin/env bash



















set -euo pipefail

RELEASE_SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"


source "$RELEASE_SCRIPT_DIR/common.sh"


source "$RELEASE_SCRIPT_DIR/linux/app-meta.sh"
SCRIPT_DIR="$RELEASE_SCRIPT_DIR"

REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"
MANIFEST_PATH="$REPO_ROOT/src/Cargo.toml"
DIST_ROOT="$REPO_ROOT/dist"





SMOKE_KNOWN_FAIL='systemd-analyze verify: engine service and target'
SYSTEMD_DEFAULT_PATH=(/usr/local/sbin /usr/local/bin /usr/sbin /usr/bin)

AUR_DIR="$REPO_ROOT/packaging/aur"
CHANGELOG="$REPO_ROOT/docs/CHANGELOG.md"
DEBIAN_CHANGELOG="$REPO_ROOT/packaging/debian/changelog"
SPEC="$REPO_ROOT/packaging/rpm/linux-soundboard.spec"
METAINFO="$REPO_ROOT/packaging/flatpak/$APP_ID.metainfo.xml"
LEGACY_DEB_CONTROL="$REPO_ROOT/packaging/deb/control"



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




source "$SCRIPT_DIR/release/core.sh"
source "$SCRIPT_DIR/release/collect.sh"
source "$SCRIPT_DIR/release/bump.sh"
source "$SCRIPT_DIR/release/validate.sh"
source "$SCRIPT_DIR/release/verify.sh"
source "$SCRIPT_DIR/release/finish.sh"

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
