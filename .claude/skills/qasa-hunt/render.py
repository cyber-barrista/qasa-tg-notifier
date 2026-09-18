#!/usr/bin/env python3
"""Render the qasa-hunt results page from search output + composed messages.

    nix develop .#skills -c python3 render.py \
        --results .qasa-hunt/results.json \
        --messages .qasa-hunt/messages.json \
        --brief .qasa-hunt/brief.json \
        --out .qasa-hunt/index.html

Jinja2 with autoescaping: listing text is landlord-written and goes straight
into the page, so it must never be treated as markup.
"""

import argparse
import json
import os
import sys
from datetime import datetime, timezone

from jinja2 import Environment, FileSystemLoader, StrictUndefined, select_autoescape

HERE = os.path.dirname(os.path.abspath(__file__))
TEMPLATE = "template.html.j2"


def load(path, default=None):
    if not path:
        return default
    if not os.path.exists(path):
        if default is None:
            raise SystemExit(f"missing file: {path}")
        return default
    with open(path, encoding="utf-8") as f:
        return json.load(f)


def fmt_kr(n):
    """1234567 → '1 234 567 kr' (sv-SE grouping, non-breaking spaces)."""
    if n is None:
        return "—"
    return f"{n:,}".replace(",", " ") + " kr"


def fmt_ago(iso):
    hours = (datetime.now(timezone.utc)
             - datetime.fromisoformat(iso.replace("Z", "+00:00"))).total_seconds() / 3600
    if hours < 1:
        return "just now"
    if hours < 24:
        return f"{round(hours)} h ago"
    return f"{round(hours / 24)} d ago"


def fmt_term(listing):
    if listing.get("term_months"):
        return f"{fmt_num(listing['term_months'])} mo"
    if listing.get("end_date"):
        return f"until {listing['end_date']}"
    return "open-ended"


def fmt_num(n):
    """Drop the pointless decimal: 3.0 → '3', 4.5 → '4.5'."""
    if n is None:
        return ""
    if isinstance(n, float) and n.is_integer():
        return str(int(n))
    return str(n)


def fmt_stamp(iso):
    return iso.replace("T", " ").replace("+00:00", " UTC")


def main():
    p = argparse.ArgumentParser(description=__doc__,
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--results", required=True, help="output of search.py")
    p.add_argument("--messages", help='{"<listing id>": {"message": …, "hook": …}}')
    p.add_argument("--brief", help='{"profile": {…}, "brief": [{"label":…,"value":…}]}')
    p.add_argument("--template-dir", default=HERE)
    p.add_argument("--out", required=True)
    args = p.parse_args()

    data = load(args.results)
    messages = load(args.messages, {})
    extra = load(args.brief, {})

    missing = []
    for listing in data.get("listings", []):
        entry = messages.get(listing["id"]) or {}
        if isinstance(entry, str):          # bare string is accepted as the message
            entry = {"message": entry}
        listing["message"] = entry.get("message", "")
        listing["hook"] = entry.get("hook", "")
        if not listing["message"]:
            missing.append(listing["id"])

    env = Environment(
        loader=FileSystemLoader(args.template_dir),
        autoescape=select_autoescape(default=True, default_for_string=True),
        undefined=StrictUndefined,          # a typo in the template fails loudly
        trim_blocks=True,
        lstrip_blocks=True,
    )
    env.filters["kr"] = fmt_kr
    env.filters["ago"] = fmt_ago
    env.filters["term"] = fmt_term
    env.filters["num"] = fmt_num
    env.filters["stamp"] = fmt_stamp

    html = env.get_template(TEMPLATE).render(
        generated_at=data.get("generated_at", ""),
        stats=data.get("stats", {}),
        query=data.get("query", {}),
        listings=data.get("listings", []),
        brief=extra.get("brief", []),
        profile=extra.get("profile", {}),
    )

    os.makedirs(os.path.dirname(os.path.abspath(args.out)), exist_ok=True)
    with open(args.out, "w", encoding="utf-8") as f:
        f.write(html)

    print(f"wrote {args.out} ({len(data.get('listings', []))} listings)", file=sys.stderr)
    if missing:
        print(f"warning: no message composed for {len(missing)} listing(s): "
              + ", ".join(missing), file=sys.stderr)


if __name__ == "__main__":
    main()
