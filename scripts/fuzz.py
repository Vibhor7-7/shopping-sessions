#!/usr/bin/env python3
"""
Dirties a generated dataset far beyond what the generator does, so the fast
pipeline and the reference can be compared on every corner of the CSV dialect
and the timestamp rules: stray quotes, spaces, `\\r`, non-ASCII digits, long
IDs, odd offsets, files with no final newline, empty files.

Usage: fuzz.py SOURCE_DIR TARGET_DIR SEED
Only the standard library, so it runs on any Python 3.
"""
import os
import random
import shutil
import sys

TIMESTAMPS = [
    b"", b"0", b"86400000", b"179102160000000000000", b"99999999999999999", b"999999999999999999",
    b"1791071890000.0", b"1.79e12", b"-5", b"+1791071890000", b" 1791071890000", b"1791071890000 ",
    "１791071890000".encode(), "١٧٩١٠٢١٦٠٠٠٠٠".encode(), b"2026-13-03T23:58:10Z", b"2026-02-30T23:58:10Z",
    b"2027-02-29T10:00:00Z", b"2028-02-29T10:00:00Z", b"2026-10-03T24:00:00Z", b"2026-10-03T23:60:00Z",
    b"2026-10-03T23:59:60Z", b"2026-10-03t23:58:10Z", b"2026-10-03T23:58:10z", b"2026-10-03 23:58:10Z",
    b"2026-10-03T23:58:10", b"2026-10-03T23:58:10.000Z", b"2026-10-03T23:58:10+24:00",
    b"2026-10-03T23:58:10+05:60", b"2026-10-03T23:58:10+0200", b"2026-10-03T23:58:10-00:00",
    b"0000-01-01T00:00:00Z", b"9999-12-31T23:59:59Z", b"2026-10-03T23:58:10Z\r", b"T", b"Z",
]
PRICES = [b"", b"0", b"00", b"1", b"0250", b"10000000", b"10000001", b"0000000000010000000", b"00000000000100000000",
          b"19.99", b"-5", b"+500", b"1e3", b" 250", b"250 ", "２５０".encode(), "٢٥٠".encode(), b"250\r", b"99999999999999999999"]
TYPES = [b"view", b"cart", b"purchase", b"Purchase", b"VIEW", b"cart ", b" view", b"views", b"", b"purchas", b"purchases", b"viewcart"]
# (event_id, ingest_ts) of every changed event copy so far: no two may be the same.
MOVED = set()


def window(source):
    import json
    import datetime
    manifest = json.load(open(os.path.join(source, "manifest.json")))
    def ms(text):
        return int(datetime.datetime.strptime(text, "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=datetime.timezone.utc).timestamp() * 1000)
    return ms(manifest["window_start"]), ms(manifest["window_end"])


def spell(rng, at_ms):
    """A valid spelling of an instant, in any of the three forms."""
    import datetime
    form = rng.randrange(4)
    if form == 0 or at_ms % 1000:
        return (b"0" * rng.choice([0, 0, 1, 5, 30])) + str(at_ms).encode()
    if form == 1:
        return datetime.datetime.fromtimestamp(at_ms // 1000, datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ").encode()
    offset = rng.choice([0, 1, 59, 60, 330, 720, 840, 1439])
    sign = rng.choice([1, -1])
    local = datetime.datetime.fromtimestamp(at_ms // 1000 + sign * offset * 60, datetime.timezone.utc)
    return local.strftime("%Y-%m-%dT%H:%M:%S").encode() + (b"+" if sign > 0 else b"-") + b"%02d:%02d" % divmod(offset, 60)


def odd_id(rng, value):
    choice = rng.randrange(12)
    if choice == 0: return b" " + value
    if choice == 1: return value + b" "
    if choice == 2: return value.upper()
    if choice == 3: return value + b"\t"
    if choice == 4: return value * 3                       # too long to pack
    if choice == 5: return value[:1]
    if choice == 6: return value + "é".encode()
    if choice == 7: return value[:10]
    if choice == 8: return (value * 2)[:11]
    if choice == 9: return b"_" + value[1:]
    if choice == 10: return value + b"-x"
    return b""


def mutate(rng, line, kind, start, end, serial):
    fields = line.split(b",")
    plain = [f[1:-1] if len(f) >= 2 and f[:1] == b'"' and f[-1:] == b'"' else f for f in fields]
    width = {"events": 7, "clicks": 4, "links": 2}[kind]
    if len(fields) != width or any(b'"' in f for f in plain):
        return [line]
    choice = rng.randrange(100)
    out = list(plain)
    if kind == "clicks":
        out[0] = out[0] + b"f%d" % serial                  # a fresh click_id: copies of a click must stay identical
    if choice < 12:      # structure
        pick = rng.randrange(9)
        if pick == 0: return [b",".join(out[:-1])]
        if pick == 1: return [b",".join(out) + b","]
        if pick == 2: return [b'"' + b",".join(out)]
        if pick == 3: return [b",".join(out) + b'"']
        if pick == 4: return [b""]
        if pick == 5:
            # Still a well-formed line, with a `\r` on its last field: it goes on to the
            # shared ending below, which keeps a changed event copy from tying with another.
            out[-1] = out[-1] + b"\r"
        if pick == 6: out[rng.randrange(width)] = b'a"b'; return [b",".join(out)]
        if pick == 7: at = rng.randrange(width); return [b",".join(out[:at] + [b'""' + out[at] + b'""'] + out[at + 1:])]
        if pick == 8: return [b",".join(b'"' + f + b'"' for f in out)]
    elif choice < 40:    # IDs
        at = rng.randrange(2) if kind != "links" else rng.randrange(2)
        out[at] = odd_id(rng, out[at])
        if kind != "links" and at == 0 and out[0]:
            # Still one of a kind: rules.md promises copies of a click are identical, and
            # that copies of an event with one ingest_ts are, so the fuzzer must not break either.
            out[0] += b"f%d" % serial
    elif kind == "links":
        out[rng.randrange(2)] = odd_id(rng, out[rng.randrange(2)])
    elif choice < 75:    # timestamps
        at = 2 if kind == "events" else 3
        edge = rng.choice([start, start - 1, start + 1, end, end - 1, end + 1, start - 604_800_000, start - 604_800_001,
                           rng.randrange(start - 700_000_000, end + 100_000_000)])
        out[at] = rng.choice(TIMESTAMPS) if rng.randrange(3) == 0 else spell(rng, edge)
    elif kind == "events":
        pick = rng.randrange(4)
        if pick == 0: out[4] = rng.choice(TYPES)
        elif pick == 1: out[4] = b"purchase"; out[6] = rng.choice(PRICES)
        elif pick == 2: out[5] = rng.choice([b"", b" p1", b"P1", b"p1", b"p50000", b"p50001", b"p00001", b"p1\r", out[5] * 4])
        else: out[6] = rng.choice(PRICES)
    else:
        out[2] = rng.choice([b"", b"c_1", b"C_1", b" c_1", b"c_1 ", b"c_2000", b"c_2001", b"c_01", b"organic", out[2] * 4])
    if kind == "events" and out[0] == plain[0]:
        # The event_id is unchanged, so this may be one copy of several. rules.md promises that
        # copies with one ingest_ts are identical; a changed copy must not share its ingest_ts
        # with another. Copies are ingested within 600 s of each other, so moving this one by
        # 700 s or more, in steps of 1300 s, cannot land it on another copy's.
        import datetime
        try:
            ingest = datetime.datetime.strptime(out[3].decode(), "%Y-%m-%dT%H:%M:%SZ")
            delta = rng.choice([1, -1]) * (700 + (serial % 97) * 1300)
            # Except on that of a copy that began with the same ingest_ts (the generator writes
            # such identical copies) and was moved by the same step, as one such pair in 194
            # is: seed 1795388356 made an event whose two earliest copies tied, one valid
            # and one not. Such a copy takes 97 more steps, where no other copy can be.
            while (out[0], ingest + datetime.timedelta(seconds=delta)) in MOVED:
                delta += (1 if delta > 0 else -1) * 97 * 1300
            MOVED.add((out[0], ingest + datetime.timedelta(seconds=delta)))
            out[3] = (ingest + datetime.timedelta(seconds=delta)).strftime("%Y-%m-%dT%H:%M:%SZ").encode()
        except ValueError:
            pass
    quoted = [b'"' + f + b'"' if rng.randrange(4) == 0 else f for f in out]
    return [b",".join(quoted)]


def main():
    source, target, seed = sys.argv[1], sys.argv[2], int(sys.argv[3])
    rng = random.Random(seed)
    start, end = window(source)
    shutil.rmtree(target, ignore_errors=True)
    os.makedirs(target)
    for name in ("manifest.json", "products.csv", "campaigns.csv"):
        shutil.copy(os.path.join(source, name), os.path.join(target, name))
    serial = 0
    for kind in ("events", "clicks", "links"):
        os.makedirs(os.path.join(target, kind))
        for name in sorted(os.listdir(os.path.join(source, kind))):
            data = open(os.path.join(source, kind, name), "rb").read()
            lines = data.split(b"\n")
            if lines and lines[-1] == b"":
                lines.pop()
            out = lines[:1]
            for line in lines[1:]:
                if rng.randrange(100) < 30:
                    serial += 1
                    out.extend(mutate(rng, line, kind, start, end, serial))
                else:
                    out.append(line)
            if kind == "links" and rng.randrange(3) == 0:
                out.append(out[0])                          # a data row equal to the header
            ending = rng.choice([b"\n", b"\n", b"", b"\n\n"])
            body = b"\n".join(out) + (ending if out else b"")
            if rng.randrange(12) == 0:
                body = b""                                  # an empty file: no header either
            open(os.path.join(target, kind, name), "wb").write(body)
        open(os.path.join(target, kind, "ignored.txt"), "wb").write(b"not,a,csv\n")
        open(os.path.join(target, kind, ".hidden.csv"), "wb").write(b"h\nnot,read\n")


if __name__ == "__main__":
    main()
