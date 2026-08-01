#!/usr/bin/env python3
"""Render the comparison results as SVG, from the numbers in results/.

Run: python3 charts.py   (writes ../docs/assets/*.svg)

The figures below are transcribed from
`results/2026-08-01-Davids-MBP.md` and nowhere else. Keeping them here rather
than parsing that file is deliberate — a chart that silently redraws itself when
a results file changes is a chart nobody can date. Change a number here only by
copying it from a results file, and say which one.

No dependencies: matplotlib would produce a raster or a 200KB vector, and these
are five bars. Hand-built SVG also lets the output carry its own light/dark
styling, which is what makes it legible in both GitHub themes.
"""

import pathlib

OUT = pathlib.Path(__file__).parent.parent / "docs" / "assets"

SOURCE = "benchmarks/results/2026-08-01-Davids-MBP.md"

# --- palette -----------------------------------------------------------------
#
# Two colours and a neutral. The neutral is deliberately achromatic: it is
# de-emphasis, not a series identity — every bar carries a direct value label,
# so nothing depends on telling two greys apart. Contrast against both surfaces
# is >= 3:1 and the accent/neutral pair separates at dE 15.9 under protanopia.
LIGHT = {
    "surface": "#fcfcfb",
    "ink": "#0b0b0b",
    "ink2": "#52514e",
    "muted": "#898781",
    "grid": "#e1e0d9",
    "axis": "#c3c2b7",
    "accent": "#2a78d6",
    "peer": "#898781",
    "series2": "#eb6834",
}
DARK = {
    "surface": "#1a1a19",
    "ink": "#ffffff",
    "ink2": "#c3c2b7",
    "muted": "#898781",
    "grid": "#2c2c2a",
    "axis": "#383835",
    "accent": "#3987e5",
    "peer": "#898781",
    "series2": "#d95926",
}

FONT = 'system-ui,-apple-system,"Segoe UI",Roboto,sans-serif'


def style_block() -> str:
    """One stylesheet, both themes.

    `prefers-color-scheme` inside an SVG follows the reader's OS setting even
    when the file is embedded with `<img>`, which is how GitHub renders it. The
    surface is painted rather than left transparent so the figure is legible
    whatever the page behind it is doing.
    """
    rules = []
    for cls, key in (
        ("s", "surface"),
        ("ink", "ink"),
        ("ink2", "ink2"),
        ("mut", "muted"),
        ("acc", "accent"),
        ("peer", "peer"),
        ("s2", "series2"),
    ):
        rules.append(f".{cls}{{fill:{LIGHT[key]}}}")
    rules.append(f".grid{{stroke:{LIGHT['grid']}}}")
    rules.append(f".axis{{stroke:{LIGHT['axis']}}}")
    rules.append(f".acc-s{{stroke:{LIGHT['accent']}}}")
    rules.append(f".s2-s{{stroke:{LIGHT['series2']}}}")
    rules.append(f".ring{{stroke:{LIGHT['surface']}}}")

    dark = []
    for cls, key in (
        ("s", "surface"),
        ("ink", "ink"),
        ("ink2", "ink2"),
        ("mut", "muted"),
        ("acc", "accent"),
        ("peer", "peer"),
        ("s2", "series2"),
    ):
        dark.append(f".{cls}{{fill:{DARK[key]}}}")
    dark.append(f".grid{{stroke:{DARK['grid']}}}")
    dark.append(f".axis{{stroke:{DARK['axis']}}}")
    dark.append(f".acc-s{{stroke:{DARK['accent']}}}")
    dark.append(f".s2-s{{stroke:{DARK['series2']}}}")
    dark.append(f".ring{{stroke:{DARK['surface']}}}")

    return (
        "<style>\n"
        f"text{{font-family:{FONT};}}\n"
        + "".join(rules)
        + "\n@media (prefers-color-scheme:dark){"
        + "".join(dark)
        + "}\n</style>"
    )


def esc(s: str) -> str:
    return (
        s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")
    )


def header(w: int, h: int, title: str, desc: str, slug: str) -> str:
    """The figure's frame, its accessible name, and its stylesheet.

    The `title`/`desc` ids carry a per-chart slug because ids are
    document-global: four figures inlined into one page would otherwise declare
    `id="t"` four times, and `aria-labelledby` would resolve every one of them
    to the first chart's title.
    """
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {h}" '
        f'width="{w}" height="{h}" role="img" '
        f'aria-labelledby="{slug}-t {slug}-d">\n'
        f"<title id=\"{slug}-t\">{esc(title)}</title>\n"
        f"<desc id=\"{slug}-d\">{esc(desc)}</desc>\n"
        f"{style_block()}\n"
        f'<rect class="s" width="{w}" height="{h}" rx="10"/>'
    )


def titles(title: str, subtitle: str, x: int = 28) -> str:
    return (
        f'<text class="ink" x="{x}" y="34" font-size="17" font-weight="600">{esc(title)}</text>'
        f'<text class="mut" x="{x}" y="55" font-size="12.5">{esc(subtitle)}</text>'
    )


def footnote(w: int, y: int, text: str, x: int = 28) -> str:
    return f'<text class="mut" x="{x}" y="{y}" font-size="11">{esc(text)}</text>'


def bar_path(x0: float, y: float, x1: float, h: float, r: float = 4) -> str:
    """A bar with its data-end rounded and its baseline end square."""
    width = x1 - x0
    if width <= r:
        return f'M{x0},{y} h{max(width, 0.8)} v{h} h{-max(width, 0.8)} Z'
    return (
        f"M{x0},{y} H{x1 - r} A{r},{r} 0 0 1 {x1},{y + r} "
        f"V{y + h - r} A{r},{r} 0 0 1 {x1 - r},{y + h} H{x0} Z"
    )


def fmt(n: float) -> str:
    if n >= 1_000_000:
        return f"{n / 1_000_000:.2f}M"
    if n >= 1_000:
        return f"{n / 1_000:.0f}k"
    return f"{n:g}"


# -----------------------------------------------------------------------------
# 1. Throughput
# -----------------------------------------------------------------------------

THROUGHPUT = [
    ("Churust", 3_101_706, True),
    ("actix-web", 3_038_968, False),
    ("Ktor (Netty)", 1_366_326, False),
    ("axum", 251_405, False),
    ("Go net/http", 97_619, False),
]


def chart_throughput() -> str:
    w, row, gap = 900, 24, 20
    top = 92
    h = top + len(THROUGHPUT) * (row + gap) + 52
    x0, x1 = 168, 700
    peak = max(v for _, v, _ in THROUGHPUT)

    out = [
        header(
            w,
            h,
            "Requests per second by framework, HTTP/1.1 pipelined at depth 64",
            "Churust 3.10M, actix-web 3.04M, Ktor 1.37M, axum 251k, Go net/http 98k "
            "requests per second. Higher is better.",
            "tp",
        ),
        titles(
            "Requests per second — HTTP/1.1 pipelined, depth 64",
            "Apple M2 Max, 12 cores · median of 3 rounds · higher is better",
        ),
    ]

    # Gridlines at clean values, behind the bars.
    for gv in (1_000_000, 2_000_000, 3_000_000):
        gx = x0 + (x1 - x0) * gv / peak
        out.append(
            f'<line class="grid" x1="{gx:.1f}" y1="{top - 8}" x2="{gx:.1f}" '
            f'y2="{top + len(THROUGHPUT) * (row + gap) - gap + 4}" stroke-width="1"/>'
        )
        out.append(
            f'<text class="mut" x="{gx:.1f}" y="{top - 14}" font-size="10.5" '
            f'text-anchor="middle">{gv // 1_000_000}M</text>'
        )

    y = top
    for name, value, is_subject in THROUGHPUT:
        bw = (x1 - x0) * value / peak
        cls = "acc" if is_subject else "peer"
        weight = "600" if is_subject else "400"
        ink = "ink" if is_subject else "ink2"
        out.append(
            f'<text class="{ink}" x="{x0 - 14}" y="{y + row / 2 + 4.5}" font-size="13" '
            f'font-weight="{weight}" text-anchor="end">{esc(name)}</text>'
        )
        out.append(f'<path class="{cls}" d="{bar_path(x0, y, x0 + bw, row)}"/>')
        out.append(
            f'<text class="{ink}" x="{x0 + bw + 10:.1f}" y="{y + row / 2 + 4.5}" '
            f'font-size="13" font-weight="{weight}">{fmt(value)}</text>'
        )
        y += row + gap

    out.append(
        f'<line class="axis" x1="{x0}" y1="{top - 8}" x2="{x0}" '
        f'y2="{y - gap + 4}" stroke-width="1"/>'
    )
    out.append(
        footnote(
            w,
            h - 22,
            "Every route returns a constant. One machine, one moment — not a general ranking. "
            "Method and caveats in benchmarks/results/.",
        )
    )
    out.append("</svg>")
    return "\n".join(out)


# -----------------------------------------------------------------------------
# 2. Depth sweep — the honesty chart
# -----------------------------------------------------------------------------

DEPTHS = [16, 64, 128, 256]
SWEEP = {
    "Churust": [772_040, 3_086_219, 4_524_352, 5_030_548],
    "actix-web": [795_389, 3_015_567, 5_116_388, 5_677_437],
}


def chart_depth_sweep() -> str:
    import math

    w, h = 900, 430
    left, right, top, bottom = 78, 782, 104, 322
    peak = 6_000_000

    def px(d: int) -> float:
        lo, hi = math.log2(DEPTHS[0]), math.log2(DEPTHS[-1])
        return left + (right - left) * (math.log2(d) - lo) / (hi - lo)

    def py(v: float) -> float:
        return bottom - (bottom - top) * v / peak

    out = [
        header(
            w,
            h,
            "Churust and actix-web throughput across HTTP/1.1 pipeline depths",
            "At depth 16 actix-web leads by 3 percent, at 64 Churust leads by 2 percent, "
            "at 128 and 256 actix-web leads by 13 percent.",
            "ds",
        ),
        titles(
            "The margin against actix-web changes hands with pipeline depth",
            "Requests per second · Churust leads at depth 64 only · higher is better",
        ),
    ]

    for gv in range(0, 6_000_001, 2_000_000):
        gy = py(gv)
        out.append(
            f'<line class="grid" x1="{left}" y1="{gy:.1f}" x2="{right + 8}" '
            f'y2="{gy:.1f}" stroke-width="1"/>'
        )
        out.append(
            f'<text class="mut" x="{left - 12}" y="{gy + 4:.1f}" font-size="10.5" '
            f'text-anchor="end">{gv // 1_000_000}M</text>'
        )

    for d in DEPTHS:
        out.append(
            f'<text class="mut" x="{px(d):.1f}" y="{bottom + 22}" font-size="11.5" '
            f'text-anchor="middle">{d}</text>'
        )
    out.append(
        f'<text class="ink2" x="{(left + right) / 2:.1f}" y="{bottom + 46}" '
        f'font-size="12" text-anchor="middle">pipeline depth (requests per batch)</text>'
    )
    out.append(
        f'<line class="axis" x1="{left}" y1="{bottom}" x2="{right + 8}" '
        f'y2="{bottom}" stroke-width="1"/>'
    )

    for name, cls, dot in (("Churust", "acc-s", "acc"), ("actix-web", "s2-s", "s2")):
        pts = " ".join(f"{px(d):.1f},{py(v):.1f}" for d, v in zip(DEPTHS, SWEEP[name]))
        out.append(
            f'<polyline class="{cls}" points="{pts}" fill="none" stroke-width="2" '
            f'stroke-linejoin="round" stroke-linecap="round"/>'
        )
        for d, v in zip(DEPTHS, SWEEP[name]):
            # A 2px surface ring keeps the markers legible where the two
            # series cross at depth 64.
            out.append(
                f'<circle class="ring" cx="{px(d):.1f}" cy="{py(v):.1f}" r="5.5" '
                f'fill="none" stroke-width="3"/>'
            )
            out.append(
                f'<circle class="{dot}" cx="{px(d):.1f}" cy="{py(v):.1f}" r="4.5"/>'
            )

    # End labels only: a value on every point would be eleven numbers of noise.
    out.append(
        f'<text class="ink" x="{px(256) + 14:.1f}" y="{py(SWEEP["Churust"][-1]) + 4:.1f}" '
        f'font-size="12" font-weight="600">5.03M</text>'
    )
    out.append(
        f'<text class="ink" x="{px(256) + 14:.1f}" y="{py(SWEEP["actix-web"][-1]) + 4:.1f}" '
        f'font-size="12" font-weight="600">5.68M</text>'
    )

    # Legend in the header, clear of the axis furniture at the foot.
    lx, ly = 618, 40
    out.append(f'<circle class="acc" cx="{lx}" cy="{ly - 4}" r="5"/>')
    out.append(f'<text class="ink2" x="{lx + 14}" y="{ly}" font-size="12.5">Churust</text>')
    out.append(f'<circle class="s2" cx="{lx + 96}" cy="{ly - 4}" r="5"/>')
    out.append(f'<text class="ink2" x="{lx + 110}" y="{ly}" font-size="12.5">actix-web</text>')

    out.append(
        footnote(
            w,
            h - 22,
            "Published in full rather than at the one depth Churust wins. "
            "Method and caveats in benchmarks/results/.",
        )
    )
    out.append("</svg>")
    return "\n".join(out)


# -----------------------------------------------------------------------------
# 3. CPU per request — where actix-web wins
# -----------------------------------------------------------------------------

CPU = [
    ("actix-web", 1.03, False),
    ("Churust", 1.87, True),
    ("Ktor (Netty)", 7.23, False),
    ("Go net/http", 32.67, False),
    ("axum", 40.51, False),
]


def chart_cpu() -> str:
    w, row, gap = 900, 24, 20
    top = 92
    h = top + len(CPU) * (row + gap) + 52
    x0, x1 = 168, 700
    peak = max(v for _, v, _ in CPU)

    out = [
        header(
            w,
            h,
            "Server CPU microseconds per request by framework",
            "actix-web 1.03, Churust 1.87, Ktor 7.23, Go net/http 32.67, axum 40.51 "
            "microseconds of CPU per request. Lower is better; actix-web is best.",
            "cpu",
        ),
        titles(
            "Server CPU per request — actix-web is the most efficient",
            "Microseconds of process CPU per request served, at depth 64 · lower is better",
        ),
    ]

    for gv in (10, 20, 30, 40):
        gx = x0 + (x1 - x0) * gv / peak
        out.append(
            f'<line class="grid" x1="{gx:.1f}" y1="{top - 8}" x2="{gx:.1f}" '
            f'y2="{top + len(CPU) * (row + gap) - gap + 4}" stroke-width="1"/>'
        )
        out.append(
            f'<text class="mut" x="{gx:.1f}" y="{top - 14}" font-size="10.5" '
            f'text-anchor="middle">{gv}</text>'
        )

    y = top
    for name, value, is_subject in CPU:
        bw = (x1 - x0) * value / peak
        cls = "acc" if is_subject else "peer"
        weight = "600" if is_subject else "400"
        ink = "ink" if is_subject else "ink2"
        out.append(
            f'<text class="{ink}" x="{x0 - 14}" y="{y + row / 2 + 4.5}" font-size="13" '
            f'font-weight="{weight}" text-anchor="end">{esc(name)}</text>'
        )
        out.append(f'<path class="{cls}" d="{bar_path(x0, y, x0 + bw, row)}"/>')
        out.append(
            f'<text class="{ink}" x="{x0 + bw + 10:.1f}" y="{y + row / 2 + 4.5}" '
            f'font-size="13" font-weight="{weight}">{value:g} µs</text>'
        )
        y += row + gap

    out.append(
        f'<line class="axis" x1="{x0}" y1="{top - 8}" x2="{x0}" '
        f'y2="{y - gap + 4}" stroke-width="1"/>'
    )
    out.append(
        footnote(
            w,
            h - 22,
            "The figure that survives a saturated network path — it does not depend on how fast the wire is."
        )
    )
    out.append("</svg>")
    return "\n".join(out)


# -----------------------------------------------------------------------------
# 4. Before and after
# -----------------------------------------------------------------------------


def chart_before_after() -> str:
    w, h = 900, 336
    x0, x1 = 168, 700
    row, gap, top = 28, 22, 96
    data = [("Before", 353_000, False), ("After", 3_101_706, True)]
    peak = 3_101_706

    out = [
        header(
            w,
            h,
            "Churust throughput before and after this work",
            "Churust went from 353 thousand to 3.10 million requests per second, "
            "an 8.8 times change, on the same benchmark and the same machine.",
            "ba",
        ),
        titles(
            "Churust, before and after this work — 8.8×",
            "Same benchmark, same machine, same routes · requests per second at depth 64",
        ),
    ]

    y = top
    for name, value, is_subject in data:
        bw = (x1 - x0) * value / peak
        cls = "acc" if is_subject else "peer"
        out.append(
            f'<text class="ink2" x="{x0 - 14}" y="{y + row / 2 + 4.5}" font-size="13" '
            f'text-anchor="end">{esc(name)}</text>'
        )
        out.append(f'<path class="{cls}" d="{bar_path(x0, y, x0 + bw, row)}"/>')
        out.append(
            f'<text class="ink" x="{x0 + bw + 10:.1f}" y="{y + row / 2 + 4.5}" '
            f'font-size="14" font-weight="600">{fmt(value)}</text>'
        )
        y += row + gap

    notes = [
        "Aggregating pipelined response flushes — 4.1×",
        "Removing per-request atomics on shared cache lines — 1.45× then 1.29×",
        "One runtime per core, connections pinned — 1.4×",
        "Dropping the per-request extension-map allocation — 1.29×",
    ]
    ny = y + 18
    out.append(
        f'<text class="ink2" x="{x0 - 14}" y="{ny}" font-size="12" font-weight="600" '
        f'text-anchor="end">What did it</text>'
    )
    for note in notes:
        out.append(f'<text class="mut" x="{x0}" y="{ny}" font-size="11.5">{esc(note)}</text>')
        ny += 17

    out.append(
        footnote(
            w,
            h - 20,
            "None of it in routing or extraction. Method and caveats in benchmarks/results/.",
        )
    )
    out.append("</svg>")
    return "\n".join(out)


def main() -> None:
    OUT.mkdir(parents=True, exist_ok=True)
    charts = {
        "benchmark-throughput.svg": chart_throughput(),
        "benchmark-depth-sweep.svg": chart_depth_sweep(),
        "benchmark-cpu-per-request.svg": chart_cpu(),
        "benchmark-before-after.svg": chart_before_after(),
    }
    for name, svg in charts.items():
        (OUT / name).write_text(svg + "\n", encoding="utf-8")
        print(f"wrote {OUT / name}")


if __name__ == "__main__":
    main()
