#!/usr/bin/env python3
"""Builds data/protondb-proton.tsv from a ProtonDB reports export.

    curl -LO https://raw.githubusercontent.com/bdefore/protondb-data/master/reports/reports_<mon>1_<year>.tar.gz
    tar xzf reports_*.tar.gz
    tools/protondb-proton.py reports_piiremoved.json <since year> > data/protondb-proton.tsv

Per Steam game, from reports since the given year (Proton changes fast), native builds left out.
A Proton (GE-Proton, or an older official one) is named only where it was the fix itself:
- at least 2 "clean" reports that tried Steam's default first, saw it fail, and then had the
  game working on that Proton with nothing else changed (no launch options beyond overlays and
  gamemode, no protontricks/winetricks, no patches, mods or file changes in the notes), and
  twice as many as reports failing on it;
- Steam's default failing at least as often as it works without changes, and no report in the
  export's last 12 months saying it works that way (games get fixed: From Dust needed a patch
  plus GE-Proton in 2025 and runs on the default since);
- the newest of those fixes within the export's last 18 months: Cookard's Proton 8.0 reports
  are from mid-2024 and it runs on the default now, with nobody reporting since.
Otherwise "anticheat" when most reports fail and at least 3, and 30% of the failing ones,
blame anti-cheat.
"""
import collections, json, re, sys, time

reports = json.load(open(sys.argv[1]))
since = time.mktime((int(sys.argv[2]), 1, 1, 0, 0, 0, 0, 0, 0))
recent = max(r["timestamp"] for r in reports) - 365 * 86400
fresh = recent - 183 * 86400
anticheat = re.compile(r"anti.?cheat|\beac\b|easy ?anti|battl[e]?.?eye|vanguard|ricochet|nprotect|gameguard|xigncode|ateam|kernel.level", re.I)
# Signs in the notes that something besides the Proton was changed.
other_fix = re.compile(r"patch|\bmods?\b|\.dll|dll override|winetricks|protontricks|launch option|github|nexus|\.ini\b|config|replace|copy|renam|delet|symlink|\blink\b|install(ed|ing)? (the|a|this)|workaround|tweak", re.I)
# Launch options that change nothing about whether a game runs.
harmless = re.compile(r"^(%command%|gamemoderun|mangohud|gamescope\b.*--|MANGOHUD=\S+|MANGOHUD_CONFIG=\S+|DXVK_HUD=\S+|PROTON_LOG=1|ENABLE_VKBASALT=1|--|\s)*$")

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
    customized = {k for k, v in (a.get("customizationsUsed") or {}).items() if v} - {"customProton"}
    clean = harmless.match(a.get("launchOptions") or "") and not customized and not any((a.get("launchFlagsUsed") or {}).values()) and not other_fix.search(notes)
    if variant in ("official", "experimental"):
        if ok and clean and not oob_failed:
            g["default_ok"] += 1
            if r["timestamp"] >= recent:
                g["default_ok_recently"] += 1
        elif not ok:
            g["default_failed"] += 1
        continue
    if variant == "ge":
        tool = "ge"
    elif variant == "older" and re.match(r"\d+\.\d+", a.get("protonVersion") or ""):
        tool = re.match(r"\d+\.\d+", a["protonVersion"])[0]
    else:
        continue
    if not ok:
        g[("failed", tool)] += 1
    elif oob_failed and clean:
        g[("fixed", tool)] += 1
        g["default_failed"] += 1
        if r["timestamp"] >= fresh:
            g[("fresh", tool)] += 1

rows = []
for app, g in games.items():
    fixes = [(k[1], n) for k, n in g.items() if isinstance(k, tuple) and k[0] == "fixed" and n >= 2 and n >= 2 * g[("failed", k[1])] and g[("fresh", k[1])]]
    if fixes and g["default_failed"] >= g["default_ok"] and not g["default_ok_recently"]:
        tool, fixed = max(fixes, key=lambda t: (t[1], t[0] == "ge"))
        rows.append((app, tool, fixed, g["default_failed"]))
    elif g["no"] > g["yes"] and g["anticheat"] >= 3 and g["anticheat"] >= 0.3 * g["no"]:
        rows.append((app, "anticheat", g["anticheat"], g["no"]))

month = time.strftime("%B %Y", time.gmtime(latest))
print(f"# Steam games whose ProtonDB reports (since {sys.argv[2]}, export of {month}) show a Proton fixing them")
print("# where Steam's default fails (ge = GE-Proton, X.Y = that official Proton; then reports where")
print("# switching to it alone fixed the game, reports Steam's default failed in), or anti-cheat stopping")
print("# it (then failing reports blaming anti-cheat, all failing reports). Made by tools/protondb-proton.py.")
print("# Derived from the ProtonDB reports export (github.com/bdefore/protondb-data), Open Database")
print("# License 1.0 (opendatacommons.org/licenses/odbl/1-0/); this table is under the same licence.")
print("# appid\ttool\tn1\tn2\ttitle")
for app, tool, a, b in sorted(rows):
    print(f"{app}\t{tool}\t{a}\t{b}\t{titles[app]}")
