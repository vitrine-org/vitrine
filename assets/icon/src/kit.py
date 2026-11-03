"""Small SVG kit for the Vitrine icon (gradients, Fluent shadows, inner highlights, shapes).
Originally: Vitrine icon concepts as hand-built Fluent-style vector art.

Master artwork is drawn on the 48-unit grid (viewBox 0 0 48 48) and used for 48 px and up.
Small frames (32, 24, 20, 16) are separate drawings in their own pixel space: geometry is
re-laid on whole pixels and detail is removed per size.
Output: svg/<concept>_master.svg and svg/<concept>_<size>.svg
"""
import math, os




def f(v):
    return ('%.3f' % v).rstrip('0').rstrip('.')


class Svg:
    def __init__(self, size, view=48, idp='vit-'):
        # idp prefixes every id, so the SVG can be inlined next to other SVGs without collisions
        self.size, self.view, self.idp = size, view, idp
        self.defs, self.body, self.n = [], [], 0

    def uid(self, p):
        self.n += 1
        return f'{self.idp}{p}{self.n}'

    # gradients: angle in Fluent terms (light top-left). Coordinates in user space.
    def lin(self, stops, x1, y1, x2, y2):
        i = self.uid('g')
        st = ''.join(f'<stop offset="{f(o)}" stop-color="{c}"' + (f' stop-opacity="{f(a)}"' if a < 1 else '') + '/>'
                     for o, c, a in [(s + (1,))[:3] if len(s) == 2 else s for s in stops])
        self.defs.append(f'<linearGradient id="{i}" gradientUnits="userSpaceOnUse" x1="{f(x1)}" y1="{f(y1)}" '
                         f'x2="{f(x2)}" y2="{f(y2)}">{st}</linearGradient>')
        return f'url(#{i})'

    def lin120(self, stops, x, y, w, h):
        """120deg ramp across a box: from upper-left to lower-right, slightly more horizontal."""
        cx, cy = x + w / 2, y + h / 2
        dx, dy = math.sin(math.radians(120)), -math.cos(math.radians(120))
        L = (abs(dx) * w + abs(dy) * h) / 2
        return self.lin(stops, cx - dx * L, cy - dy * L, cx + dx * L, cy + dy * L)

    def rad(self, stops, cx, cy, r, fx=None, fy=None):
        i = self.uid('r')
        st = ''.join(f'<stop offset="{f(o)}" stop-color="{c}"' + (f' stop-opacity="{f(a)}"' if a < 1 else '') + '/>'
                     for o, c, a in [(s + (1,))[:3] if len(s) == 2 else s for s in stops])
        fx = cx if fx is None else fx; fy = cy if fy is None else fy
        self.defs.append(f'<radialGradient id="{i}" gradientUnits="userSpaceOnUse" cx="{f(cx)}" cy="{f(cy)}" r="{f(r)}" '
                         f'fx="{f(fx)}" fy="{f(fy)}">{st}</radialGradient>')
        return f'url(#{i})'

    def shadow(self, k=1.0, key=(1.0, 1.0, 0.22), contact=(0.25, 0.25, 0.18)):
        """Fluent-style two-layer drop shadow, values in 48-grid units (scaled by k)."""
        i = self.uid('s')
        (d1, b1, a1), (d2, b2, a2) = key, contact
        self.defs.append(
            f'<filter id="{i}" x="-30%" y="-30%" width="160%" height="170%" color-interpolation-filters="sRGB">'
            f'<feGaussianBlur in="SourceAlpha" stdDeviation="{f(b1 * k)}" result="b1"/>'
            f'<feOffset in="b1" dy="{f(d1 * k)}" result="o1"/>'
            f'<feComponentTransfer in="o1" result="s1"><feFuncA type="linear" slope="{a1}"/></feComponentTransfer>'
            f'<feGaussianBlur in="SourceAlpha" stdDeviation="{f(b2 * k)}" result="b2"/>'
            f'<feOffset in="b2" dy="{f(d2 * k)}" result="o2"/>'
            f'<feComponentTransfer in="o2" result="s2"><feFuncA type="linear" slope="{a2}"/></feComponentTransfer>'
            f'<feMerge><feMergeNode in="s1"/><feMergeNode in="s2"/><feMergeNode in="SourceGraphic"/></feMerge></filter>')
        return f'url(#{i})'

    def blur(self, sd):
        i = self.uid('b')
        self.defs.append(f'<filter id="{i}" x="-50%" y="-50%" width="200%" height="200%"><feGaussianBlur stdDeviation="{f(sd)}"/></filter>')
        return f'url(#{i})'

    def clip(self, inner):
        i = self.uid('c')
        self.defs.append(f'<clipPath id="{i}">{inner}</clipPath>')
        return f'url(#{i})'

    def add(self, s):
        self.body.append(s)

    def out(self, path, size=None):
        size = size or self.size
        s = (f'<svg xmlns="http://www.w3.org/2000/svg" width="{size}" height="{size}" '
             f'viewBox="0 0 {self.view} {self.view}"><defs>{"".join(self.defs)}</defs>{"".join(self.body)}</svg>\n')
        with open(path, 'w', encoding='utf-8', newline='\n') as fh:
            fh.write(s)
        return s


def rr(x, y, w, h, r):
    r = min(r, w / 2, h / 2)
    return (f'M{f(x + r)} {f(y)}H{f(x + w - r)}A{f(r)} {f(r)} 0 0 1 {f(x + w)} {f(y + r)}V{f(y + h - r)}'
            f'A{f(r)} {f(r)} 0 0 1 {f(x + w - r)} {f(y + h)}H{f(x + r)}A{f(r)} {f(r)} 0 0 1 {f(x)} {f(y + h - r)}'
            f'V{f(y + r)}A{f(r)} {f(r)} 0 0 1 {f(x + r)} {f(y)}Z')


def P(d, fill, extra=''):
    return f'<path d="{d}" fill="{fill}" {extra}/>'


def circ(cx, cy, r, fill, extra=''):
    return f'<circle cx="{f(cx)}" cy="{f(cy)}" r="{f(r)}" fill="{fill}" {extra}/>'


def arcd(cx, cy, r, a0, a1):
    """Arc path, screen angles in degrees (0 = right, clockwise)."""
    x0, y0 = cx + r * math.cos(math.radians(a0)), cy + r * math.sin(math.radians(a0))
    x1, y1 = cx + r * math.cos(math.radians(a1)), cy + r * math.sin(math.radians(a1))
    large = 1 if (a1 - a0) % 360 > 180 else 0
    return f'M{f(x0)} {f(y0)}A{f(r)} {f(r)} 0 {large} 1 {f(x1)} {f(y1)}'


def stroke(d, color, w, extra='', cap='round'):
    return f'<path d="{d}" fill="none" stroke="{color}" stroke-width="{f(w)}" stroke-linecap="{cap}" stroke-linejoin="round" {extra}/>'


def inner_highlight(sv, d, w, top='#FFFFFF', a=0.6, y0=0, y1=48, bottom=None, ab=0.0):
    """Light inner edge: shape outline stroked 2w wide, clipped to the shape, fading top -> bottom."""
    stops = [(0, top, a), (0.45, top, 0)]
    if bottom:
        stops = [(0, top, a), (0.5, top, 0), (0.5, bottom, 0), (1, bottom, ab)]
    g = sv.lin(stops, 0, y0, 0, y1)
    cp = sv.clip(f'<path d="{d}"/>')
    return f'<g clip-path="{cp}">{stroke(d, g, 2 * w)}</g>'


