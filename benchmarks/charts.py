#!/usr/bin/env python3
"""Render the comparison results as SVG, from the numbers in results/.

Run: python3 charts.py   (writes ../docs/assets/*.svg)

The figures below are transcribed from
`results/2026-08-01-linux-docker.md` and nowhere else — the six-app run at four
workers, median of nine rounds. Before this they had drifted badly: the
throughput series said 699k/675k while its own alt text said 914k/880k, and
neither matched any run. A chart and its description disagreeing is worse than
either being stale, because a reader has no way to tell which one lied. Keeping them here rather
than parsing that file is deliberate — a chart that silently redraws itself when
a results file changes is a chart nobody can date. Change a number here only by
copying it from a results file, and say which one.

No dependencies: matplotlib would produce a raster or a 200KB vector, and these
are five bars. Hand-built SVG also lets the output carry its own light/dark
styling, which is what makes it legible in both GitHub themes.
"""

import pathlib

OUT = pathlib.Path(__file__).parent.parent / "docs" / "assets"

SOURCE = "benchmarks/results/2026-08-01-linux-docker.md"

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

# `bare hyper` is not a framework and is not a competitor: it is hyper and tokio
# with nothing on top, serving the same bytes, and it is the floor any
# hyper-based framework is measured against. It is drawn in the peer colour and
# named as the floor so nobody reads it as a sixth contender.
THROUGHPUT = [
    ("actix-web", 890_387, False),
    ("bare hyper (floor)", 873_574, False),
    ("Churust", 798_184, True),
    ("axum", 455_360, False),
    ("Go net/http", 310_501, False),
    ("Ktor (Netty)", 297_274, False),
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
            "Requests per second by framework, keep-alive, four workers each",
            "actix-web 890k, bare hyper 874k, Churust 798k, axum 455k, "
            "Go net/http 311k, Ktor 297k requests per second. Higher is better; "
            "actix-web leads and Churust is second of the frameworks.",
            "tp",
        ),
        titles(
            "Requests per second — keep-alive, four workers each",
            "Linux 6.12, 8 pinned cores · 64 connections · median of 9 rounds · higher is better",
        ),
    ]

    # Gridlines at clean values, behind the bars.
    for gv in (200_000, 400_000, 600_000):
        gx = x0 + (x1 - x0) * gv / peak
        out.append(
            f'<line class="grid" x1="{gx:.1f}" y1="{top - 8}" x2="{gx:.1f}" '
            f'y2="{top + len(THROUGHPUT) * (row + gap) - gap + 4}" stroke-width="1"/>'
        )
        out.append(
            f'<text class="mut" x="{gx:.1f}" y="{top - 14}" font-size="10.5" '
            f'text-anchor="middle">{gv // 1000}k</text>'
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
            "Each server at the worker count that suits it: actix-web 4, Churust 6, the rest their default. "
            "Every route returns a constant.",
        )
    )
    out.append("</svg>")
    return "\n".join(out)


# -----------------------------------------------------------------------------
# 2. Pipelined throughput — the mode Churust loses
# -----------------------------------------------------------------------------

PIPELINED = [
    ("actix-web", 5_955_207, False),
    ("Churust", 4_449_755, True),
    ("Ktor (Netty)", 1_227_741, False),
    ("Go net/http", 352_427, False),
    ("axum", 24_379, False),
]


def chart_pipelined() -> str:
    w, row, gap = 900, 24, 20
    top = 92
    h = top + len(PIPELINED) * (row + gap) + 66
    x0, x1 = 168, 700
    peak = max(v for _, v, _ in PIPELINED)

    out = [
        header(
            w,
            h,
            "Requests per second with HTTP/1.1 pipelining at depth 16",
            "actix-web 5.96M, Churust 4.45M, Ktor 1.23M, Go net/http 352k, axum 24k "
            "requests per second. actix-web leads this mode.",
            "pl",
        ),
        titles(
            "With pipelining, actix-web leads — Churust is second",
            "Requests per second at pipeline depth 16 · a client shape most traffic is not",
        ),
    ]

    for gv in (2_000_000, 4_000_000):
        gx = x0 + (x1 - x0) * gv / peak
        out.append(
            f'<line class="grid" x1="{gx:.1f}" y1="{top - 8}" x2="{gx:.1f}" '
            f'y2="{top + len(PIPELINED) * (row + gap) - gap + 4}" stroke-width="1"/>'
        )
        out.append(
            f'<text class="mut" x="{gx:.1f}" y="{top - 14}" font-size="10.5" '
            f'text-anchor="middle">{gv // 1_000_000}M</text>'
        )

    y = top
    for name, value, is_subject in PIPELINED:
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
            h - 36,
            "axum is last because it cannot aggregate pipelined writes — axum::serve exposes no way",
        )
    )
    out.append(
        footnote(w, h - 20, "to ask for it — so a batch of 16 costs it 16 write syscalls.")
    )
    out.append("</svg>")
    return "\n".join(out)


# -----------------------------------------------------------------------------
# 3. Tail latency — the other mode Churust loses
# -----------------------------------------------------------------------------

# Milliseconds at the 99th percentile, keep-alive.
P99 = [
    ("actix-web", 0.171, False),
    ("bare hyper (floor)", 0.175, False),
    ("Churust", 0.211, True),
    ("axum", 0.326, False),
    ("Ktor (Netty)", 1.84, False),
    ("Go net/http", 2.46, False),
]


def chart_p99() -> str:
    w, row, gap = 900, 24, 20
    top = 92
    h = top + len(P99) * (row + gap) + 66
    x0, x1 = 168, 700
    peak = max(v for _, v, _ in P99)

    out = [
        header(
            w,
            h,
            "Tail latency at the 99th percentile, keep-alive",
            "actix-web 0.171ms, bare hyper 0.175ms, Churust 0.211ms, axum 0.326ms, "
            "Ktor 1.84ms, Go net/http 2.46ms. Lower is better; actix-web leads.",
            "p99",
        ),
        titles(
            "Tail latency — actix-web leads at equal tuning",
            "99th-percentile response time, median of 9 rounds · lower is better",
        ),
    ]

    for gv in (0.5, 1.0, 1.5, 2.0, 2.5):
        gx = x0 + (x1 - x0) * gv / peak
        out.append(
            f'<line class="grid" x1="{gx:.1f}" y1="{top - 8}" x2="{gx:.1f}" '
            f'y2="{top + len(P99) * (row + gap) - gap + 4}" stroke-width="1"/>'
        )
        out.append(
            f'<text class="mut" x="{gx:.1f}" y="{top - 14}" font-size="10.5" '
            f'text-anchor="middle">{gv:g}ms</text>'
        )

    y = top
    for name, value, is_subject in P99:
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
            f'font-size="13" font-weight="{weight}">{value:g} ms</text>'
        )
        y += row + gap

    out.append(
        f'<line class="axis" x1="{x0}" y1="{top - 8}" x2="{x0}" '
        f'y2="{y - gap + 4}" stroke-width="1"/>'
    )
    out.append(
        footnote(
            w,
            h - 36,
            "Each server at its best worker count. At the shared one-per-core default the two are",
        )
    )
    out.append(
        footnote(
            w,
            h - 20,
            "level at ~245us — worker count moves this more than anything in either framework.",
        )
    )
    out.append("</svg>")
    return "\n".join(out)


# -----------------------------------------------------------------------------
# 3. CPU per request — where actix-web wins
# -----------------------------------------------------------------------------

CPU = [
    ("bare hyper (floor)", 4.17, False),
    ("actix-web", 4.24, False),
    ("Churust", 4.92, True),
    ("axum", 8.83, False),
    ("Go net/http", 13.12, False),
    ("Ktor (Netty)", 20.47, False),
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
            "bare hyper 4.17, actix-web 4.24, Churust 4.92, axum 8.83, "
            "Go net/http 13.12, Ktor 20.47 microseconds of CPU per request. "
            "Lower is better. The gap from Churust to the bare-hyper floor, "
            "0.75, is what the framework layer itself costs.",
            "cpu",
        ),
        titles(
            "Server CPU per request — actix-web is the most efficient",
            "Microseconds of process CPU per request served, keep-alive · lower is better",
        ),
    ]

    for gv in (5, 10, 15, 20):
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
            "Each server at its best worker count. actix-web does the same work for a third less CPU, "
            "which is the clearest gap in this comparison."
        )
    )
    out.append("</svg>")
    return "\n".join(out)


# -----------------------------------------------------------------------------
# 4. Before and after
# -----------------------------------------------------------------------------


def chart_before_after() -> str:
    w, h = 900, 392
    x0, x1 = 168, 700
    row, gap, top = 28, 22, 96
    # Both sides from the superseded eight-worker run. Kept as a pair because
    # the *comparison* is the claim and both halves were measured together;
    # neither number is current, and the chart's title says so.
    data = [("Before", 390_772, False), ("After", 880_352, True)]
    peak = 880_352

    out = [
        header(
            w,
            h,
            "Churust throughput before and after this work",
            "Churust went from 391 thousand to 699 thousand requests per second on "
            "keep-alive load, a 2.25 times change, with 99th-percentile latency improving "
            "from 444 microseconds to 183 microseconds as well.",
            "ba",
        ),
        titles(
            "Churust, before and after this work — 2.25× on keep-alive",
            "Same kernel, same harness, same pinned cores · only the binary differs",
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
        "Removing per-request atomics from shared cache lines",
        "One runtime per core (App::run_sharded), SO_REUSEPORT on Linux",
        "Dropping per-request allocations, and the connection loop's bookkeeping",
        "TCP_NODELAY on by default",
        "",
        "Tail latency improved too: p99 444us before, 183us after, once the",
        "per-connection handoff and the loop bookkeeping were removed.",
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
            "None of it in routing or extraction. Pipelined workloads gain far more; see the results.",
        )
    )
    out.append("</svg>")
    return "\n".join(out)


def main() -> None:
    OUT.mkdir(parents=True, exist_ok=True)
    charts = {
        "benchmark-throughput.svg": chart_throughput(),
        "benchmark-pipelined.svg": chart_pipelined(),
        "benchmark-p99-latency.svg": chart_p99(),
        "benchmark-cpu-per-request.svg": chart_cpu(),
        "benchmark-before-after.svg": chart_before_after(),
    }
    for name, svg in charts.items():
        (OUT / name).write_text(svg + "\n", encoding="utf-8")
        print(f"wrote {OUT / name}")


if __name__ == "__main__":
    main()
