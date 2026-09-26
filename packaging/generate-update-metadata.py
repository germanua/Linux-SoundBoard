#!/usr/bin/env python3
import argparse
import json
from pathlib import Path


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--version", required=True)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--published-at", required=True)
    parser.add_argument("--channel", choices=("stable", "dev"), default="stable")
    parser.add_argument("--minimum-updater-version", default="2.4.6")
    parser.add_argument("--appimage", required=True)
    parser.add_argument("--summary-file", required=True)
    parser.add_argument("--output", required=True)
    parser.add_argument("--requires-helper-update", action="store_true")
    args = parser.parse_args()

    appimage = Path(args.appimage)
    if not appimage.is_file():
        raise SystemExit(f"missing AppImage: {appimage}")
    summaries = []
    for line in Path(args.summary_file).read_text(encoding="utf-8").splitlines():
        if line.startswith("- "):
            summaries.append(line[2:].strip())
        if len(summaries) == 5:
            break

    payload = {
        "schema": 1,
        "version": args.version,
        "tag": args.tag,
        "channel": args.channel,
        "published_at": args.published_at,
        "minimum_updater_version": args.minimum_updater_version,
        "critical": False,
        "requires_helper_update": args.requires_helper_update,
        "appimage": {"name": appimage.name, "size": appimage.stat().st_size},
        "summary": summaries,
    }
    output = Path(args.output)
    output.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
