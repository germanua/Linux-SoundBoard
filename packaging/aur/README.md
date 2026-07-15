# Stable AUR release

`PKGBUILD` is the tagged-release package for normal users. Keep
`linux-soundboard-git` for development builds only.

For every accepted release:

1. Update `pkgver`, reset `pkgrel=1`, and keep the tag archive source.
2. Do not publish the AUR package until the matching GitHub tag and release assets are live.
3. In a separate clone of `aur.archlinux.org/linux-soundboard.git`, copy `PKGBUILD` and `linux-soundboard.install` from this directory.
4. Run `updpkgsums`, reject `SKIP`, and verify the downloaded tag archive.
5. Run `makepkg --cleanbuild --syncdeps`, `makepkg --printsrcinfo > .SRCINFO`, and `namcap` on both the recipe and built package.
6. Confirm the package version, installed binary version, desktop entry, user unit, legal notices, and engine handoff all match the accepted release commit.
7. Commit and push the AUR repository only after every check passes.

The in-repository `SKIP` is a pre-tag template value. It must never be copied to the public stable AUR repository.
