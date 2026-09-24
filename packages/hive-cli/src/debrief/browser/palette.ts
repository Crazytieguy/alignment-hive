/**
 * Colour-vision measurements for the diff palette: Vienot 1999 dichromacy in
 * linear sRGB (matrices from libDaltonLens), WCAG contrast and CIE76 deltaE.
 * No browser needed.
 */
type Rgb = [number, number, number];
type Matrix = [Rgb, Rgb, Rgb];

export const VISIONS: Record<'normal' | 'protan' | 'deutan', Matrix> = {
  normal: [[1, 0, 0], [0, 1, 0], [0, 0, 1]],
  protan: [[0.11238, 0.88762, 0], [0.11238, 0.88762, 0], [0.00401, -0.00401, 1]],
  deutan: [[0.29275, 0.70725, 0], [0.29275, 0.70725, 0], [-0.02234, 0.02234, 1]],
};

export function linear(hex: string): Rgb {
  return [1, 3, 5].map((i) => {
    const v = parseInt(hex.slice(i, i + 2), 16) / 255;
    return v <= 0.04045 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4;
  }) as Rgb;
}

export function simulate(hex: string, matrix: Matrix): Rgb {
  const v = linear(hex);
  return matrix.map((row) => Math.max(0, Math.min(1, row[0] * v[0] + row[1] * v[1] + row[2] * v[2]))) as Rgb;
}

export function contrast(a: Rgb, b: Rgb): number {
  const lum = (c: Rgb) => 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
  const [x, y] = [lum(a), lum(b)].sort((p, q) => p - q);
  return (y + 0.05) / (x + 0.05);
}

export function lab(rgb: Rgb): Rgb {
  const rows: Matrix = [[0.4124564, 0.3575761, 0.1804375], [0.2126729, 0.7151522, 0.072175], [0.0193339, 0.119192, 0.9503041]];
  const white = [0.95047, 1, 1.08883];
  const [x, y, z] = rows.map((row, i) => {
    const v = (row[0] * rgb[0] + row[1] * rgb[1] + row[2] * rgb[2]) / white[i];
    return v > (6 / 29) ** 3 ? Math.cbrt(v) : v / (3 * (6 / 29) ** 2) + 4 / 29;
  });
  return [116 * y - 16, 500 * (x - y), 200 * (y - z)];
}

/** The `--dv-*: #rrggbb` tokens of one theme block of review-diff.css. */
export function themeTokens(css: string, theme: 'light' | 'dark'): Record<string, string> {
  const marker = theme === 'light' ? ':root {' : ':root[data-theme="dark"] {';
  const body = css.split(marker)[1]?.split('}')[0] ?? '';
  return Object.fromEntries(Array.from(body.matchAll(/--dv-([\w-]+):\s*(#[0-9a-fA-F]{6})\b/g), (m) => [m[1], m[2].toLowerCase()]));
}

export interface PaletteReport {
  colors: Record<string, string>;
  /** Added vs removed row backgrounds as each vision sees them. */
  visions: Record<string, { contrast: number; deltaE: number; deltaL: number }>;
  /** Syntax token contrast on the added and removed row backgrounds, as normal vision and protanopia see it. */
  syntaxContrast: Record<string, { add: number; del: number; protanAdd: number; protanDel: number }>;
}

const round = (n: number) => Math.round(n * 1000) / 1000;

export function paletteReport(css: string, theme: 'light' | 'dark'): PaletteReport {
  const colors = themeTokens(css, theme);
  const visions: PaletteReport['visions'] = {};
  for (const [vision, matrix] of Object.entries(VISIONS)) {
    const added = simulate(colors['add-bg'], matrix);
    const removed = simulate(colors['del-bg'], matrix);
    const a = lab(added), b = lab(removed);
    visions[vision] = { contrast: round(contrast(added, removed)), deltaE: round(Math.hypot(a[0] - b[0], a[1] - b[1], a[2] - b[2])), deltaL: round(a[0] - b[0]) };
  }
  const syntaxContrast: PaletteReport['syntaxContrast'] = {};
  for (const token of ['comment', 'meta', 'keyword', 'string', 'number', 'title']) {
    const ink = linear(colors[`hl-${token}`]), protan = (hex: string) => simulate(hex, VISIONS.protan);
    syntaxContrast[token] = {
      add: round(contrast(ink, linear(colors['add-bg']))), del: round(contrast(ink, linear(colors['del-bg']))),
      protanAdd: round(contrast(protan(colors[`hl-${token}`]), protan(colors['add-bg']))), protanDel: round(contrast(protan(colors[`hl-${token}`]), protan(colors['del-bg']))),
    };
  }
  return { colors, visions, syntaxContrast };
}
