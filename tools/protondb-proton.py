#!/usr/bin/env python3
"""Builds data/protondb-proton.tsv from a ProtonDB reports export.

    curl -LO https://raw.githubusercontent.com/bdefore/protondb-data/master/reports/reports_<mon>1_<year>.tar.gz
    tar xzf reports_*.tar.gz
    tools/protondb-proton.py reports_piiremoved.json <since year> > data/protondb-proton.tsv

Per Steam game, from reports since the given year (Proton changes fast), native builds left out:
- a Proton that works where Steam's default (current official or Experimental) fails: GE-Proton,
  or an older official one; at least 2 working reports, twice as many as failing ones on it,
  and the default failing for at least 2 and at least as often as it works;
- otherwise "anticheat" when most reports fail and at least 3, and 30% of the failing ones,
  blame anti-cheat.
"""
import collections, json, re, sys, time

reports = json.load(open(sys.argv[1]))
since = time.mktime((int(sys.argv[2]), 1, 1, 0, 0, 0, 0, 0, 0))
anticheat = re.compile(r"anti.?cheat|\beac\b|easy ?anti|battl[e]?.?eye|vanguard|ricochet|nprotect|gameguard|xigncode|ateam|kernel.level", re.I)

games = collections.defaultdict(collections.Counter)
titles = {}
latest = 0
for r in reports:
    a = r["responses"]
    variant = a.get("variant")
    latest = max(latest, r["timestamp"])
    if variant == "native" or r["timestamp"] < since:
        continue
    app = int(r["app"]["steam"]["appId"])
    titles[app] = " ".join(r["app"]["title"].split())
    g = games[app]
    ok = a["verdict"] == "yes"
    oob_failed = a.get("triedOob") == "yes" and a.get("verdictOob") == "no"
    g["yes" if ok else "no"] += 1
    notes = " ".join(str(v) for v in (a.get("notes") or {}).values()) + " " + str(a.get("concludingNotes") or "")
    if not ok and (a.get("isImpactedByAntiCheat") or anticheat.search(notes)):
        g["anticheat"] += 1
    if variant in ("official", "experimental"):
        tinkered = a.get("launchOptions") or a.get("customizationsUsed") or a.get("launchFlagsUsed")
        # Made to work on it with launch options or the like: says nothing about the Proton.
        if ok and not tinkered and not oob_failed:
            g["default_ok"] += 1
        elif not ok:
            g["default_failed"] += 1
        continue
    # Tried Steam's default first, then another Proton.
    if oob_failed:
        g["default_failed"] += 1
    if variant == "ge":
        g["ge_ok" if ok else "ge_failed"] += 1
    elif variant == "older":
        m = re.match(r"(\d+)\.(\d+)", a.get("protonVersion") or "")
        if m:
            g[("ok" if ok else "failed", f"{m[1]}.{m[2]}")] += 1

rows = []
for app, g in games.items():
    if g["default_failed"] >= 2 and g["default_failed"] >= g["default_ok"]:
        tools = [("ge", g["ge_ok"], g["ge_failed"])]
        tools += [(k[1], n, g[("failed", k[1])]) for k, n in g.items() if isinstance(k, tuple) and k[0] == "ok"]
        tools = [t for t in tools if t[1] >= 2 and t[1] >= 2 * t[2]]
        if tools:
            tool, worked, _ = max(tools, key=lambda t: (t[1], t[0] == "ge"))
            rows.append((app, tool, worked, g["default_failed"]))
            continue
    if g["no"] > g["yes"] and g["anticheat"] >= 3 and g["anticheat"] >= 0.3 * g["no"]:
        rows.append((app, "anticheat", g["anticheat"], g["no"]))

month = time.strftime("%B %Y", time.gmtime(latest))
print(f"# Steam games whose ProtonDB reports (since {sys.argv[2]}, export of {month}) show a Proton that works")
print("# where Steam's default fails (ge = GE-Proton, X.Y = that official Proton; then reports it worked")
print("# in, reports Steam's default failed in), or anti-cheat stopping it (then failing reports blaming")
print("# anti-cheat, all failing reports). Made by tools/protondb-proton.py.")
print("# Derived from the ProtonDB reports export (github.com/bdefore/protondb-data), Open Database")
print("# License 1.0 (opendatacommons.org/licenses/odbl/1-0/); this table is under the same licence.")
print("# appid\ttool\tn1\tn2\ttitle")
for app, tool, a, b in sorted(rows):
    print(f"{app}\t{tool}\t{a}\t{b}\t{titles[app]}")
