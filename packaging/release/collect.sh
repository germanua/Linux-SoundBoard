phase_preflight() {
    heading "Preflight"
    public_release_tree_is_clean

    git_repo rev-parse --git-dir >/dev/null 2>&1 || fail "not a git repository: $REPO_ROOT"

    local branch
    branch="$(git_repo rev-parse --abbrev-ref HEAD)"
    [[ "$branch" != "HEAD" ]] || fail "HEAD is detached; check out $OPT_BRANCH first"
    if [[ "$branch" != "$OPT_BRANCH" ]]; then
        fail "on branch '$branch', expected '$OPT_BRANCH' (override with --branch)"
    fi

    local tool
    for tool in git cargo sed awk curl tar grep; do
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
    [[ -r "$REPO_ROOT/release-keyring.txt" ]] || fail "release-keyring.txt is missing or unreadable"
    local active_release_key
    active_release_key="$(tail -n 1 "$REPO_ROOT/release.pub" 2>/dev/null || true)"
    [[ -n "$active_release_key" ]] || fail "release.pub does not contain a public key"
    grep -Fxq "$active_release_key" "$REPO_ROOT/release-keyring.txt" \
        || fail "the active release.pub key is missing from release-keyring.txt"

    local script
    for script in \
        "$SCRIPT_DIR/linux/package-tarball.sh" \
        "$SCRIPT_DIR/docker/build-appimage.sh" \
        "$SCRIPT_DIR/docker/build-deb-appimage.sh" \
        "$SCRIPT_DIR/docker/build-rpm.sh" \
        "$SCRIPT_DIR/generate-checksums.sh" \
        "$SCRIPT_DIR/validate-metadata.sh" \
        "$SCRIPT_DIR/smoke-check.sh"
    do
        [[ -f "$script" ]] || fail "missing packaging script: ${script#"$REPO_ROOT"/}"
    done




    if ! tree_is_clean; then
        if [[ "$OPT_ALLOW_DIRTY" -ne 1 ]]; then
            printf '\n' >&2
            git_repo status --short >&2
            printf '\n' >&2
            fail "working tree is dirty. Artifacts build from the tree but the tag names HEAD, so the release would not be reproducible from its own tag. Commit first, or pass --allow-dirty (which implies --no-tag)."
        fi
        warn "working tree is dirty; artifacts will not match the tagged tree"


        mapfile -t DIRTY_AT_START < <(git_repo status --porcelain | sed 's/^...//')
        if [[ "$OPT_TAG_DIRTY" -ne 1 ]]; then
            OPT_NO_TAG=1
            note "--allow-dirty implies --no-tag (override with --tag-dirty)"
        fi
    fi



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








notes_source() {
    if [[ -n "$OPT_NOTES_FILE" ]]; then

        printf '## [Unreleased]\n'
        cat "$OPT_NOTES_FILE"
        printf '\n## [end]\n'
    else
        cat "$CHANGELOG"
    fi
}


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


                idx = index(line, ". ")
                lead = (idx > 0) ? substr(line, 1, idx - 1) : line
                sub(/\.$/, "", lead)
            }
            printf "%s\t%s\n", (section == "" ? "Changed" : section), lead
        }
    '
}



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



    DATE_ISO="$(LC_ALL=C date -d "$RELEASE_DATE" +%F)" \
        || fail "unparsable date: $RELEASE_DATE"
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


        sed -i "s/^headline: EDIT-ME$/headline: $APP_NAME $NEW_VERSION:/" "$SUMMARY_FILE"
        summaries_valid "$SUMMARY_FILE" || fail "generated draft is not usable"
        note "using the mechanical draft verbatim (--summaries-auto)"
        return
    fi

    printf '\n[5/6]  Package summaries\n'
    printf '       The .deb, .rpm and AppStream entries carry short summaries, not the\n'
    printf '       full CHANGELOG. Capped at %s lines (--summary-max).\n\n' "$OPT_SUMMARY_MAX"
    printf '       The draft is mechanical. It needs your voice.\n\n'


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
        "README.md|versioned AppImage filename" \
        "docs/INSTALL.md|versioned AppImage filename" \
        "docs/TROUBLESHOOTING.md|assert no versioned filenames"
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
