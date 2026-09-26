phase_aur() {
    note "AUR publication skipped: current release policy is AppImage-only"
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



    local have_tag=0
    git_repo rev-parse -q --verify "refs/tags/$TAG_NAME" >/dev/null && have_tag=1

    local -a notes=()
    if [[ "${#SKIPPED[@]}" -gt 0 ]]; then
        notes+=("$(printf '%d target(s) skipped. No tag was created - a partial artifact set is not a release.' "${#SKIPPED[@]}")")
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

    printf '\n  Release policy: AppImage only; do not publish AUR/DEB/RPM/tarball artifacts.\n'

    printf '\n  exit %d\n\n' "$EXIT_STATUS"
}





do_undo() {
    local head tag_version
    head="$(git_repo rev-parse --short HEAD)"


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
    local notes_file="$SELF_TEST_DIR/notes.md"

    heading "Self test"
    info "cloning into $clone"
    git clone -q "$REPO_ROOT" "$clone"




    tar -C "$REPO_ROOT" --exclude=./.git --exclude=./target --exclude=./dist -cf - . \
        | tar -C "$clone" -xf -


    git -C "$clone" config user.name "release self-test"
    git -C "$clone" config user.email "self-test@localhost"
    minisign -G -W -f -p "$clone/release.pub" -s "$SELF_TEST_DIR/release.key" >/dev/null
    tail -n 1 "$clone/release.pub" >"$clone/release-keyring.txt"
    sed -i "s|^MINISIGN_PUBLIC_KEY=.*|MINISIGN_PUBLIC_KEY=\"$(tail -n 1 "$clone/release.pub")\"|" \
        "$clone/install.sh"
    printf '%s\n' '### Fixed' '- Exercise the release pipeline.' >"$notes_file"
    git -C "$clone" add -A
    git -C "$clone" commit -q -m "self-test baseline" 2>/dev/null || true



    local rc=0
    local self_test_branch
    self_test_branch="$(git -C "$clone" branch --show-current)"
    ( cd "$clone" && LSB_RELEASE_SIGNING_KEY="$SELF_TEST_DIR/release.key" \
        bash packaging/build-release.sh \
        --bump patch --yes --no-review --summaries-auto \
        --notes-file "$notes_file" --branch "$self_test_branch" \
        --fake-containers --skip-tests ) || rc=$?



    [[ "$rc" -eq 0 || "$rc" -eq 2 ]] || fail "self test failed (exit $rc)"

    heading "Self test result"
    git -C "$SELF_TEST_DIR/repo" --no-pager show --stat HEAD
    printf '\n'
    git -C "$SELF_TEST_DIR/repo" tag --list 'v*' | tail -n 3 | sed 's/^/    tag /'
    pass "self test completed; the clone is discarded"
    exit 0
}





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
  default                    AppImage only (current release policy)
  --only LIST                maintenance override: appimage,tarball,deb,rpm,flatpak
  --skip LIST                same vocabulary, subtractive
  --with-flatpak             legacy/maintenance opt in
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
            --with-flatpak)       shift ;;
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
