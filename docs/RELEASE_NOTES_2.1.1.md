# Linux Soundboard 2.1.1

Version 2.1.1 is a safe-upgrade correction for the published 2.1.0 release.

- Stable installs restart a stale engine once and reconnect only when protocol 1, schema 7, and app version 2.1.1 all match.
- Closing the GUI leaves an installed service and virtual microphone running.
- Temporary AppImage and development runs restore an eligible previous/default microphone before removing their virtual source.
- A first direct AppImage execution requires an explicit Install, Run temporarily, or Exit choice. Opening a newer AppImage after installation updates the stable user copy and engine automatically, with no terminal or service-management steps.
- Schema-6 configuration migration creates an exact private `config.json.pre-v6-backup`; malformed, future, or conflicting configuration fails closed.
- Arch-family users receive the tagged `linux-soundboard` AUR package; `linux-soundboard-git` remains a development package.

The 2.1.0 tag and artifacts remain unchanged. Do not publish 2.1.1 until the documented package-upgrade and live capture acceptance tests pass. Publish the stable AUR update only after the GitHub tag and release assets are live and verified.
