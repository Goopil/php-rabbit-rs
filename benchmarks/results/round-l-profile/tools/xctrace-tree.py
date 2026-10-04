#!/usr/bin/env python3
"""Round L profile — xctrace time-profile post-processor.

Parses `xctrace export --xpath .../table[@schema="time-profile"]` XML and
aggregates per-symbol self samples (leaf frames) and total samples (frames
anywhere in the stack), per thread. Prints a call-tree summary.

Usage: xctrace-tree.py <export.xml> [--top 25] [--min-percent 0.5]
"""
import sys
import xml.etree.ElementTree as ET
from collections import Counter, defaultdict


def main() -> None:
    path = sys.argv[1]
    top = 25
    min_pct = 0.5
    args = sys.argv[2:]
    for i, a in enumerate(args):
        if a == "--top":
            top = int(args[i + 1])
        if a == "--min-percent":
            min_pct = float(args[i + 1])

    frames: dict[str, str] = {}          # id -> "name [binary]"
    thread_names: dict[str, str] = {}
    binaries: dict[str, str] = {}
    self_samples: Counter[str] = Counter()          # (thread, frame_id)
    total_samples: Counter[str] = Counter()
    thread_self: Counter[str] = Counter()
    thread_total: Counter[str] = Counter()
    total_rows = 0

    for event, elem in ET.iterparse(path, events=("end",)):
        if elem.tag == "row":
            total_rows += 1
            th = elem.find("thread")
            thread_ref = th.get("ref") if th is not None else "?"
            bt = elem.find("tagged-backtrace/backtrace")
            if bt is None:
                elem.clear()
                continue
            stack = []
            for fr in bt.findall("frame"):
                fid = fr.get("id") or fr.get("ref")
                name = fr.get("name")
                if name is None and fid in frames:
                    name = frames[fid]
                binref = fr.find("binary")
                bname = ""
                if binref is not None:
                    bid = binref.get("id") or binref.get("ref")
                    bname = binaries.get(bid, "")
                label = f"{name} [{bname}]" if name else f"<{fid}> [{bname}]"
                if fid and name:
                    frames[fid] = label
                stack.append(label)
            if stack:
                leaf = stack[0]
                self_samples[(thread_ref, leaf)] += 1
                thread_self[thread_ref] += 1
                seen = set()
                for label in stack:
                    # Normalize away xctrace's deduplicated backtrace-prefix
                    # chain markers ("name [] []") so inclusive totals
                    # aggregate per symbol, not per unique stack path.
                    symbol = label.split(" []")[0] if " []" in label else label
                    if symbol not in seen:
                        total_samples[(thread_ref, symbol)] += 1
                        seen.add(symbol)
                thread_total[thread_ref] += 1
            elem.clear()
        elif elem.tag == "frame" and elem.get("id"):
            bid = ""
            b = elem.find("binary")
            if b is not None:
                bid = b.get("id") or b.get("ref")
            frames[elem.get("id")] = f"{elem.get('name')} [{binaries.get(bid, '')}]"
        elif elem.tag == "binary" and elem.get("id"):
            binaries[elem.get("id")] = elem.get("name") or ""
        elif elem.tag == "thread" and elem.get("id"):
            thread_names[elem.get("id")] = elem.get("name") or elem.get("fmt") or ""

    if total_rows == 0:
        print("no rows parsed")
        return

    print(f"// total samples: {total_rows} (1 sample = 1 profiling interval)")
    for th in sorted(thread_total, key=lambda t: -thread_total[t]):
        tname = thread_names.get(th, "?")
        print(f"\n=== thread {th} ({tname}) — {thread_total[th]} samples ===")
        print("-- self time (leaf frames) --")
        agg = Counter()
        for (t, label), n in self_samples.items():
            if t == th:
                agg[label] += n
        for label, n in agg.most_common(top):
            pct = 100.0 * n / thread_total[th]
            if pct < min_pct:
                break
            print(f"  {n:6d}  {pct:5.1f}%  {label}")
        print("-- total time (inclusive) --")
        agg = Counter()
        for (t, label), n in total_samples.items():
            if t == th:
                agg[label] += n
        for label, n in agg.most_common(top):
            pct = 100.0 * n / thread_total[th]
            if pct < min_pct:
                break
            print(f"  {n:6d}  {pct:5.1f}%  {label}")


if __name__ == "__main__":
    main()
