#!/usr/bin/env python3
"""
The most expensive source lines in a callgrind profile, by instructions executed.

Usage: hot_lines.py CALLGRIND_OUT [HOW_MANY]
The profile must be written with --compress-strings=no --compress-pos=no.
Inlined code is charged to the line it was written on, which is the point:
after inlining nearly everything is "in" two or three functions.
"""
import collections
import os
import sys


def main():
    path = sys.argv[1]
    top = int(sys.argv[2]) if len(sys.argv) > 2 else 60
    events = []
    cost = collections.defaultdict(lambda: None)
    current = None
    skip = False
    for raw in open(path, errors="replace"):
        line = raw.rstrip("\n")
        if line.startswith("events:"):
            events = line.split()[1:]
        elif line.startswith(("fl=", "fi=", "fe=")):
            current = line[3:]
        elif line.startswith("calls="):
            skip = True                      # the next line is the callee's inclusive cost
        elif line and line[0].isdigit():
            if skip:
                skip = False
                continue
            parts = line.split()
            key = (current, int(parts[0]))
            numbers = [int(x) for x in parts[1:]]
            had = cost[key]
            if had is None:
                cost[key] = numbers + [0] * (len(events) - len(numbers))
            else:
                for i, n in enumerate(numbers):
                    had[i] += n
    if not events:
        sys.exit("no events line: is this a callgrind profile?")
    index = {name: i for i, name in enumerate(events)}
    total = [sum(c[i] for c in cost.values()) for i in range(len(events))]
    ir = index["Ir"]

    def column(name):
        return index.get(name)

    shown = [n for n in ("Ir", "Bcm", "Bim", "D1mr", "D1mw", "DLmr", "DLmw") if column(n) is not None]
    print("totals: " + ", ".join(f"{n} {total[index[n]]:,}" for n in shown))
    by_file = collections.Counter()
    for (name, _), c in cost.items():
        by_file[name] += c[ir]
    print("\nby file:")
    for name, n in by_file.most_common(12):
        print(f"  {100 * n / total[ir]:5.1f}%  {name}")
    print(f"\ntop {top} lines by instructions ({' '.join(shown)}):")
    sources = {}
    for (name, number), c in sorted(cost.items(), key=lambda kv: -kv[1][ir])[:top]:
        if name not in sources:
            try:
                sources[name] = open(name, errors="replace").read().split("\n")
            except OSError:
                sources[name] = None
        text = sources[name][number - 1].strip() if sources[name] and 0 < number <= len(sources[name]) else ""
        short = os.path.relpath(name) if name and os.path.exists(name) else (name or "?").split("/library/")[-1]
        numbers = " ".join(f"{c[index[n]]:>12,}" for n in shown)
        print(f"{100 * c[ir] / total[ir]:5.2f}% {numbers}  {short}:{number}  {text[:110]}")


if __name__ == "__main__":
    main()
