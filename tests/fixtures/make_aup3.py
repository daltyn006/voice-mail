#!/usr/bin/env python3
"""Generate synthetic Audacity projects (.aup3) for importer tests.

Independent binary-XML encoder written from the published ProjectSerializer
field layout (NOT from Audacity code): dict blob (FT_CharSize + FT_Name
records) + doc blob (typed tags). If our C++ decoder reads these, the
format contract holds.

Files written to tests/fixtures/:
  basic.aup3    8 kHz mono lecture: 2 float32 blocks (1 s 440 Hz sine each),
                track gain 0.5, plus a muted DC track (must be excluded).
  missing.aup3  references blockid 99 (absent) -> must render as silence.
  corrupt.aup3  random bytes (rejected).
  empty.aup3    valid SQLite/AUDY shell with empty project+autosave (rejected).
"""
import math
import os
import sqlite3
import struct
import sys

FIX = os.path.join(os.path.dirname(os.path.abspath(__file__)))

FT_CHAR_SIZE, FT_START, FT_END = 0, 1, 2
FT_STRING, FT_INT, FT_BOOL = 3, 4, 5
FT_LONG, FT_LONGLONG, FT_SIZET = 6, 7, 8
FT_FLOAT, FT_DOUBLE = 9, 10
FT_NAME = 15

SF_FLOAT = 0x4000F

RATE = 8000


class Encoder:
    def __init__(self):
        self.names = {}
        self.dict = bytearray()
        self.doc = bytearray()

    def nid(self, name):
        if name not in self.names:
            i = len(self.names)
            self.names[name] = i
            raw = name.encode("utf-16-le")
            self.dict += struct.pack("<BHH", FT_NAME, i, len(raw)) + raw
        return self.names[name]

    def _u16(self, buf, v):
        buf += struct.pack("<H", v)

    def start(self, name):
        self.doc += struct.pack("<B", FT_START)
        self._u16(self.doc, self.nid(name))

    def end(self, name):
        self.doc += struct.pack("<B", FT_END)
        self._u16(self.doc, self.nid(name))

    def wstr(self, name, value):
        raw = value.encode("utf-16-le")
        self.doc += struct.pack("<B", FT_STRING)
        self._u16(self.doc, self.nid(name))
        self.doc += struct.pack("<i", len(raw)) + raw

    def dbl(self, name, value):
        self.doc += struct.pack("<B", FT_DOUBLE)
        self._u16(self.doc, self.nid(name))
        self.doc += struct.pack("<d", value) + struct.pack("<i", 19)

    def i64(self, name, value):
        self.doc += struct.pack("<B", FT_LONGLONG)
        self._u16(self.doc, self.nid(name))
        self.doc += struct.pack("<q", value)

    def u32(self, name, value):
        self.doc += struct.pack("<B", FT_SIZET)
        self._u16(self.doc, self.nid(name))
        self.doc += struct.pack("<I", value)

    def boolean(self, name, value):
        self.doc += struct.pack("<B", FT_BOOL)
        self._u16(self.doc, self.nid(name))
        self.doc += struct.pack("<B", 1 if value else 0)

    def long(self, name, value):
        self.doc += struct.pack("<B", FT_LONG)
        self._u16(self.doc, self.nid(name))
        self.doc += struct.pack("<i", value)


def sine_block(freq, secs, amp=0.5):
    n = int(RATE * secs)
    return struct.pack("<%df" % n, *[amp * math.sin(2 * math.pi * freq * i / RATE) for i in range(n)])


def make_db(path, project_tracks, blocks, use_autosave=False):
    if os.path.exists(path):
        os.remove(path)
    con = sqlite3.connect(path)
    con.execute("PRAGMA application_id = 1096107097")  # 'AUDY' = 0x41554459
    con.execute("PRAGMA user_version = 50331648")  # 3.0.0 packed
    con.execute("CREATE TABLE project (id INTEGER PRIMARY KEY, dict BLOB, doc BLOB)")
    con.execute("CREATE TABLE autosave (id INTEGER PRIMARY KEY, dict BLOB, doc BLOB)")
    con.execute(
        "CREATE TABLE sampleblocks (blockid INTEGER PRIMARY KEY AUTOINCREMENT,"
        " sampleformat INTEGER, summin REAL, summax REAL, sumrms REAL,"
        " summary256 BLOB, summary64k BLOB, samples BLOB)"
    )
    e = Encoder()
    e.dict += struct.pack("<BB", FT_CHAR_SIZE, 2)
    e.start("project")
    e.wstr("version", "1.3.0")
    e.wstr("audacityversion", "3.7.5")
    e.dbl("rate", float(RATE))
    for tr in project_tracks:
        e.start("wavetrack")
        e.wstr("name", tr["name"])
        e.long("channel", tr.get("channel", 0))
        e.boolean("mute", tr.get("mute", False))
        e.boolean("solo", tr.get("solo", False))
        e.dbl("rate", float(RATE))
        e.dbl("gain", tr.get("gain", 1.0))
        e.dbl("pan", tr.get("pan", 0.0))
        e.long("sampleformat", SF_FLOAT)
        for cl in tr["clips"]:
            e.start("waveclip")
            e.dbl("offset", cl.get("offset", 0.0))
            e.dbl("trimLeft", 0.0)
            e.dbl("trimRight", 0.0)
            e.start("sequence")
            e.u32("maxsamples", 1048576)
            e.i64("numsamples", cl["numsamples"])
            e.u32("sampleformat", SF_FLOAT)
            for start, bid in cl["blocks"]:
                e.start("waveblock")
                e.i64("start", start)
                e.i64("blockid", bid)
                e.end("waveblock")
            e.end("sequence")
            e.start("envelope")
            e.u32("numpoints", 2)
            for t, v in [(0.0, 1.0), (2.0, 1.0)]:
                e.start("controlpoint")
                e.dbl("t", t)
                e.dbl("val", v)
                e.end("controlpoint")
            e.end("envelope")
            e.end("waveclip")
        e.end("wavetrack")
    e.end("project")
    table = "autosave" if use_autosave else "project"
    con.execute(f"INSERT INTO {table} (id, dict, doc) VALUES (1, ?, ?)", (bytes(e.dict), bytes(e.doc)))
    other = "project" if use_autosave else "autosave"
    con.execute(f"INSERT INTO {other} (id) VALUES (1)")
    for bid, payload in blocks:
        con.execute(
            "INSERT INTO sampleblocks (blockid, sampleformat, summin, summax, sumrms, samples)"
            " VALUES (?, ?, 0, 0, 0, ?)",
            (bid, SF_FLOAT, payload),
        )
    con.commit()
    con.close()


def main():
    os.makedirs(FIX, exist_ok=True)
    # basic: 2 s lecture (gain 0.5) + muted DC track (must vanish from mix).
    b1 = sine_block(440.0, 1.0)
    b2 = sine_block(440.0, 1.0)
    dc = struct.pack("<8000f", *[0.9] * 8000)
    make_db(
        os.path.join(FIX, "basic.aup3"),
        [
            {"name": "Lecture", "gain": 0.5,
             "clips": [{"numsamples": 16000, "blocks": [(0, 1), (8000, 2)]}]},
            {"name": "Muted", "mute": True,
             "clips": [{"numsamples": 8000, "blocks": [(0, 3)]}]},
        ],
        [(1, b1), (2, b2), (3, dc)],
    )
    # missing: references absent blockid 99 -> silence, still renders.
    make_db(
        os.path.join(FIX, "missing.aup3"),
        [{"name": "Gappy", "clips": [{"numsamples": 8000, "blocks": [(0, 99)]}]}],
        [],
    )
    with open(os.path.join(FIX, "corrupt.aup3"), "wb") as f:
        f.write(os.urandom(2048))
    # empty: valid shell, no doc.
    p = os.path.join(FIX, "empty.aup3")
    if os.path.exists(p):
        os.remove(p)
    con = sqlite3.connect(p)
    con.execute("PRAGMA application_id = 1096107097")  # 'AUDY' = 0x41554459
    con.execute("CREATE TABLE project (id INTEGER PRIMARY KEY, dict BLOB, doc BLOB)")
    con.execute("CREATE TABLE autosave (id INTEGER PRIMARY KEY, dict BLOB, doc BLOB)")
    con.execute(
        "CREATE TABLE sampleblocks (blockid INTEGER PRIMARY KEY AUTOINCREMENT,"
        " sampleformat INTEGER, summin REAL, summax REAL, sumrms REAL,"
        " summary256 BLOB, summary64k BLOB, samples BLOB)"
    )
    con.execute("INSERT INTO project (id) VALUES (1)")
    con.execute("INSERT INTO autosave (id) VALUES (1)")
    con.commit()
    con.close()
    print("fixtures written to", FIX)


if __name__ == "__main__":
    sys.exit(main())
