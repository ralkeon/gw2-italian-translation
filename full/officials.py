"""Official GW2 names in English, German, French and Spanish, from the public API.

The game's own translations show how a name was treated (translated or kept as a proper
name) and what it means. They never give the Italian word to use (TASTE.md rule 4).

  python officials.py build [db]          download or refresh the archive (only new ids)
  python officials.py find NAME [db]      exact match first, then names containing NAME

The archive is game text (ArenaNet's): it stays local, *.db is gitignored.
"""
import concurrent.futures as cf
import json
import os
import sqlite3
import sys
import time
import urllib.request

API = "https://api.guildwars2.com/v2/"
LANGS = ("en", "de", "fr", "es")
ENDPOINTS = (
    "items", "skins", "skills", "traits", "specializations", "professions", "maps",
    "masteries", "currencies", "achievements", "achievements/categories", "titles",
    "minis", "mounts/types", "mounts/skins", "gliders", "outfits", "novelties", "pets",
    "itemstats", "legendaryarmory", "colors", "finishers", "mailcarriers", "jadebots",
    "skiffs", "homestead/decorations", "wizardsvault/listings",
)
HERE = os.path.dirname(os.path.abspath(__file__))
DEFAULT_DB = os.path.join(HERE, "officials.db")


def get(url):
    err = None
    for attempt in range(5):
        try:
            req = urllib.request.Request(url, headers={"User-Agent": "gw2-glossary-script"})
            with urllib.request.urlopen(req, timeout=90) as r:
                return json.load(r)
        except urllib.error.HTTPError as e:
            err = e
            if e.code == 404:
                return None
            time.sleep(2 + attempt * 3)
        except Exception as e:
            err = e
            time.sleep(2 + attempt * 3)
    raise err


def name_of(x):
    return (x.get("name") or x.get("title") or "").strip()


def build(db_path):
    db = sqlite3.connect(db_path)
    db.execute("create table if not exists names (endpoint text, id text, en text, de text, fr text, es text, primary key (endpoint, id))")
    db.execute("create index if not exists names_en on names (en collate nocase)")
    for ep in ENDPOINTS:
        ids = get(API + ep)
        if not ids or not isinstance(ids, list):
            print(f"{ep}: skipped")
            continue
        ids = [str(i) for i in ids]
        have = {r[0] for r in db.execute("select id from names where endpoint=?", (ep,))}
        new = [i for i in ids if i not in have]
        chunks = [new[i:i + 200] for i in range(0, len(new), 200)]

        def work(chunk):
            rows = {}
            for lang in LANGS:
                data = get(f"{API}{ep}?ids={','.join(chunk)}&lang={lang}") or []
                for x in data:
                    rows.setdefault(str(x["id"]), {})[lang] = name_of(x)
            return rows

        added = 0
        with cf.ThreadPoolExecutor(4) as ex:
            for rows in ex.map(work, chunks):
                for i, r in rows.items():
                    if r.get("en"):
                        db.execute("insert or replace into names values (?,?,?,?,?,?)",
                                   (ep, i, r.get("en"), r.get("de"), r.get("fr"), r.get("es")))
                        added += 1
                db.commit()
        print(f"{ep}: {len(ids)} ids, {added} added")
    db.close()


def find(name, db_path):
    db = sqlite3.connect(db_path)
    rows = db.execute("select endpoint, id, en, de, fr, es from names where en = ? collate nocase", (name,)).fetchall()
    exact = bool(rows)
    if not rows:
        rows = db.execute("select endpoint, id, en, de, fr, es from names where en like ? limit 15", (f"%{name}%",)).fetchall()
    seen = set()
    print("exact:" if exact else "containing:")
    for ep, i, en, de, fr, es in rows:
        key = (en, de, fr, es)
        if key in seen:
            continue
        seen.add(key)
        print(f"  [{ep} {i}] EN {en} | DE {de} | FR {fr} | ES {es}")
    if not rows:
        print("  nothing found")


if __name__ == "__main__":
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8")
    if len(sys.argv) >= 2 and sys.argv[1] == "build":
        build(sys.argv[2] if len(sys.argv) > 2 else DEFAULT_DB)
    elif len(sys.argv) >= 3 and sys.argv[1] == "find":
        find(sys.argv[2], sys.argv[3] if len(sys.argv) > 3 else DEFAULT_DB)
    else:
        print(__doc__)
