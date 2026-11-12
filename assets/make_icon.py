"""Draws assets/icon.png (1024x1024): a stopwatch on a teal rounded square. No dependencies."""
import math, struct, zlib, pathlib

N = 1024
BG = (17, 94, 89); FG = (255, 255, 255); HAND = (250, 204, 21)

def inside_rounded(x, y, r=200):
    cx = min(max(x, r), N - 1 - r); cy = min(max(y, r), N - 1 - r)
    return (x - cx) ** 2 + (y - cy) ** 2 <= r * r

def seg_dist(px, py, ax, ay, bx, by):
    dx, dy = bx - ax, by - ay
    t = max(0, min(1, ((px - ax) * dx + (py - ay) * dy) / (dx * dx + dy * dy)))
    return math.hypot(px - ax - t * dx, py - ay - t * dy)

cx, cy, R = 512, 560, 300
rows = []
for y in range(N):
    row = bytearray([0])
    for x in range(N):
        if not inside_rounded(x, y):
            row += bytes((0, 0, 0, 0)); continue
        c = BG
        d = math.hypot(x - cx, y - cy)
        if R - 45 <= d <= R: c = FG
        if 452 <= x <= 572 and 170 <= y <= 230: c = FG          # crown
        if 492 <= x <= 532 and 225 <= y <= 262: c = FG          # stem
        if seg_dist(x, y, cx, cy, cx, cy - 210) <= 22: c = FG   # minute hand
        if seg_dist(x, y, cx, cy, cx + 150, cy + 60) <= 22: c = HAND
        if d <= 38: c = HAND
        row += bytes((*c, 255))
    rows.append(bytes(row))

def chunk(t, d):
    return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)

png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", N, N, 8, 6, 0, 0, 0))
png += chunk(b"IDAT", zlib.compress(b"".join(rows), 9)) + chunk(b"IEND", b"")
pathlib.Path(__file__).with_name("icon.png").write_bytes(png)
