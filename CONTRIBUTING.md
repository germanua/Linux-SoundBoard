# Contributing

[Documentation](docs/README.md) · [License](LICENSE) · [Legal overview](docs/LEGAL.md)

Linux Soundboard is maintained by germanua. Start with a focused issue describing
a reproducible problem or proposed change. Acceptance depends on scope,
maintainability, provenance, and the project's licensing model.

## Choose the right contribution

- **Bug:** follow [Bug reports](docs/BUG_REPORTS.md) and include reproduction steps.
- **Documentation:** identify the incorrect section and the behavior it should describe.
- **Code or artwork:** discuss substantial changes before preparing a pull request.
- **Security-sensitive information:** avoid publishing credentials or private data;
  arrange an appropriate contact channel with the maintainer first.

Source is available for inspection, but this project does not use an open-source
license. The contribution exception permits preparing and testing patches for
the official project. It does not authorize a separate distributed application.

## Prepare a focused pull request

1. Use the [source-build guide](docs/INSTALL.md#build-from-source).
2. Keep unrelated changes out of the patch and describe the resulting behavior.
3. Update affected user documentation and the Unreleased changelog when relevant.
4. Run checks appropriate to the change. For Rust changes, use `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and
   `cargo test --workspace`. State what was and was not tested.
5. Identify third-party material and complete the contributor acknowledgements.

Do not commit private logs, credentials, user libraries, local configuration,
build output, or unrelated screenshots. Documentation changes should be checked
for valid links, accurate commands, and readable rendering.

## Contributor agreement

A **Contribution** is code, documentation, artwork, scripts, configuration, or
other copyrightable material you intentionally submit for inclusion in Linux
Soundboard. Material clearly marked **Not a Contribution** is excluded. Ordinary
bug reports and diagnostic material have the narrower permission below.

By deliberately submitting a Contribution under these terms and confirming the
agreement, you represent that you own it or have the necessary authority to
grant these rights, including any required employer or co-author authorization.
You identify third-party portions separately; this grant does not override their
licenses or transfer rights you do not hold.

You grant germanua and recipients authorized by germanua a perpetual, worldwide,
non-exclusive, irrevocable, royalty-free copyright license to use, reproduce,
modify, prepare derivative works from, publicly display, publicly perform,
distribute, sublicense, and relicense your Contribution in source or binary form
under any terms, including commercial and proprietary terms.

You also grant germanua and such authorized recipients a perpetual, worldwide,
non-exclusive, royalty-free patent license, with the right to sublicense, to
make, have made, use, sell, offer for sale, import, and otherwise transfer your
Contribution. It covers only claims you can license that are necessarily
infringed by your Contribution alone or its combination with the project to
which you submitted it. This patent grant is irrevocable except that it ends
for a recipient who brings patent litigation alleging that your Contribution
or that combination infringes a patent.

You retain ownership. This agreement is not an assignment of copyright and does
not cover your unrelated work. It permits inclusion in public source-available
releases, official paid builds, and separately licensed commercial releases.
No payment, acceptance, credit placement, or support obligation is promised by
submission alone. Mandatory moral rights remain unaffected; to the extent
permitted by law, you consent to adaptations needed to exercise this grant.

This agreement applies prospectively to contributions submitted with agreement
to these terms. It does not retroactively change earlier contributions. A
separately executed contributor agreement controls where it expressly differs.

## Third-party material

For every copied snippet, dependency, icon, font, sound, sample, or other external
asset, provide its source, version or revision, license, required notices, and
any modifications. Generated material also requires a review of provenance and
rights; the tool used does not by itself establish ownership or compatibility.

Do not submit material with unknown or incompatible terms. Maintainer approval
cannot waive an upstream owner's rights. Material under GPL, AGPL,
noncommercial, or no-derivatives terms needs specific compatibility review before
submission; permissive and weak-copyleft components still carry obligations.

## Record agreement before acceptance

Complete the [pull-request template](.github/pull_request_template.md). The
maintainer should retain the acknowledgement with the contribution and may
require a separately signed agreement. A checkbox is a record of the stated
agreement, not a substitute for verifying identity or authority when in doubt.

## Reports and diagnostics

For bug reports, logs, and screenshots intentionally submitted for diagnosis,
you permit the maintainer to reproduce and use the submitted material as needed
to investigate, fix, and document the issue. That permission does not assign
ownership of your audio or other unrelated content. Review reports before
sharing and do not include confidential or unauthorized third-party material.
