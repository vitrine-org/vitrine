"""Vitrine app icon, concept "Lastre": an abstract V made of two panes. A lit lagoon pane (the left arm)
lies behind; a deep lagoon pane (the right arm) crosses over it down to the tip; an ember orb, the
widget, is tucked between them and peeks out over the back pane. Windows 11 Fluent app-icon style.

Construction on the 48-unit grid (viewBox 0 0 48 48), master used for 48 px and up:
  - The V spans the 40 x 40 square keyline horizontally (x 4..44, margin 4).
  - Every outer edge is a 1:2 grid diagonal (dx:dy = 1:2), so edges pass through grid intersections.
  - Arms are 14 u wide at the top (12.5 u thick across the slope).
  - Key points are whole units: tops (4,6) (18,6) (30,6) (44,6), notch (24,18), crossing (17,32),
    geometric tip (24,46). The tip sits outside the keyline on paper; the 2 u radius pulls the visible
    tip up to 43.5, inside it (optical compensation for a pointed bottom).
  - Radii follow Microsoft: 2 u on exterior corners; none where the two panes cross (an occlusion edge,
    not a curve).
  - Gradients run at 120 deg, light from the top left. Two-layer drop shadow from kit.shadow(); the
    front pane's shadow is masked into the shapes beneath it.

The 32, 24, 20 and 16 px frames are separate drawings (ANCHORS): top edge and outer x extents land on
whole device pixels and the 1:2 slope is kept. Detail drops by size: the orb and all shadows below 32 px,
the inner highlights below 48 px; below 32 px the lit ramp starts one step deeper so the arm holds on the
light taskbar.

Usage: python lastre.py  -> ../vitrine.svg (master), ../svg/vitrine-<size>.svg (small frames),
                            construction.svg (grid, keyline and construction lines, for reference)
"""
import os
from kit import (Svg, P, circ, inner_highlight, f, rpoly, line_x, intersect, vadd, vsub, vmul, vnorm)

# ---------------------------------------------------------------- palette: Lagoon & Ember
LAG = dict(l0='#B8FFF4', l1='#6BF2E2', l2='#1EE0D2', l3='#00B8F5', l4='#1A7CFF', l5='#2B4DE0', l6='#1C2A8F')
EMB = dict(e0='#FFF1A8', e1='#FFE05A', e2='#FFB23E', e3='#FF7A45', e4='#FF4D73', e5='#D62E7E')
INK = '#0E1B4D'

# ---------------------------------------------------------------- geometry
# Anchors per frame, in grid units (one device pixel = 48/size units).
ANCHORS = {
    48: dict(x0=4.0, x1=44.0, T=6.0, a=14.0),
    32: dict(x0=4.5, x1=43.5, T=6.0, a=13.5),   # x 3..29 px, top 4 px, arm 9 px
    24: dict(x0=4.0, x1=44.0, T=6.0, a=14.0),   # x 2..22 px, top 3 px, arm 7 px
    20: dict(x0=4.8, x1=43.2, T=7.2, a=14.4),   # x 2..18 px, top 3 px, arm 6 px
    16: dict(x0=3.0, x1=45.0, T=3.0, a=15.0),   # x 1..15 px, top 1 px, arm 5 px
}
K = 0.5          # dx/dy of every outer edge: the 1:2 grid diagonal
RX = 2.0         # exterior corner radius (Microsoft: 2 px at 48 x 48)
ORB_R = 5.0      # ember orb radius, master
ORB_DY = 7.0     # orb centre: on the front pane's inner edge, this far below the notch


def geom(s):
    A = ANCHORS[48 if s >= 48 else s]
    x0, x1, T, a = A['x0'], A['x1'], A['T'], A['a']
    cx = (x0 + x1) / 2
    g = dict(x0=x0, x1=x1, T=T, a=a, cx=cx)
    g['tip'] = (cx, T + (cx - x0) / K)             # geometric (sharp) tip
    g['notch'] = (cx, T + (cx - x0 - a) / K)       # inner vertex
    g['TL'], g['TLi'], g['TRi'], g['TR'] = (x0, T), (x0 + a, T), (x1 - a, T), (x1, T)
    g['cut'] = intersect(g['TRi'], g['notch'], g['TL'], g['tip'])   # front inner edge meets left outer edge
    return g


def paths(g):
    TL, TLi, TRi, TR, tip, notch, cut, cx = (g[k] for k in ('TL', 'TLi', 'TRi', 'TR', 'tip', 'notch', 'cut', 'cx'))
    # front pane: slanted bar from the top right to the tip, crossing over the left arm
    front = rpoly([TRi, TR, tip, cut], RX)
    # back pane: the left arm. It runs 1.6 u past the front pane's edge, hidden under it, so the
    # shared edge has no anti-aliasing seam.
    d = vnorm(vsub(notch, TRi))
    n = (-d[1], d[0]) if -d[1] > 0 else (d[1], -d[0])
    pa, pb = vadd(TRi, vmul(n, 1.6)), vadd(notch, vmul(n, 1.6))
    on_axis = (cx, pa[1] + (cx - pa[0]) * (pb[1] - pa[1]) / (pb[0] - pa[0]))
    back = rpoly([TL, TLi, notch, on_axis, intersect(pa, pb, TL, tip)], [RX, RX, 0, 0, 0])
    # outline of the whole V (construction drawing)
    sil = rpoly([TL, TLi, notch, TRi, TR, tip], [RX, RX, 0, RX, RX, RX])
    return front, back, sil


def orb_at(g):
    y = g['notch'][1] + ORB_DY
    return line_x(g['TRi'], g['notch'], y), y


# ---------------------------------------------------------------- drawing
def lastre(s):
    full = s >= 48
    sv = Svg(s, 48)
    g = geom(s)
    front, back, _ = paths(g)
    T, tipy, x0, x1, cutx = g['T'], g['tip'][1], g['x0'], g['x1'], g['cut'][0]
    parts = []
    # back pane: lit lagoon
    lit = sv.lin120([(0, LAG['l1'] if s >= 32 else LAG['l2']), (0.5, LAG['l3']), (1, LAG['l4'])],
                    x0, T, g['cx'] - x0, tipy - T)
    parts.append(P(back, lit))
    if full:
        parts.append(inner_highlight(sv, back, 0.6, top='#FFFFFF', a=0.6, y0=T, y1=tipy, bottom=LAG['l6'], ab=0.25))
    # ember orb between the panes
    orb = ''
    if s >= 32:
        ox, oy = orb_at(g)
        r = ORB_R if full else max(ORB_R, 1.5 * 48 / s)
        fill = sv.rad([(0, EMB['e0']), (0.35, EMB['e2']), (0.75, EMB['e3']), (1, EMB['e4'])],
                      ox, oy, r * 1.15, ox - r * 0.45, oy - r * 0.5)
        orb = circ(ox, oy, r, fill)
        parts.append(orb)
        # the front pane's shadow, masked into what lies beneath it
        cp = sv.clip(f"<path d='{back}'/>{orb}")
        parts.append(f'<g clip-path="{cp}">' + P(front, INK, f'transform="translate(0 {f(1.0 if full else 0.75)})" '
                     f'opacity="0.32" filter="{sv.blur(1.1 if full else 0.8)}"') + '</g>')
    # front pane: deep lagoon with a soft top-left sheen
    deep = sv.lin120([(0, LAG['l4']), (0.55, LAG['l5']), (1, LAG['l6'])], cutx, T, x1 - cutx, tipy - T)
    sheen = sv.lin120([(0, '#FFFFFF', 0.22), (0.5, '#FFFFFF', 0.04), (1, '#FFFFFF', 0)], cutx, T, x1 - cutx, tipy - T)
    cp = sv.clip(f"<path d='{front}'/>")
    parts.append(f'<g clip-path="{cp}">{P(front, deep)}{P(front, sheen)}</g>')
    if full:
        parts.append(inner_highlight(sv, front, 0.6, top='#FFFFFF', a=0.6, y0=T, y1=tipy, bottom=LAG['l6'], ab=0.45))
    body = ''.join(parts)
    sh = sv.shadow(1) if full else (sv.shadow(1, key=(1.2, 1.0, 0.18), contact=(0.4, 0.4, 0.14)) if s == 32 else None)
    sv.add(f'<g filter="{sh}">{body}</g>' if sh else body)
    return sv


def construction():
    """Reference drawing: 48 grid, square keyline, 1:2 construction lines and key points."""
    g = geom(48)
    _, _, sil = paths(g)
    L = []
    for i in range(49):
        major = i % 4 == 0
        L.append(f'<path d="M{i} 0V48M0 {i}H48" stroke="#5B8CFF" stroke-width="{0.1 if major else 0.06}" '
                 f'stroke-opacity="{0.6 if major else 0.35}" fill="none"/>')
    L.append('<rect x="4" y="4" width="40" height="40" fill="none" stroke="#FF4D73" stroke-width="0.18"/>')
    L.append('<path d="M24 0V48" stroke="#FF4D73" stroke-width="0.12" stroke-dasharray="0.6 0.6"/>')
    ext = lambda p, q: vadd(q, vmul(vsub(q, p), 0.12))
    for p, q in ((g['TL'], g['tip']), (g['TR'], g['tip']), (g['TLi'], g['notch']), (g['TRi'], g['notch'])):
        e = ext(p, q)
        L.append(f'<path d="M{f(p[0])} {f(p[1])}L{f(e[0])} {f(e[1])}" stroke="#00B8F5" stroke-width="0.14" stroke-dasharray="0.8 0.5"/>')
    c = g['cut']
    L.append(f'<path d="M{f(g["TRi"][0])} {f(g["TRi"][1])}L{f(c[0])} {f(c[1])}" stroke="#00B8F5" stroke-width="0.14" stroke-dasharray="0.8 0.5"/>')
    L.append(f'<path d="{sil}" fill="none" stroke="{INK}" stroke-width="0.14"/>')
    for k in ('TL', 'TLi', 'TRi', 'TR', 'notch', 'cut', 'tip'):
        p = g[k]
        L.append(f'<circle cx="{f(p[0])}" cy="{f(p[1])}" r="0.45" fill="#FFFFFF" stroke="#FF4D73" stroke-width="0.16"/>')
    return f'<svg xmlns="http://www.w3.org/2000/svg" width="512" height="512" viewBox="0 0 48 48">{"".join(L)}</svg>\n'


SMALL = (32, 24, 20, 16)
HERE = os.path.dirname(os.path.abspath(__file__))
ICON = os.path.dirname(HERE)                     # assets/icon

if __name__ == '__main__':
    os.makedirs(os.path.join(ICON, 'svg'), exist_ok=True)
    lastre(48).out(os.path.join(ICON, 'vitrine.svg'), size=256)          # master + site logo/favicon
    for s in SMALL:
        lastre(s).out(os.path.join(ICON, 'svg', f'vitrine-{s}.svg'))     # pixel-fitted small frames
    with open(os.path.join(HERE, 'construction.svg'), 'w', encoding='utf-8', newline='\n') as fh:
        fh.write(construction())
    print('svg written to', ICON)
