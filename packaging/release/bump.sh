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





bump_doc_filenames() {
    local rel_path="$1"; shift
    local file="$REPO_ROOT/$rel_path"
    local pattern replacement expected found



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
    local old_appimage="$APP_BINARY-$OLD_VERSION-x86_64.AppImage"
    local new_appimage="$APP_BINARY-$NEW_VERSION-x86_64.AppImage"
    local old_deb="${APP_BINARY}_${OLD_VERSION}-${OLD_PKGREL}_amd64.deb"
    local new_deb="${APP_BINARY}_${NEW_VERSION}-${PKGREL}_amd64.deb"
    local old_rpm="$APP_BINARY-$OLD_VERSION-$OLD_PKGREL.x86_64.rpm"
    local new_rpm="$APP_BINARY-$NEW_VERSION-$PKGREL.x86_64.rpm"

    bump_doc_filenames README.md \
        "$old_appimage" "$new_appimage" 3

    bump_doc_filenames docs/INSTALL.md \
        "$old_appimage" "$new_appimage" 3

    bump_doc_filenames docs/INSTALL.md \
        "$old_deb" "$new_deb" 0 \
        "$old_rpm" "$new_rpm" 0

    bump_doc_filenames docs/TROUBLESHOOTING.md \
        "$old_appimage" "$new_appimage" 0 \
        "$old_deb" "$new_deb" 0 \
        "$old_rpm" "$new_rpm" 0
}

bump_legacy_deb_control() {
    [[ -f "$LEGACY_DEB_CONTROL" ]] || return 0

    if [[ "$OPT_PRUNE_LEGACY" -eq 1 ]]; then
        git_repo rm -q -- packaging/deb/control
        track_bumped packaging/deb/control
        note "removed packaging/deb/control (legacy, unused by any build)"
        return 0
    fi



    sed -i "s/^Version: .*/Version: $NEW_VERSION/" "$LEGACY_DEB_CONTROL"
    track_bumped packaging/deb/control
}

bump_already_applied() {
    local cargo_v
    cargo_v="$(cargo_version_from_manifest "$MANIFEST_PATH" 2>/dev/null || printf '')"
    [[ "$cargo_v" == "$NEW_VERSION" ]]
}

phase_bump() {
    if [[ "$BUMP_ALREADY_APPLIED" -eq 1 ]]; then
        heading "Bump"
        skip "already applied"
        return
    fi

    heading "Bump"
    BUMP_IN_PROGRESS=1

    bump_cargo_toml ; info "src/Cargo.toml"
    bump_cargo_lock ; info "Cargo.lock"
    bump_metainfo   ; info "packaging/flatpak/$APP_ID.metainfo.xml"
    bump_changelog  ; info "docs/CHANGELOG.md"
    bump_docs       ; info "docs/INSTALL.md, docs/TROUBLESHOOTING.md"

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

        BUMP_IN_PROGRESS=0
        note "--no-commit: the bump is staged but not committed"
        note "a tag must point at a commit containing the bump, so later phases are refused"
        exit 0
    fi

    heading "Release commit"
    git_repo commit -q -m "release: bump to $NEW_VERSION"


    BUMP_IN_PROGRESS=0

    info "$(git_repo rev-parse --short HEAD)  release: bump to $NEW_VERSION"
}
