#!/usr/bin/env python3
"""Builds data/umu-fixes.tsv: the games bought outside Steam that protonfixes has a fix for.

    git clone --depth 1 https://github.com/Open-Wine-Components/umu-protonfixes
    curl -LO https://raw.githubusercontent.com/Open-Wine-Components/umu-database/main/umu-database.csv
    tools/umu-table.py umu-database.csv umu-protonfixes data/umu-fixes.tsv

protonfixes (inside GE-Proton, UMU-Proton and some other Protons) runs a fix by the game's
UMU_ID and STORE: gamefixes-<store>/<umu id>.py for a store it knows, else gamefixes-umu/.
A row is kept when that file exists and does something (some are left empty on purpose:
"this game exists in GOG"). The STORE column is what to set for protonfixes to find it: the
store, or "-" for none (the fix is in gamefixes-umu).
"""
import ast, csv, os, sys

STORES = {"amazon", "battlenet", "ea", "egs", "gog", "humble", "itchio", "steam", "ubisoft", "zoomplatform"}


def does_something(path):
    """The fix's main/early functions hold more than a docstring and `pass`."""
    tree = ast.parse(open(path, encoding="utf-8").read())
    for node in tree.body:
        if isinstance(node, ast.FunctionDef) and node.name in ("main", "early", "main_with_id", "early_with_id"):
            for stmt in node.body:
                if isinstance(stmt, ast.Pass) or (isinstance(stmt, ast.Expr) and isinstance(stmt.value, ast.Constant)):
                    continue
                return True
    return False


database, fixes, out = sys.argv[1:4]
rows = set()
for row in csv.DictReader(open(database, newline="", encoding="utf-8")):
    store, codename, umu_id = row["STORE"].strip(), row["CODENAME"].strip(), row["UMU_ID"].strip()
    exe = (row.get("EXE_STRINGS (Optional)") or "").strip()
    if not umu_id.startswith("umu-") or not (codename not in ("", "none") or exe):
        continue
    for env, folder in ([(store, store)] if store in STORES else []) + [("-", "umu")]:
        path = os.path.join(fixes, f"gamefixes-{folder}", f"{umu_id}.py")
        if os.path.isfile(path) and does_something(path):
            title = " ".join(row["TITLE"].split())
            rows.add((store, codename if codename != "none" else "-", umu_id, env, exe or "-", title))
            break

with open(out, "w", encoding="utf-8") as f:
    f.write("# store\tcodename\tumu id\tSTORE to set\tprogram path ending\ttitle (tools/umu-table.py, from umu-database and umu-protonfixes)\n")
    for r in sorted(rows):
        f.write("\t".join(r) + "\n")
print(len(rows), "games")
