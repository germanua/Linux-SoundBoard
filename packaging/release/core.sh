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



replace_file() {
    local target="$1" source="$2"
    cat "$source" >"$target"
    rm -f "$source"
}




bump_tmp() {
    printf '%s/%s.bump' "$WORK_DIR" "${1##*/}"
}



count_occurrences() {
    local file="$1" pattern="$2"
    { grep -oF -- "$pattern" "$file" 2>/dev/null || true; } | wc -l | tr -d ' '
}







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





detect_container_runtime() {


    command -v docker >/dev/null 2>&1 && command -v rsync >/dev/null 2>&1
}

target_is_available() {
    case "$1" in
        appimage)      [[ "$OPT_FAKE_CONTAINERS" -eq 1 ]] || detect_container_runtime ;;
        tarball)       command -v cargo >/dev/null 2>&1 ;;
        debappimage)   [[ "$OPT_FAKE_CONTAINERS" -eq 1 ]] || detect_container_runtime ;;
        rpm)           [[ "$OPT_FAKE_CONTAINERS" -eq 1 ]] || detect_container_runtime ;;
        flatpak)       command -v flatpak-builder >/dev/null 2>&1 ;;
        *)             return 1 ;;
    esac
}

target_requirement() {
    case "$1" in
        appimage)      printf 'docker + rsync (ubuntu:24.04)' ;;
        tarball)       printf 'host cargo (legacy target)' ;;
        debappimage)   printf 'docker + rsync (legacy deb + AppImage target)' ;;
        rpm)           printf 'docker + rsync (legacy rpm target)' ;;
        flatpak)       printf 'flatpak-builder' ;;
    esac
}

target_label() {
    case "$1" in
        appimage)      printf 'AppImage' ;;
        tarball)       printf 'tarball (legacy)' ;;
        debappimage)   printf 'deb + AppImage (legacy)' ;;
        rpm)           printf 'rpm (legacy)' ;;
        flatpak)       printf 'flatpak' ;;
    esac
}

resolve_targets() {
    local requested=(appimage)

    if [[ -n "$OPT_ONLY" ]]; then
        requested=()
        local name
        while IFS= read -r name; do
            case "$name" in
                appimage)          requested+=(appimage) ;;
                tarball)           requested+=(tarball) ;;
                deb)               requested+=(debappimage) ;;
                rpm)               requested+=(rpm) ;;
                flatpak)           requested+=(flatpak) ;;
                checksums)         : ;;
                '')                : ;;
                *)                 fail "--only: unknown target '$name'" ;;
            esac
        done < <(tr ',' '\n' <<<"$OPT_ONLY")
    fi

    if [[ -n "$OPT_SKIP" ]]; then
        local name kept=() t
        local -a drop=()
        while IFS= read -r name; do
            case "$name" in
                appimage|tarball|deb|rpm|flatpak) drop+=("$name") ;;
                '') : ;;
                *) fail "--skip: unknown target '$name'" ;;
            esac
        done < <(tr ',' '\n' <<<"$OPT_SKIP")

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


    if [[ -n "$OPT_ONLY" || -n "$OPT_SKIP" ]]; then
        NARROWED=1
    fi
}

public_release_tree_is_clean() {
    local path
    for path in \
        dev \
        .github/workflows/private-dev.yml \
        bootstrap-install-dev.sh \
        install-dev.sh \
        dev-release-keyring.txt \
        dev-release.pub \
        packaging/build-dev-release.sh \
        packaging/tests/dev-profile-isolation-test.sh \
        packaging/tests/dev-release-container-test.sh \
        dist-dev \
        .commandcode; do
        [[ ! -e "$REPO_ROOT/$path" ]] || fail "DEV-only path is present in the public release tree: $path"
    done
}
