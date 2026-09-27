# Linux Soundboard documentation

[Project home](../README.md) · [Releases](https://github.com/germanua/Linux-SoundBoard/releases) · [Changelog](CHANGELOG.md)

Install the app, route your first sound, or find a specific control. These guides
describe the source revision you are reading; released builds may differ. Use the
docs and license accompanying your version when investigating older behavior.

## Start here

| Goal | Guide |
| --- | --- |
| Install, verify, update, or remove the app | [Installation](INSTALL.md) |
| Add sounds and send them to a call or game | [Getting started](GETTING_STARTED.md) |
| Look up a button, setting, or hotkey | [Feature reference](FEATURE_REFERENCE.md) |
| Fix startup, sound, routing, or hotkeys | [Troubleshooting](TROUBLESHOOTING.md) |
| Collect evidence for a reproducible problem | [Bug reports](BUG_REPORTS.md) |
| See the application | [Screenshot gallery](SCREENSHOTS.md) |
| Review changes between versions | [Changelog](CHANGELOG.md) |

## Contribute and understand permissions

| Topic | Reference |
| --- | --- |
| Submit a bug, patch, documentation change, or asset | [Contributing](../CONTRIBUTING.md) |
| Understand personal use, code reuse, and redistribution | [Legal overview](LEGAL.md) and [full license](../LICENSE) |
| Use the name, logo, or screenshots | [Brand and artwork policy](BRANDING.md) |
| Request business use or distribution rights | [Commercial licensing](../COMMERCIAL-LICENSE.md) |
| Check upstream libraries and icon licenses | [Third-party licenses](../THIRDPARTY_LICENSES.md) and [generated notices](../THIRD_PARTY_NOTICES.html) |

## Installation and profile conventions

Installation commands in this documentation target the official public repository
and the standard Linux Soundboard application profile.

Paths shown as `~/.config` or `~/.local/state` assume the default XDG locations.
If you override XDG directories, use your configured paths. A per-user AppImage
installation lives at `~/.local/opt/linux-soundboard/linux-soundboard`; it is not
guaranteed to add a `linux-soundboard` command to `PATH`.
