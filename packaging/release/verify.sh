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
            appimage)    printf '%s\n' "$APP_BINARY-$NEW_VERSION-$arch.AppImage" "$APP_BINARY-$arch.AppImage" ;;
            tarball)     printf '%s\n' "$APP_BINARY-$NEW_VERSION-linux-$arch.tar.gz" ;;
            debappimage) printf '%s\n' \
                            "${APP_BINARY}_${NEW_VERSION}-${PKGREL}_amd64.deb" \
                            "$APP_BINARY-$NEW_VERSION-$arch.AppImage" \
                            "$APP_BINARY-$arch.AppImage" ;;
            rpm)         printf '%s\n' "$APP_BINARY-$NEW_VERSION-$PKGREL.x86_64.rpm" ;;
        esac
    done
    printf '%s\n' install.sh
    [[ -f "$DIST_ROOT/update.json" ]] && printf '%s\n' update.json
}

pinned_base_image() {
    local pinned
    pinned="$(sed -n -e 's/^UBUNTU_IMAGE_DIGEST="\(.*\)"$/\1/p' -e 's/^FEDORA_IMAGE_DIGEST="\(.*\)"$/\1/p' "$SCRIPT_DIR/docker/$1" | head -n 1)"
    if [[ -z "$pinned" ]]; then
        fail "could not read the pinned base image from packaging/docker/$1"
    fi
    printf '%s\n' "$pinned"
}

builder_for() {
    case "$1" in
        *.tar.gz)
            printf 'packaging/linux/package-tarball.sh (host)' ;;
        *.AppImage)
            printf 'packaging/docker/build-appimage.sh (%s)' \
                "${APPIMAGE_BUILD_IMAGE:-${DEB_BUILD_IMAGE:-$(pinned_base_image build-appimage.sh)}}" ;;
        update.json)
            printf 'packaging/generate-update-metadata.py' ;;
        *.deb)
            printf 'packaging/docker/build-deb-appimage.sh (%s)' \
                "${DEB_BUILD_IMAGE:-$(pinned_base_image build-deb-appimage.sh)}" ;;
        *.rpm)
            printf 'packaging/docker/build-rpm.sh (%s)' \
                "${RPM_BUILD_IMAGE:-$(pinned_base_image build-rpm.sh)}" ;;
        *)
            printf 'unknown' ;;
    esac
}

min_size_for() {


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


        if [[ "$name" != "install.sh" && "$name" != "update.json" && "$name" != "$APP_BINARY-$(uname -m).AppImage" && "$name" != *"$NEW_VERSION"* ]]; then
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



    local tarball
    tarball="$DIST_ROOT/$APP_BINARY-$NEW_VERSION-linux-$(uname -m).tar.gz"
    if [[ -f "$tarball" ]] && ! list_has "$(basename "$tarball")" "${FAKED[@]+"${FAKED[@]}"}"; then
        local top

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


    local sums="$DIST_ROOT/SHA256SUMS.txt"
    [[ -f "$sums" ]] || fail "SHA256SUMS.txt is missing; install.sh cannot verify downloads without it"



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



    git_repo push origin "$OPT_BRANCH" || fail "pushing $OPT_BRANCH failed"
    git_repo push origin "$TAG_NAME"   || fail "pushing $TAG_NAME failed"
    pass "pushed $OPT_BRANCH and $TAG_NAME"
}
