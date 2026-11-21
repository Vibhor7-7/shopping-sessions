#!/usr/bin/env python3
"""A dataset in which nearly every event has a user of its own.

The organizers' generator gives a person about 30 events, so the table that
numbers a partition's people (src/person.rs, `Group`) never outgrows its first
size on generated data: it is sized for a person every 12 events. Here most
people have one or two events, and the table grows two or three times in the
middle of the pass that fills it, which is the one moment the pass's copies of
the table's addresses go stale (BENCH.md, experiment 41).

    scripts/many_people.py OUT [EVENTS] [SEED]

Writes a dataset in the layout rules.md describes: clean lines only, since the
dirt is what the fixtures and the fuzzer are for.
"""
import os
import random
import sys
from datetime import datetime, timedelta, timezone

out = sys.argv[1]
events = int(sys.argv[2]) if len(sys.argv) > 2 else 60000
seed = int(sys.argv[3]) if len(sys.argv) > 3 else 1
rng = random.Random(seed)

start = datetime(2026, 10, 1, tzinfo=timezone.utc)
days = 14
DIGITS = "0123456789abcdefghijklmnopqrstuvwxyz"


def base36(n):
    text = ""
    while True:
        text = DIGITS[n % 36] + text
        n //= 36
        if n == 0:
            return text


def stamp(at):
    return at.strftime("%Y-%m-%dT%H:%M:%SZ")


for folder in ("events", "clicks", "links"):
    os.makedirs(os.path.join(out, folder), exist_ok=True)

with open(os.path.join(out, "manifest.json"), "w") as f:
    f.write('{"window_start": "%s", "window_end": "%s"}\n' % (stamp(start), stamp(start + timedelta(days=days))))
categories = ["garden", "apparel", "toys", "books", "audio"]
with open(os.path.join(out, "products.csv"), "w") as f:
    f.write("product_id,category\n")
    for p in range(1, 201):
        f.write("p%d,%s\n" % (p, categories[p % len(categories)]))
with open(os.path.join(out, "campaigns.csv"), "w") as f:
    f.write("campaign_id,channel\n")
    for c in range(1, 41):
        f.write("c_%d,%s\n" % (c, ["email", "search", "social"][c % 3]))

# Users: one for nearly every event. A few get a second and third event minutes
# later (one session), a few an event hours later (another session).
users = []
files = [open(os.path.join(out, "events", "part-%d.csv" % i), "w") for i in range(3)]
for f in files:
    f.write("event_id,user_id,ts,ingest_ts,event_type,product_id,price_cents\n")
number = 0
written = 0
while written < events:
    user = "u" + base36(1_000_000 + number * 7919 % 50_000_000)
    number += 1
    users.append(user)
    at = start + timedelta(seconds=rng.randrange(days * 86400))
    burst = rng.choice([1, 1, 1, 1, 2, 3])
    for _ in range(burst):
        kind = rng.choice(["view", "view", "view", "cart", "purchase"])
        price = str(rng.randrange(100, 50000)) if kind == "purchase" else ""
        if at < start + timedelta(days=days):
            files[written % 3].write(
                "e%s,%s,%s,%s,%s,p%d,%s\n"
                % (base36(written + 5_000_000), user, stamp(at), stamp(at + timedelta(seconds=rng.randrange(600))), kind, rng.randrange(1, 220), price)
            )
            written += 1
        at += timedelta(seconds=rng.choice([20, 90, 400, 1800, 1801, 7200]))
for f in files:
    f.close()

with open(os.path.join(out, "clicks", "part-0.csv"), "w") as f:
    f.write("click_id,user_id,campaign_id,ts\n")
    for k in range(events // 4):
        at = start - timedelta(days=7) + timedelta(seconds=rng.randrange((days + 7) * 86400))
        f.write("k%s,%s,c_%d,%s\n" % (base36(k + 9_000_000), rng.choice(users), rng.randrange(1, 43), stamp(at)))

with open(os.path.join(out, "links", "part-0.csv"), "w") as f:
    f.write("user_id_a,user_id_b\n")
    for _ in range(events // 20):
        f.write("%s,%s\n" % (rng.choice(users), rng.choice(users)))
