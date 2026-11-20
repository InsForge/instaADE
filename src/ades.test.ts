import { describe, expect, it } from "vitest";
import { SKINS, skinStyle } from "./ades";

/** WCAG relative luminance of a #RRGGBB color. */
function luminance(hex: string): number {
  const [r, g, b] = [1, 3, 5].map((i) => {
    const c = parseInt(hex.slice(i, i + 2), 16) / 255;
    return c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

function contrast(a: string, b: string): number {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return (hi + 0.05) / (lo + 0.05);
}

const WHITE = "#FFFFFF";
/** The menus keep their own background in every mode. */
const MENU = "#1C1C1C";

describe("ADE skins", () => {
  for (const [id, s] of Object.entries(SKINS)) {
    it(`${id}: every text color keeps a contrast of 4.5:1 or more`, () => {
      const pairs: [string, string, string][] = [
        ["text on page", WHITE, s.page],
        ["text on header", WHITE, s.header],
        ["text on raised", WHITE, s.raised],
        ["muted on page", s.muted, s.page],
        ["muted on header", s.muted, s.header],
        ["muted on raised", s.muted, s.raised],
        ["muted on menu", s.muted, MENU],
        ["ink on accent", s.ink, s.accent],
        ["ink on accent hover", s.ink, s.accentHover],
        ["accent text on page", s.accentText, s.page],
        ["accent text on raised", s.accentText, s.raised],
        ["accent on page", s.accent, s.page],
      ];
      for (const [what, fg, bg] of pairs) {
        expect(contrast(fg, bg), `${what}: ${fg} on ${bg}`).toBeGreaterThanOrEqual(4.5);
      }
    });
  }

  it("plain Instabox sets no colors, so it keeps the neutral look", () => {
    expect(skinStyle("instabox")).toBeUndefined();
    expect(skinStyle("t3")).toMatchObject({ "--accent": "#A78BFA", "--accent-ink": "#140F22" });
    expect(skinStyle("cmux")).toMatchObject({ "--accent": "#38BDF8", "--accent-ink": "#04141F" });
  });
});
