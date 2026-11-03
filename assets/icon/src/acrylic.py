"""Vitrine app icon, concept "Acrylic": a frosted widget pane floating over a lagoon desktop pane,
with an ember orb glowing between them. Windows 11 Fluent app-icon style, hand-built vector.

The master is drawn on the 48-unit Fluent grid (viewBox 0 0 48 48) and is used for 48 px and up.
The 32, 24, 20 and 16 px frames are separate drawings: main edges are snapped to that frame's
pixel grid (Fit.q), strokes get a 1 px floor (Fit.w), blur and secondary detail are removed.

Usage: python acrylic.py   -> ../vitrine.svg (master) and ../svg/vitrine-<size>.svg (small frames)
"""
import math, os, sys
from kit import Svg, rr, P, circ, arcd, stroke, inner_highlight, f

# ---------------------------------------------------------------- palette: Lagoon & Ember
# Lagoon (cool, structure) and Ember (warm, the widget); both ramps run light top-left -> deep bottom-right.
LAG = dict(l0='#B8FFF4', l1='#6BF2E2', l2='#1EE0D2', l3='#00B8F5', l4='#1A7CFF', l5='#2B4DE0', l6='#1C2A8F')
EMB = dict(e0='#FFF1A8', e1='#FFE05A', e2='#FFB23E', e3='#FF7A45', e4='#FF4D73', e5='#D62E7E')
INK = '#0E1B4D'
FROST = '#F4FBFF'


class Fit:
    """Coordinate helper: 48-unit geometry, optionally snapped to the pixel grid of size s."""
    def __init__(self, s):
        self.s, self.full = s, s >= 48
        self.px = 48 / s                    # one device pixel in grid units

    def q(self, v):                         # snap an edge
        return v if self.full else round(v / self.px) * self.px

    def w(self, v, floor=1.0):              # stroke width with a pixel floor
        return v if self.full else max(v, floor * self.px)


def frame(s):
    return Svg(s, 48)


def wrap(sv, parts, shadow):
    body = ''.join(parts)
    sv.add(f'<g filter="{shadow}">{body}</g>' if shadow else body)


def outer_shadow(sv, F):
    if F.full:
        return sv.shadow(1)
    if F.s == 32:
        return sv.shadow(1, key=(1.2, 1.0, 0.18), contact=(0.4, 0.4, 0.14))
    return None


def acrylic(s):
    F, sv = Fit(s), frame(s)
    q, full = F.q, F.full
    g = []
    # back pane: the desktop, a saturated lagoon gradient
    bx, by, bw, bh, br = q(4), q(5), q(30), q(30), (6 if full else max(6, 2.5 * F.px))
    back = rr(bx, by, bw, bh, br)
    back_fill = sv.lin120([(0, LAG['l1']), (0.45, LAG['l3']), (1, LAG['l5'])], bx, by, bw, bh)
    g.append(P(back, back_fill))
    if full:
        g.append(inner_highlight(sv, back, 0.6, top='#FFFFFF', a=0.55, y0=by, y1=by + bh, bottom=LAG['l6'], ab=0.3))
    # ember orb: the widget, glowing between the two panes
    ox, oy, orr = (31, 15.5, 8.5) if full else (q(31), q(15.5), q(8.5))
    orb_fill = sv.rad([(0, EMB['e0']), (0.35, EMB['e2']), (0.75, EMB['e3']), (1, EMB['e4'])], ox, oy, orr * 1.15, ox - orr * 0.45, oy - orr * 0.5)
    orb = circ(ox, oy, orr, orb_fill)
    # front pane geometry
    fx, fy, fw, fh, fr = q(15), q(15), q(29), q(29), (6 if full else max(6, 2.5 * F.px))
    front = rr(fx, fy, fw, fh, fr)
    # shadow of the front pane, masked into the shapes below (Fluent 'separate metaphor' shadow)
    if full or s == 32:
        cpb = sv.clip(f"<path d='{back}'/>{orb}")
        g.append(f'<g clip-path="{cpb}">' + P(rr(fx + 0.6, fy + 1.4, fw, fh, fr), INK, f'opacity="0.35" filter="{sv.blur(1.4 if full else 1)}"') + '</g>')
    g.append(orb)
    if full:
        g.append(circ(ox - orr * 0.42, oy - orr * 0.48, orr * 0.22, '#FFFFFF', 'opacity="0.55"'))
    # front pane: frosted acrylic that shows a blurred, lightened view of what is behind it
    fg = [P(front, sv.lin120([(0, '#FFFFFF'), (0.6, FROST), (1, '#DDEBFF')], fx, fy, fw, fh))]
    behind = P(back, back_fill) + orb
    blur = sv.blur(2.2) if full else (sv.blur(1.0) if s >= 24 else None)
    fg.append(f'<g opacity="0.62"' + (f' filter="{blur}"' if blur else '') + f'>{behind}</g>')
    fg.append(P(front, sv.lin120([(0, '#FFFFFF', 0.55), (0.55, '#FFFFFF', 0.30), (1, '#FFFFFF', 0.18)], fx, fy, fw, fh)))
    # content on the glass: two text lines and a small ember dot = a widget card
    if s >= 24:
        lx, l1y = q(20), q(32.5)
        lh = 2.4 if full else max(2.4, F.px)
        fg.append(P(rr(lx, l1y, q(14), lh, lh / 2), LAG['l5'], 'opacity="0.9"'))
        if s >= 32:
            fg.append(P(rr(lx, l1y + lh + (2.0 if full else F.px * 1.5), q(9), lh, lh / 2), LAG['l3'], 'opacity="0.75"'))
    cpf = sv.clip(f"<path d='{front}'/>")
    pane = f'<g clip-path="{cpf}">{"".join(fg)}</g>'
    if full:
        pane += inner_highlight(sv, front, 0.6, top='#FFFFFF', a=1.0, y0=fy, y1=fy + fh, bottom='#9DB6E8', ab=0.7)
    else:
        pane += f'<path d="{rr(fx + F.px / 2, fy + F.px / 2, fw - F.px, fh - F.px, fr - F.px / 2)}" fill="none" stroke="#8FA9DE" stroke-opacity="0.75" stroke-width="{f(F.px)}"/>'
    pane_sh = sv.shadow(1, key=(1.0, 1.2, 0.26), contact=(0.25, 0.3, 0.2)) if full else None
    g.append(f'<g filter="{pane_sh}">{pane}</g>' if pane_sh else pane)
    wrap(sv, g, outer_shadow(sv, F))
    return sv


SMALL = (32, 24, 20, 16)
HERE = os.path.dirname(os.path.abspath(__file__))
ICON = os.path.dirname(HERE)                     # assets/icon

if __name__ == '__main__':
    os.makedirs(os.path.join(ICON, 'svg'), exist_ok=True)
    acrylic(48).out(os.path.join(ICON, 'vitrine.svg'), size=256)          # master + site logo/favicon
    for s in SMALL:
        acrylic(s).out(os.path.join(ICON, 'svg', f'vitrine-{s}.svg'))    # pixel-fitted small frames
    print('svg written to', ICON)
