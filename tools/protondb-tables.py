#!/usr/bin/env python3
"""Builds gamefix's tables from a ProtonDB reports export:

    curl -LO https://raw.githubusercontent.com/bdefore/protondb-data/master/reports/reports_<mon>1_<year>.tar.gz
    tar xzf reports_*.tar.gz
    tools/protondb-tables.py reports_piiremoved.json <since year> data/

From reports since the given year (Proton changes fast), native builds left out, only fixes
that were the fix itself count: "clean" reports that tried the game as it comes first, saw it
fail, and then had it working with just that change (no protontricks/winetricks, no patches,
mods or file changes in the notes), at least 2 of them, the newest within the export's last
18 months, and no report in its last 12 months saying it works without the change (games get
fixed: From Dust needed a patch plus GE-Proton in 2025 and runs on the default since; Cookard's
Proton 8.0 reports were from 2024 and it runs on the default now).

data/protondb-proton.tsv: the Proton (GE-Proton, or an older official one) that fixed it,
twice as many reports as failing on it, Steam's default failing at least as often as it works;
otherwise "anticheat" when most reports fail and at least 3, and 30% of the failing ones,
blame anti-cheat.

data/protondb-options.tsv: launch options that fixed it on Steam's default Proton, per
setting (so slightly different combinations add up), from an allowlist: settings that work
on any hardware, and ones for one GPU maker counted only from reports on that maker's GPUs
and applied only there. Paths, driver overrides and settings needing other builds of Proton's
parts are left out.
"""
import collections, json, os, re, shlex, sys, time

reports = json.load(open(sys.argv[1]))
since = time.mktime((int(sys.argv[2]), 1, 1, 0, 0, 0, 0, 0, 0))
out_dir = sys.argv[3]
latest = max(r["timestamp"] for r in reports)
recent = latest - 365 * 86400
fresh = latest - 548 * 86400

anticheat = re.compile(r"anti.?cheat|\beac\b|easy ?anti|battl[e]?.?eye|vanguard|ricochet|nprotect|gameguard|xigncode|ateam|kernel.level", re.I)
# Signs in the notes that something else was changed too.
other_fix = re.compile(r"patch|\bmods?\b|\.dll|dll override|winetricks|protontricks|github|nexus|\.ini\b|config|replace|copy|renam|delet|symlink|\blink\b|install(ed|ing)? (the|a|this)|workaround|tweak", re.I)
# Launch options that change nothing about whether a game runs: overlays, HDR, upscalers.
ignored = re.compile(r"^(gamemoderun|mangohud|MANGOHUD(_CONFIG)?=.*|DXVK_HUD=.*|PROTON_LOG=1|ENABLE_VKBASALT=1|DXVK_FRAME_RATE=.*|PROTON_ENABLE_WAYLAND=.*|PROTON_ENABLE_HDR=.*|DXVK_HDR=.*|ENABLE_HDR_WSI=.*|PROTON_FSR4_UPGRADE=.*|PROTON_DLSS_UPGRADE=.*|LSFG.*|ENABLE_LSFG=.*|WINE_FULLSCREEN_FSR.*|OBS_VKCAPTURE=1|obs-gamecapture|SteamDeck=1|STEAMDECK=1)$")
plain = r"[A-Za-z0-9_.,:=+-]+"
allowed = {
    "any": re.compile(rf"^(PROTON_USE_WINED3D=1|PROTON_NO_ESYNC=1|PROTON_NO_FSYNC=1|PROTON_NO_D3D11=1|PROTON_NO_D3D10=1|PROTON_FORCE_LARGE_ADDRESS_AWARE=1|PROTON_SET_GAME_DRIVE=1|PROTON_HEAP_DELAY_FREE=1|PROTON_OLD_GL_STRING=1|PROTON_USE_D9VK=1|PROTON_NO_WRITE_WATCH=1|PROTON_DISABLE_NVAPI=1|WINE_DISABLE_HARDWARE_SCHEDULING=1|PULSE_LATENCY_MSEC=[0-9]+|DXVK_CONFIG={plain}|WINEDLLOVERRIDES=[A-Za-z0-9_.-]+=[bd]?|arg:-{plain}( {plain})?)$"),
    "nvidia": re.compile(r"^(PROTON_HIDE_NVIDIA_GPU=1|PROTON_ENABLE_NVAPI=1|DXVK_ENABLE_NVAPI=1)$"),
    "amd": re.compile(rf"^(RADV_PERFTEST={plain}|RADV_DEBUG={plain}|AMD_VULKAN_ICD=RADV)$"),
}
# The old launch flags on ProtonDB's form, as the settings they stand for.
flags = {"useWineD3d11": "PROTON_USE_WINED3D=1", "disableEsync": "PROTON_NO_ESYNC=1", "disableFsync": "PROTON_NO_FSYNC=1", "largeAddressAware": "PROTON_FORCE_LARGE_ADDRESS_AWARE=1",
         "gameDrive": "PROTON_SET_GAME_DRIVE=1", "disableD3d11": "PROTON_NO_D3D11=1", "disableD3d10": "PROTON_NO_D3D10=1", "heapDelayFree": "PROTON_HEAP_DELAY_FREE=1",
         "oldGlString": "PROTON_OLD_GL_STRING=1", "enableNvapi": "PROTON_ENABLE_NVAPI=1", "hideNvidia": "PROTON_HIDE_NVIDIA_GPU=1", "useWineD9vk": "PROTON_USE_D9VK=1", "noWriteWatch": "PROTON_NO_WRITE_WATCH=1"}


def vendor(gpu):
    gpu = (gpu or "").lower()
    if any(w in gpu for w in ("nvidia", "geforce", "rtx", "gtx")):
        return "nvidia"
    if any(w in gpu for w in ("amd", "radeon", "radv", "vangogh", "rdna")):
        return "amd"
    return "intel" if "intel" in gpu else "?"


def settings(a):
    """The launch options' settings (and game arguments as "arg:..."), or None if they hold
    something that isn't a plain setting or argument."""
    try:
        words = shlex.split(a.get("launchOptions") or "")
    except ValueError:
        return None
    found = {flags[k] for k, v in (a.get("launchFlagsUsed") or {}).items() if v and k in flags}
    # gamescope (a compositor) with its own options, ending at "--".
    if words[:1] == ["gamescope"] and "--" in words:
        words = words[words.index("--") + 1:]
    after = False
    args = []
    for w in words:
        if w == "%command%":
            after = True
        elif ignored.match(w):
            continue
        elif not after and re.match(r"^[A-Z][A-Z0-9_]*=", w):
            found.add(w)
        elif after and w.startswith("-"):
            args.append(w)
        elif after and args and not args[-1].count(" "):
            args[-1] += " " + w  # an argument's value: --rendering-driver vulkan
        else:
            return None
    return found | {"arg:" + a for a in args}


games = collections.defaultdict(collections.Counter)
titles = {}
for r in reports:
    a = r["responses"]
    variant = a.get("variant")
    if variant == "native" or r["timestamp"] < since:
        continue
    app = int(r["app"]["steam"]["appId"])
    titles[app] = " ".join(r["app"]["title"].split())
    g = games[app]
    gpu = vendor((r.get("systemInfo") or {}).get("gpu"))
    ok = a["verdict"] == "yes"
    oob_failed = a.get("triedOob") == "yes" and a.get("verdictOob") == "no"
    g["yes" if ok else "no"] += 1
    notes = " ".join(str(v) for v in (a.get("notes") or {}).values()) + " " + str(a.get("concludingNotes") or "")
    if not ok and (a.get("isImpactedByAntiCheat") or anticheat.search(notes)):
        g["anticheat"] += 1
    customized = {k for k, v in (a.get("customizationsUsed") or {}).items() if v} - {"customProton"}
    found = settings(a)
    untouched = found == set() and not customized and not other_fix.search(notes)
    clean = found is not None and not customized and not other_fix.search(notes)
    if variant in ("official", "experimental"):
        if ok and untouched and not oob_failed:
            g["default_ok"] += 1
            if r["timestamp"] >= recent:
                g["default_ok_recently"] += 1
                g[("plain_recently", gpu)] += 1
        elif not ok:
            g["default_failed"] += 1
        elif oob_failed and clean and found:
            g["options_fixes"] += 1
            for s in found:
                for bucket in ("any", gpu):
                    g[("option", bucket, s)] += 1
                    if r["timestamp"] >= fresh:
                        g[("option_fresh", bucket, s)] += 1
        continue
    if variant == "ge":
        tool = "ge"
    elif variant == "older" and re.match(r"\d+\.\d+", a.get("protonVersion") or ""):
        tool = re.match(r"\d+\.\d+", a["protonVersion"])[0]
    else:
        continue
    if not ok:
        g[("failed", tool)] += 1
    elif oob_failed and clean and not found:
        g[("fixed", tool)] += 1
        g["default_failed"] += 1
        if r["timestamp"] >= fresh:
            g[("fresh", tool)] += 1

proton, options = [], []
for app, g in games.items():
    fixes = [(k[1], n) for k, n in g.items() if isinstance(k, tuple) and k[0] == "fixed" and n >= 2 and n >= 2 * g[("failed", k[1])] and g[("fresh", k[1])]]
    if fixes and g["default_failed"] >= g["default_ok"] and not g["default_ok_recently"]:
        tool, fixed = max(fixes, key=lambda t: (t[1], t[0] == "ge"))
        proton.append((app, tool, fixed, g["default_failed"]))
    elif g["no"] > g["yes"] and g["anticheat"] >= 3 and g["anticheat"] >= 0.3 * g["no"]:
        proton.append((app, "anticheat", g["anticheat"], g["no"]))
    for bucket, pattern in allowed.items():
        if bucket == "any" and g["default_ok_recently"] or bucket != "any" and g[("plain_recently", bucket)]:
            continue
        chosen = sorted(s for (kind, b, s), n in ((k, n) for k, n in g.items() if isinstance(k, tuple) and k[0] == "option") if b == bucket and n >= 2 and g[("option_fresh", b, s)] and pattern.match(s))
        if chosen:
            env = [s for s in chosen if not s.startswith("arg:")]
            args = [s[4:] for s in chosen if s.startswith("arg:")]
            line = " ".join(env + ["%command%"] + args)
            options.append((app, bucket, line, min(g[("option", bucket, s)] for s in chosen), g["default_failed"]))

month = time.strftime("%B %Y", time.gmtime(latest))
licence = [
    "# Derived from the ProtonDB reports export (github.com/bdefore/protondb-data), Open Database",
    "# License 1.0 (opendatacommons.org/licenses/odbl/1-0/); this table is under the same licence.",
]
with open(os.path.join(out_dir, "protondb-proton.tsv"), "w") as f:
    print(f"# Steam games whose ProtonDB reports (since {sys.argv[2]}, export of {month}) show a Proton fixing them", file=f)
    print("# where Steam's default fails (ge = GE-Proton, X.Y = that official Proton; then reports where", file=f)
    print("# switching to it alone fixed the game, reports Steam's default failed in), or anti-cheat stopping", file=f)
    print("# it (then failing reports blaming anti-cheat, all failing reports). Made by tools/protondb-tables.py.", file=f)
    print("\n".join(licence), file=f)
    print("# appid\ttool\tn1\tn2\ttitle", file=f)
    for app, tool, a, b in sorted(proton):
        print(f"{app}\t{tool}\t{a}\t{b}\t{titles[app]}", file=f)
with open(os.path.join(out_dir, "protondb-options.tsv"), "w") as f:
    print(f"# Steam games whose ProtonDB reports (since {sys.argv[2]}, export of {month}) show launch options fixing", file=f)
    print("# them on Steam's default Proton: for any GPU, or only for one maker's (nvidia, amd); the options;", file=f)
    print("# the fewest reports any of them fixed it in; reports Steam's default failed in. Made by", file=f)
    print("# tools/protondb-tables.py.", file=f)
    print("\n".join(licence), file=f)
    print("# appid\tgpu\toptions\treports\tfailed\ttitle", file=f)
    for app, bucket, line, a, b in sorted(options):
        print(f"{app}\t{bucket}\t{line}\t{a}\t{b}\t{titles[app]}", file=f)
print(f"{len(proton)} Proton rows, {len(options)} launch option rows", file=sys.stderr)
