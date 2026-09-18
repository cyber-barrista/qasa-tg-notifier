#!/usr/bin/env python3
"""Qasa apartment search — the query layer behind the `qasa-hunt` skill.

Talks to the same public, unauthenticated GraphQL endpoint the Rust notifier
uses (`api.qasa.com/graphql`, operation `HomeSearch`), but asks for two fields
the notifier doesn't need: `description` (the landlord's own ad text) and
`location.point { lat lon }` (real coordinates, so distance to the centre is
computed rather than guessed from an area slug).

Uses `gql`, so the query document is parsed and validated client-side before
anything is sent, and one requests session is reused across pages. Run it in
the skills shell: `nix develop .#skills -c python3 search.py …`.

Output is a JSON document on stdout or at --out, ready for render.py.
"""

import argparse
import json
import math
import re
import sys
from datetime import datetime, timedelta, timezone

from gql import Client, gql
from gql.transport.exceptions import TransportError, TransportQueryError
from gql.transport.requests import RequestsHTTPTransport

ENDPOINT = "https://api.qasa.com/graphql"
PAGE_LIMIT = 50
MAX_PAGES = 12

# Sergels torg — the conventional "centre of Stockholm" for distance purposes.
CENTER = (59.3326, 18.0649)

HOME_SEARCH = gql("""
query HomeSearch($order: HomeIndexSearchOrderInput, $offset: Int,
                 $limit: Int, $params: HomeSearchParamsInput) {
  homeIndexSearch(order: $order, params: $params) {
    documents(offset: $offset, limit: $limit) {
      totalCount
      nodes {
        id title description rent currency monthlyCost roomCount squareMeters
        homeType furnished firstHand platform startDate endDate
        rentalLengthSeconds publishedAt publishedOrBumpedAt
        location { locality route streetNumber point { lat lon } }
      }
    }
  }
}
""")

# The search document carries only `landlordUid` — no name. `home(id:)` does
# expose the landlord, so one aliased batch query resolves every listing's
# first name in a single request (aliases must be valid GraphQL names, hence
# the `h` prefix on the numeric id).
LANDLORD_FIELDS = "{ landlord { firstName companyName professional } }"
LANDLORD_BATCH = 25


def fetch_landlord_names(session, ids):
    """{listing id: greeting name}. Best-effort: never fails the search."""
    names = {}
    for i in range(0, len(ids), LANDLORD_BATCH):
        chunk = ids[i:i + LANDLORD_BATCH]
        body = " ".join(f'h{hid}: home(id: "{hid}") {LANDLORD_FIELDS}' for hid in chunk)
        try:
            data = session.execute(gql("{ " + body + " }"))
        except (TransportError, TransportQueryError) as e:
            tracing = f"landlord names unavailable for {len(chunk)} listing(s): {e}"
            print(tracing, file=sys.stderr)
            continue
        for hid in chunk:
            node = (data.get(f"h{hid}") or {}).get("landlord") or {}
            # A professional letting agent is greeted by company, a private
            # landlord by first given name only: "Roman Sergeevitj" -> "Roman",
            # which is how a Swedish sublet message would actually open.
            if node.get("professional") and node.get("companyName"):
                names[hid] = node["companyName"].strip()
            elif node.get("firstName"):
                names[hid] = node["firstName"].strip().split()[0]
    return names


# What landlords screen on, as they actually phrase it in ad text. Derived by
# counting these patterns across 400 live Stockholm ads — see SKILL.md for the
# frequencies. `label` is what the UI chips show; the skill answers every
# matched requirement explicitly in the application message.
REQUIREMENTS = [
    ("no_smoking", "Non-smoking",
     r"rökfri|rökfritt|röker inte|ej rökn|inga rökare|non.?smok|no smok"),
    ("no_pets", "No pets",
     r"djurfri|djurfritt|kattfri|inga husdjur|ej husdjur|utan husdjur|no pets|pet.?free"),
    ("tidy", "Wants 'skötsam' / responsible",
     r"skötsam|ordningsam|ordningsamt|ansvarsfull|pålitlig|responsible|tidy|ta väl hand om|vårdar|take (good )?care of"),
    ("income", "Proof of income / permanent job",
     r"fast (arbete|anställning|inkomst|jobb)|fastanställ|tillsvidareanställ|stabil (inkomst|ekonomi)|styrkt ekonomi|stable (income|job)|permanent (job|employment)|inkomstkrav"),
    ("credit", "Credit check / no payment remarks",
     r"betalningsanmärkning|kreditupplysning|\buc\b|credit check"),
    ("references", "References",
     r"referens|reference"),
    ("long_term", "Long-term (1 yr+)",
     r"långsiktig|långtid|minst 1 år|minst ett år|long.?term|at least (one|1) year|gärna längre|längre uthyrning|förläng"),
    ("intro", "Asks you to introduce yourself",
     r"berätta (gärna )?(lite )?om dig|skriv (gärna )?(några rader|en kort)|kort presentation|presentera dig|introduction about yourself|tell (me|us) (a little |a bit )?about"),
    ("occupation", "Asks what you do",
     r"sysselsättning|vad du arbetar|vad du gör|what you do|occupation|yrke"),
    ("deposit", "Deposit",
     r"deposition|deposit"),
    ("insurance", "Home insurance required",
     r"hemförsäkring|home insurance"),
    ("quiet", "Quiet / no parties",
     r"inga fester|ej fester|no parties|lugn hyresgäst|störa|respekt(era)? (för )?(mina |de )?grann|respect for neighb"),
    ("single_only", "One person only",
     r"endast (till )?för en person|bara en person|en person, inte flera|single (person|tenant) only"),
    ("no_children", "No children",
     r"utan barn|barnfri|barn, rök|no children"),
    ("students", "Students welcome",
     r"student"),
]
REQ_RE = [(key, label, re.compile(pat, re.I)) for key, label, pat in REQUIREMENTS]

# A sentence matching this is the landlord stating what they want from a
# tenant (as opposed to describing the flat). Quoted back in the UI so the
# message can be checked against the ad's own words.
DEMAND_RE = re.compile(
    r"söker|önskar|krav|vill (jag|vi|att)|kräv|ser gärna|förutsätt|passar (bra |bäst )?för|"
    r"uthyres till|hyrs ut till|berätta|looking for|require|must be|please (send|tell)|preference",
    re.I,
)


def new_session():
    """One HTTP session, reused for every page."""
    transport = RequestsHTTPTransport(url=ENDPOINT, timeout=30, retries=2)
    # The endpoint has introspection disabled, so the schema can't be fetched
    # for validation; gql still parses the document above at import time,
    # which catches the mistakes worth catching.
    return Client(transport=transport, fetch_schema_from_transport=False)


def fetch_page(session, offset, areas, home_types):
    data = session.execute(HOME_SEARCH, operation_name="HomeSearch", variable_values={
        "offset": offset,
        "limit": PAGE_LIMIT,
        # Newest first: everything published recently clusters at the top, so
        # paging can stop as soon as a whole page predates the cutoff.
        "order": {"direction": "descending", "orderBy": "published_or_bumped_at"},
        "params": {
            "currency": "SEK",
            "areaIdentifier": areas,
            "markets": ["sweden"],
            "homeType": home_types,
            "rentalType": ["long_term"],
            # Qasa tags single rooms in shared flats as `apartment` too;
            # this keeps the results to whole homes.
            "shared": False,
        },
    })
    return data["homeIndexSearch"]["documents"]


def parse_dt(s):
    return datetime.fromisoformat(s.replace("Z", "+00:00")) if s else None


def km_from_center(lat, lon):
    lat0, lon0 = CENTER
    dlat, dlon = math.radians(lat - lat0), math.radians(lon - lon0)
    a = (math.sin(dlat / 2) ** 2
         + math.cos(math.radians(lat0)) * math.cos(math.radians(lat))
         * math.sin(dlon / 2) ** 2)
    return 2 * 6371.0 * math.asin(math.sqrt(a))


def analyse(desc):
    """Requirement flags + the landlord's own tenant-facing sentences."""
    flags = [{"key": k, "label": label} for k, label, rx in REQ_RE if rx.search(desc)]
    demands = []
    for sentence in re.split(r"(?<=[.!?\n])\s+", desc):
        s = " ".join(sentence.split())
        if 20 < len(s) < 280 and DEMAND_RE.search(s) and any(
                rx.search(s) for _, _, rx in REQ_RE):
            demands.append(s)
    return flags, demands[:4]


def main():
    p = argparse.ArgumentParser(description="Search Qasa for apartments.")
    p.add_argument("--days", type=float, default=3,
                   help="only listings first published within this many days")
    p.add_argument("--min-cost", type=int, default=0, help="min total monthly cost, SEK")
    p.add_argument("--max-cost", type=int, default=10**9, help="max total monthly cost, SEK")
    p.add_argument("--min-rooms", type=float, default=0)
    p.add_argument("--min-sqm", type=float, default=0)
    p.add_argument("--max-km", type=float, default=10**9,
                   help="max straight-line distance from Sergels torg")
    p.add_argument("--areas", default="se/stockholm",
                   help="comma-separated Qasa areaIdentifier slugs. Only verified "
                        "slugs work — an invalid one silently returns a "
                        "country-wide result. See src/search.rs for the curated list.")
    p.add_argument("--home-types", default="apartment",
                   help="comma-separated: apartment,house,terrace_house,cottage,…")
    p.add_argument("--furnished", choices=["any", "yes", "no"], default="any")
    p.add_argument("--min-term-months", type=float, default=0,
                   help="minimum rental length; open-ended listings always pass")
    p.add_argument("--limit", type=int, default=40, help="max results to keep")
    p.add_argument("--out", help="write JSON here instead of stdout")
    args = p.parse_args()

    areas = [a.strip() for a in args.areas.split(",") if a.strip()]
    home_types = [t.strip() for t in args.home_types.split(",") if t.strip()]
    cutoff = datetime.now(timezone.utc) - timedelta(days=args.days)

    scanned, seen, total = [], set(), 0
    try:
        with new_session() as session:
            for page in range(MAX_PAGES):
                docs = fetch_page(session, page * PAGE_LIMIT, areas, home_types)
                total = docs.get("totalCount", 0)
                nodes = docs["nodes"]
                if not nodes:
                    break
                page_has_fresh = False
                for n in nodes:
                    bumped = parse_dt(n.get("publishedOrBumpedAt") or n.get("publishedAt"))
                    if bumped is None or bumped >= cutoff:
                        page_has_fresh = True
                    if n["id"] not in seen:
                        seen.add(n["id"])
                        scanned.append(n)
                if not page_has_fresh:
                    break
    except TransportQueryError as e:
        # The schema drifted, or a variable was rejected. The errors name the
        # offending field, often with a "Did you mean…?".
        raise SystemExit(f"Qasa GraphQL rejected the query:\n{e.errors}")
    except TransportError as e:
        raise SystemExit(f"Could not reach {ENDPOINT}: {e}")

    kept = []
    for n in scanned:
        published = parse_dt(n.get("publishedAt"))
        if published is None or published < cutoff:
            continue

        cost = n.get("monthlyCost") or n.get("rent") or 0
        if not (args.min_cost <= cost <= args.max_cost):
            continue
        if (n.get("roomCount") or 0) < args.min_rooms:
            continue
        sqm = n.get("squareMeters") or 0
        if sqm < args.min_sqm:
            continue
        if args.furnished != "any" and n.get("furnished") is not None:
            if n["furnished"] != (args.furnished == "yes"):
                continue

        secs = n.get("rentalLengthSeconds")
        months = round(secs / 2629800, 1) if secs else None
        if months is not None and months < args.min_term_months:
            continue

        loc = n.get("location") or {}
        pt = loc.get("point") or {}
        km = km_from_center(pt["lat"], pt["lon"]) if pt.get("lat") is not None else None
        if km is None or km > args.max_km:
            # No coordinates means the distance filter can't be honoured;
            # drop rather than silently include something 30 km out.
            continue

        desc = (n.get("description") or "").strip()
        flags, demands = analyse(desc)
        street = " ".join(x for x in [loc.get("route"), loc.get("streetNumber")] if x)

        kept.append({
            "id": n["id"],
            "url": f"https://qasa.com/se/en/home/{n['id']}",
            "title": n.get("title"),
            "cost": cost,
            "rent": n.get("rent"),
            "sqm": sqm or None,
            "kr_per_sqm": round(cost / sqm) if sqm else None,
            "rooms": n.get("roomCount"),
            "km": round(km, 1),
            "street": street or None,
            "locality": loc.get("locality"),
            "furnished": n.get("furnished"),
            "first_hand": n.get("firstHand"),
            "platform": n.get("platform"),
            "start_date": (n.get("startDate") or "")[:10] or None,
            "end_date": (n.get("endDate") or "")[:10] or None,
            "term_months": months,
            "published_at": n["publishedAt"],
            "description": desc,
            "requirements": flags,
            "landlord_demands": demands,
            # A thin ad can't support a long letter; the skill switches to the
            # short message variant below this threshold.
            "thin_ad": len(desc) < 300,
        })

    kept.sort(key=lambda r: (r["kr_per_sqm"] is None, r["kr_per_sqm"] or 0))
    kept = kept[:args.limit]

    # Resolve landlord names last, so only the listings actually kept cost a
    # lookup. The message greets them by name, so this is worth one request.
    if kept:
        try:
            with new_session() as session:
                names = fetch_landlord_names(session, [r["id"] for r in kept])
        except TransportError as e:
            print(f"landlord names unavailable: {e}", file=sys.stderr)
            names = {}
        for r in kept:
            r["landlord_name"] = names.get(r["id"])

    out = {
        "generated_at": datetime.now(timezone.utc).isoformat(timespec="seconds"),
        "query": {
            "days": args.days, "min_cost": args.min_cost,
            "max_cost": None if args.max_cost >= 10**9 else args.max_cost,
            "min_rooms": args.min_rooms, "min_sqm": args.min_sqm,
            "max_km": None if args.max_km >= 10**9 else args.max_km,
            "areas": areas, "home_types": home_types,
            "furnished": args.furnished, "min_term_months": args.min_term_months,
        },
        "stats": {"scanned": len(scanned), "live_total": total, "matched": len(kept)},
        "listings": kept,
    }

    text = json.dumps(out, ensure_ascii=False, indent=2)
    if args.out:
        with open(args.out, "w", encoding="utf-8") as f:
            f.write(text)
        print(f"{len(kept)} match(es) of {len(scanned)} scanned → {args.out}",
              file=sys.stderr)
    else:
        print(text)


if __name__ == "__main__":
    main()
