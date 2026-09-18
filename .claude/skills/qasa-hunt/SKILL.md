---
name: qasa-hunt
description: Search Qasa's public GraphQL API for rental listings and turn the results into a ready-to-send application pack. Use when the user wants to find an apartment / flat / room to rent in Stockholm or elsewhere in Sweden, asks what's new on Qasa, wants listings ranked by value, or wants help writing an intro/application message to a Swedish landlord. Runs a questionnaire for the search brief, queries the API, runs a second questionnaire for the applicant profile, composes one tailored message per listing from that listing's own ad text, and serves an interactive results page locally.
---

# Qasa hunt

End-to-end apartment hunt: **brief → search → profile → per-listing messages →
local UI**. The output is a page the user works from by hand — copy a message,
send it, mark the listing, move on.

Everything runs from this directory:

| File | Role |
|---|---|
| `search.py` | GraphQL client (`gql`). Fetch → filter → rank → `results.json` |
| `render.py` | Jinja2 render: `results.json` + `messages.json` → `index.html` |
| `template.html.j2` | The UI template. Self-contained, no network, no CDN |

Working files go in `.qasa-hunt/` at the repo root (gitignored).

**Everything runs in the `skills` dev shell — `nix develop .#skills -c …`,
never plain `nix develop`.** That shell (defined in `flake.nix`) carries
`gql`, `requests`, `requests-toolbelt` and `jinja2`; the default shell is for
the Rust binary and has no Python. Commands below are written from the repo
root.

---

## Step 0 — reuse what you know

```bash
cat .qasa-hunt/brief.json 2>/dev/null
```

If it exists, it holds the user's identity block and last answers from a
previous run.

**Still ask every question below — both questionnaires, every run.** This
applies to Step 1 *and* Step 3: a saved `brief.json` changes the *defaults*,
never which questions get asked. Put the previous value first in each option
list and label it `(last used)`. A returning user then confirms fourteen
visible settings in a handful of clicks instead of retyping them.

Never replace the field questions with a meta-question like "same as last
time, or adjust?" — that hides what is about to be searched behind a yes/no,
and the user can't see or correct a single value. One question per actual
field, always, with the concrete value in the label.

A saved field is the single most likely thing the user came back to change —
their age, a new job, a new employer. Skipping a question because an answer
exists is how a run silently applies last month's facts, and the user only
finds out after reading twenty finished messages.

## Step 1 — search brief questionnaire

Two `AskUserQuestion` calls, four questions each — eight visible fields, every
one showing the concrete value it will set. Adapt the option values to anything
the user already said in the conversation or to `brief.json`: put that value
first and label it `(last used)`. Never collapse these into fewer, vaguer
questions.

**Call 1 — the hard filters**

1. *Budget (total monthly cost, incl. Qasa's fee)* — e.g. `up to 10 000` /
   `up to 13 000` / `up to 16 000` / `up to 20 000`
2. *Size* — `Studio is fine` / `At least 1 bedroom (2 rok)` /
   `At least 2 bedrooms (3 rok)` / `3+ bedrooms`
3. *How central* — `≤ 3 km (inner city)` / `≤ 5 km` / `≤ 8 km` /
   `Anywhere with good transit`
4. *How fresh* — `Last 2 days` / `Last 3 days` / `Last week` / `Last 2 weeks`

**Call 2 — the soft filters**

5. *Furnished* — `Furnished` / `Unfurnished` / `Either`
6. *Contract length* — `Open-ended / long-term only` / `1 year or more` /
   `6 months is fine` / `Anything, even short`
7. *Move-in* — `ASAP` / `Within a month` / `In 2–3 months` / `Flexible`
8. *Must-haves* (multi-select) — `Balcony` / `Home-office space` /
   `Own laundry / dishwasher` / `Pets allowed`

Budget note: the API's `rent` is the landlord's asking price and `monthlyCost`
is what the tenant actually pays (Qasa's fee on top, ~6%). Everything here
filters on `monthlyCost`. Say so if the user quotes a figure from the website.

## Step 2 — run the search

```bash
mkdir -p .qasa-hunt
nix develop .#skills -c python3 .claude/skills/qasa-hunt/search.py \
  --days 3 --max-cost 16500 --min-rooms 1 --max-km 5 \
  --areas se/stockholm --furnished any --min-term-months 0 \
  --limit 40 --out .qasa-hunt/results.json
```

`--help` lists every flag. Notes that matter:

- Results are sorted best-value-first (**kr/m²**) and capped by `--limit`.
- Listings with no coordinates are dropped, because the distance filter
  can't be honoured for them. This is rare.
- Must-haves from question 8 aren't API filters — grep the descriptions in
  `results.json` (`balkong`, `diskmaskin`, `tvättmaskin`, `husdjur`) and lead
  with the listings that mention them.

**If the result is thin (< 5 matches):** don't just report "nothing found".
Re-run once with the budget or radius loosened one notch, present both sets,
and say plainly which constraint was the binding one.

Then read the matches and give the user a short ranked summary **in the
terminal** before moving on — best value, best location, anything anomalous
(a listing far below market for its size and area is worth flagging, and worth
a sanity check). Wait for them to confirm which ones to write to; there's no
point composing messages for listings they've ruled out.

## Step 3 — applicant profile questionnaire

Ask all four calls in full, exactly as in Step 1 — **never skip a field
because `.qasa-hunt/brief.json` already has it.** The saved value becomes the
first option, labelled `(last used)`, so confirming it is one click; changing
it is equally one click. The identity fields in particular (age, job title,
employer, the personal line) go stale between runs.

**Call 1 — `AskUserQuestion`**

1. *Who'd be living there* — `Just me` / `Me + partner` /
   `Me + partner + kids` / `Me + a friend`
2. *Work & income in Sweden* — `Permanent contract (tillsvidare)` /
   `Fixed-term or probation` / `Consultant / self-employed` /
   `Relocating, job starts soon`
3. *Language* — `English` / `Swedish` / `Both (Swedish first)`
4. *What you can honestly claim* (multi-select) — `Non-smoker` / `No pets` /
   `References from previous landlords` / `Deposit + staying 1 year+`

**Calls 2 and 3 — `AskUserQuestion`, one question per identity field**

Six fields, asked as a form: four questions in one call, two in the next.
Most are free text, so each question offers 2–4 options covering the
genuinely enumerable answers plus an explicit *Skip* — and the question text
says to pick **Other** to type the real value. Never ask for these as a prose
paragraph, and never as a single "fill in your details, or leave
placeholders?" choice.

**Every option label must be the literal value that goes into the message**,
never a description of its format. `First name only` is wrong — picking it
tells you nothing to substitute, so the user ends up hand-editing twenty
messages afterwards, which is the whole thing this step exists to avoid.
`Daniil` is right: pick it and it is inserted verbatim. Typed input is used
verbatim too. The only non-value option allowed is an explicit `Skip`, and it
must say what the message will read instead.

Mine real candidates before falling back to guesses — the repo's git identity,
anything the user has already said in the conversation. Say in the option
description where a guessed value came from, so a wrong guess is obvious.

| # | Field | Options to offer |
|---|---|---|
| 1 | Name, as signed off | actual candidate names (git identity, earlier mentions) · `Skip — keep [name]` |
| 2 | Age | `Skip — leave age out`; there is nothing to guess, so say plainly that the typed number is used verbatim |
| 3 | Year moved to Stockholm | literal years — `2026` · `2025` · `2024` · `Skip` |
| 4 | Job title | literal titles that would be inserted as-is — `Software engineer` · `Senior software engineer` · `Skip` |
| 5 | Employer | literal company names, flagged as a guess if inferred · `Skip — "a Stockholm company"` |
| 6 | One personal line for the hooks | three literal sentences, ready to drop into a hook · `Skip` |

Field 6 is the one that earns its keep. A hook built only on ad facts states
a preference; a hook that connects an ad fact to something true about the
user states a reason, and a reason is what gets a reply. Offer lines like
*"I work from home two or three days a week"*, *"I cook a lot and have people
over rarely and quietly"*, *"My current sublet ends and I want somewhere to
settle"*.

Those presets are claims about someone's life: they count only if the user
picks them. Never assume one, and never carry one over from a different
user's run.

No phone number, and no nationality — see the template rules in Step 4.

Save both answer sets plus the search brief to `.qasa-hunt/brief.json`:

```json
{
  "profile": {"name": "…", "age": 34, "year_moved": 2021, "role": "…",
              "employer": "…", "household": "single", "language": "en",
              "claims": ["non_smoker", "no_pets", "references", "deposit"]},
  "brief": [{"label": "Move-in", "value": "Flexible, from 1 Nov"}]
}
```

`brief[]` is displayed verbatim on the results page — put anything there the
user will want to see while following up.

**Never invent a fact.** Blank in the profile means blank in the message, or a
`[bracketed placeholder]` for the user to fill. Claiming a permanent contract,
a reference, or a deposit the user doesn't have is a lie told in their name.

## Step 4 — compose one message per listing

### What Swedish landlords actually screen on

Measured across 400 live Stockholm ads (`description` field, Sept 2026) —
the share of ads stating each requirement:

| Requirement | Share |
|---|---|
| Deposit (1 month, sometimes 2) | 17% |
| **"Skötsam / ordningsam / ansvarsfull"** — tidy, will care for the home | 15% |
| Fixed / permanent employment, stable income | 8% |
| Non-smoking | 8% |
| Long-term, 1 year+ | 7% |
| **"Tell me about yourself"** — explicitly invites an intro | 7% |
| No pets | 7% |
| References | 5% |
| What you do for a living | 4% |
| Why you're looking, and for how long | 4% |
| Home insurance required | 4% |
| Credit check / no payment remarks | 4% |

Three conclusions drive the message:

1. **Character outranks money.** More ads ask for a *tidy, responsible* person
   than ask about income. Most are private individuals subletting their own
   home; they want a feel for who you are, not just a solvent stranger.
2. **Only ~7% state hard requirements, but every landlord screens anyway.**
   Volunteering the checkable facts unprompted saves a round-trip — and
   landlords receiving dozens of applications decide in seconds.
3. **37% describe the home as "lugn/tyst".** Quiet is the dominant
   self-image of these flats. Mirror it.

`search.py` has already flagged each listing's requirements
(`requirements[]`) and pulled the landlord's own tenant-facing sentences
(`landlord_demands[]`). **Answer every flag that listing raises**, in the
listing's own terms.

### Structure (~170 words, for a listing with a real description)

```
Hello [landlord first name]! I'd like to apply for your apartment.

A bit about me: I'm [name], [age], living in Stockholm since [year]. I work
as [role] at [employer] on a permanent contract (tillsvidareanställning), so
my income covers the rent with a good margin. I'm happy to send my
employment contract, payslips and a UC credit check straight away — no
payment remarks.

I'd be living there on my own. I'm a non-smoker, I have no pets, and I'm a
quiet, tidy tenant — no parties, and I'll look after the flat and be
considerate of the neighbours. I'm looking for somewhere long-term, a year
or more. I have references from my previous landlords who are happy to be
contacted.

[THE HOOK — one sentence that could only have been written about this ad.]

I can make any viewing time that suits you, and I'm ready to move in
[straight away / on your date]. My ID and income are verified on my Qasa
profile.

Best regards,
[name]
```

**Do not re-add what this template deliberately leaves out:**

- *Always open with their name.* `search.py` resolves it into
  `landlord_name` for every listing — use it verbatim. If it is missing
  (rare), open with plain `Hello!`; never guess a name, and never lift one
  out of the ad text unless the landlord signed it there themselves.
- *No echo of the size or street.* The landlord knows what they are renting;
  repeating it back reads as mail-merge.
- *No nationality.* Irrelevant to the decision, and it invites bias.
- *Occupancy is fixed: always "on my own", never a partner or a household.*
  It is deliberately boilerplate — a large share of ads restrict the flat to
  one person, and stating it up front answers them before they ask. Where an
  ad makes it an explicit condition (`single_only` / `no_children`), echo
  their wording in the hook as well.
- *No phone number.* Contact happens in the Qasa thread; a number in the
  sign-off adds nothing and leaks a detail before there's any relationship.
- *No deposit or home-insurance offer as boilerplate*, even though deposit is
  the most-stated requirement (17%). Answer it in the hook when that ad asks.
- *Never name a viewing time.* Timing is entirely the landlord's; the line
  says you will make any slot work.

Order is deliberate: the checkable facts first (that's the screen), the
character sentence in the middle (that's the decision), the hook before the
close (that's what separates you from the other thirty applicants).

Keep it lean. Every sentence that isn't a checkable fact, a character signal
or the hook is padding, and padding is what makes a message look mass-sent.

**For `thin_ad: true` listings** (ad under 300 characters — nothing to hook
onto, and a long letter reads as mass-mail), use the short form:

```
Hello [landlord first name]! I'm interested in your apartment.

I'm [name], [age], [role] at [employer] on a permanent contract, living on
my own. Non-smoker, no pets, quiet and tidy, looking to stay a year or more.
References from previous landlords, and I can send payslips and a UC check.
I can make any viewing time that suits you.

Best, [name]
```

### The hook — the part that actually differentiates

One sentence, drawn from something **only this ad** contains: a named feature
(the *sovalkov*, the working *kakelugn*, the courtyard, the big hall), the
contract length, a neighbourhood detail, or a requirement they spelled out.
Tie it to a real reason it suits the user. Put the same sentence in the
`hook` field so the page can show the angle at a glance.

Examples of the register to aim for:

- *"The open-ended contract is exactly what I'm after — I'm not looking for
  a few months, I want somewhere to settle."*
- *"The alcove plus the hall as a workspace is ideal — I work from home a
  couple of days a week, and a quiet courtyard is what I need for that."*
- *"Non-smoker, no cat, and a year with the option to extend suits me
  perfectly."* ← when the ad said *"rök- kattfritt, minst 1 år"*

If nothing in the ad supports a genuine hook, write no hook rather than a
generic compliment. "What a lovely apartment!" is worse than nothing.

### Language

Follow the user's choice. If they picked Swedish or "both" and the ad is in
Swedish, write the Swedish version as the primary text — natural, not
translated-sounding; `du`, never `ni` to a private landlord. For "both", put
Swedish first and a short English version after a `---` line.

### Don'ts — every one of these gets applications binned

- **No offer to pay several months up front or above asking.** In Sweden that
  reads as a scam signal, not enthusiasm. Offer the normal deposit.
- **No rent negotiation in the first message.** Ask for a viewing.
- **Never the same text twice.** The hook is the whole point; landlords say
  outright that vague or generic first messages get ignored.
- **Don't raise folkbokföring, further subletting, or an extra person moving
  in later.** Those belong at the viewing, if at all.
- **No apologising, no over-explaining, no emoji, no wall of text.** Confident
  and factual reads as reliable.

Write the result to `.qasa-hunt/messages.json`:

```json
{
  "1460295": {"message": "Hi! I'd like to apply for…", "hook": "Open-ended contract — they want someone who'll stay."},
  "1462236": {"message": "…", "hook": "…"}
}
```

## Step 5 — render and serve

```bash
nix develop .#skills -c python3 .claude/skills/qasa-hunt/render.py \
  --results .qasa-hunt/results.json \
  --messages .qasa-hunt/messages.json \
  --brief .qasa-hunt/brief.json \
  --out .qasa-hunt/index.html
```

Serve it (python3 comes from the dev shell — nothing is installed on the host):

```bash
nix develop .#skills -c python3 -m http.server 8765 --directory .qasa-hunt
```

Run that with `run_in_background: true`, poll `curl -s -o /dev/null -w '%{http_code}'
http://localhost:8765/` until it answers 200, then give the user the URL. Don't
announce the page before it responds. If the port is taken, pick another.

`make hunt` (and `make hunt PORT=8770`) is the same thing for the user to run
themselves — but `make` isn't always on the agent's PATH, so use the `nix
develop` form above from the skill.

The page does the rest: sort by value / price / distance / recency, filter,
per-listing status (New → Sent → Viewing → No) and notes, and a Copy button per
message. Status, notes and message edits live in that browser's `localStorage`,
keyed by the run's timestamp — a re-render of the same run keeps them, a fresh
search starts clean.

---

## The GraphQL endpoint

`POST https://api.qasa.com/graphql` — public, unauthenticated, no API key, no
rate limit observed at this volume. Plain JSON `{query, operationName,
variables}`. The site's own operation is `HomeSearch` → `homeIndexSearch`.

```graphql
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
```

`params`: `{currency: "SEK", areaIdentifier: [slug…], markets: ["sweden"],
homeType: ["apartment"], rentalType: ["long_term"], shared: false}`.
`order`: `{direction: "descending", orderBy: "published_or_bumped_at"}`.
`offset`/`limit` sit on `documents`, not on the search — limit 50 per page.

Things that will bite you:

- **Introspection is disabled** (`__type` doesn't exist on `QueryRoot`), but
  error messages name valid fields: ask for a field that doesn't exist and the
  response says so, often with a "Did you mean…?". That's how the field list
  above was established — probe, don't guess silently.
- **`location.point { lat lon }` exists; `latitude`/`longitude`/`postalCode`
  do not.** The point is the only geo data available, and it's what makes a
  real radius filter possible.
- **The landlord's name is not on the search document** — it carries only
  `landlordUid`. The root `home(id:)` query does expose it:
  `home(id: "1462860") { landlord { firstName companyName professional } }`.
  Aliases batch it into one request (`h1462860: home(id: …) {…}` per listing),
  which is how `search.py` fills `landlord_name` for the greeting. `firstName`
  can hold two given names ("Roman Sergeevitj", "Terezia Magdalena") — greet
  with the first token only. `professional: true` marks a letting agency;
  greet those by `companyName` when it's set.
- **An invalid `areaIdentifier` silently returns a country-wide result**
  (~16 700 homes) instead of an error. If a search suddenly returns listings
  in Malmö, the slug is wrong. Use the verified list in `src/search.rs`
  (`declare_areas!`) — diacritics are inconsistent between slugs
  (`se/sodermalm` but `se/björkhagen`), each is the exact working string.
  Multiple slugs are unioned.
- **`shared: false` matters.** Qasa tags single rooms in shared flats as
  `homeType: apartment` too; without it you get rooms mixed into the results.
- **`publishedAt` vs `publishedOrBumpedAt`.** Ordering is by bump time, so an
  old listing that was bumped appears at the top. Filter freshness on
  `publishedAt`; page until a whole page's bump times predate the cutoff.
- **Home ids are monotonic integers** — a higher id is a newer listing. (The
  Bostadsförmedlingen feed's ids are *not*; don't carry the assumption over.)
- **The schema drifts.** Treat every field as optional; a missing one should
  show as a gap in the UI, not crash the run.

Listing URL: `https://qasa.com/se/en/home/<id>`.

## Troubleshooting

- `Qasa GraphQL rejected the query: … doesn't exist on type 'HomeDocument'` —
  the schema moved. Drop the field from `HOME_SEARCH` in `search.py`; the rest
  keeps working. (`gql` raises `TransportQueryError`, which carries the same
  field-name hints the raw API returns.)
- Country-wide results → bad area slug (see above).
- `ModuleNotFoundError: gql` / `jinja2` → you used plain `nix develop`.
  The skill tooling lives in `nix develop .#skills`.
- `jinja2.exceptions.UndefinedError` → the template referenced a key the
  context doesn't have. `StrictUndefined` is deliberate: a silent blank in a
  results page is worse than a loud failure.
- Port already in use → `make hunt PORT=8770`.
