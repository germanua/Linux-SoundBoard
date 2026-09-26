systemd_default_path_has_binary() {
    local dir
    for dir in "${SYSTEMD_DEFAULT_PATH[@]}"; do
        [[ -x "$dir/$APP_BINARY" ]] && return 0
    done
    return 1
}



smoke_failure_is_known() {
    local line="$1"
    [[ "$line" == "$SMOKE_KNOWN_FAIL" ]] || return 1
    systemd_default_path_has_binary && return 1
    return 0
}

install_systemd_stub() {
    local built="$REPO_ROOT/target/release/$APP_BINARY"
    [[ -x "$built" ]] || { warn "--smoke-fix-systemd: no built binary at target/release/$APP_BINARY yet"; return 1; }


    if [[ -e "/usr/local/bin/$APP_BINARY" ]]; then
        warn "--smoke-fix-systemd: /usr/local/bin/$APP_BINARY already exists; leaving it untouched"
        return 1
    fi
    info "installing $APP_BINARY to /usr/local/bin so systemd-analyze can resolve ExecStart"
    sudo install -Dm755 "$built" "/usr/local/bin/$APP_BINARY" || return 1
    return 0
}



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
        appimage)
            cp /bin/true "$DIST_ROOT/$APP_BINARY-$NEW_VERSION-$arch.AppImage"
            cp /bin/true "$DIST_ROOT/$APP_BINARY-$arch.AppImage"
            FAKED+=("$APP_BINARY-$NEW_VERSION-$arch.AppImage" "$APP_BINARY-$arch.AppImage")
            ;;
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


            printf 'fake rpm\n' >"$DIST_ROOT/$APP_BINARY-$NEW_VERSION-$PKGREL.x86_64.rpm"
            FAKED+=("$APP_BINARY-$NEW_VERSION-$PKGREL.x86_64.rpm")
            ;;
        flatpak) : ;;
    esac

    if [[ "$target" != "flatpak" ]]; then
        bash "$SCRIPT_DIR/generate-checksums.sh" "$DIST_ROOT" >/dev/null || return 1
    fi
}

run_target() {
    local target="$1"

    if [[ "$OPT_FAKE_CONTAINERS" -eq 1 ]]; then
        warn "$(target_label "$target"): fabricating artifacts (--fake-containers)"
        fake_artifacts_for "$target" || return 1
        BUILT+=("$target")
        return 0
    fi

    case "$target" in
        appimage)
            bash "$SCRIPT_DIR/docker/build-appimage.sh" || return 1
            ;;
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


    unset LSB_RELEASE_SIGNING_KEY LSB_RELEASE_PUBLIC_KEY LSB_RELEASE_TAG
    sweep_dist
    mkdir -p "$DIST_ROOT"

    local target rc
    for target in "${TARGETS[@]}"; do
        if ! target_is_available "$target"; then


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

            fail "$(target_label "$target") build failed (exit $rc)"
        fi
    done

    install -m755 "$REPO_ROOT/install.sh" "$DIST_ROOT/install.sh"
    local update_image
    update_image="$DIST_ROOT/$APP_BINARY-$NEW_VERSION-$(uname -m).AppImage"
    if [[ -f "$update_image" ]]; then
        python3 "$SCRIPT_DIR/generate-update-metadata.py" \
            --version "$NEW_VERSION" \
            --tag "$TAG_NAME" \
            --published-at "${DATE_ISO}T00:00:00Z" \
            --appimage "$update_image" \
            --summary-file "$SUMMARY_FILE" \
            --output "$DIST_ROOT/update.json"
    fi
    [[ "${#SKIPPED[@]}" -gt 0 ]] && EXIT_STATUS=2
    return 0
}
