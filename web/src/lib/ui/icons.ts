// Static icon geometry (16×16 viewBox), rendered as SVG elements by Icon.svelte (no HTML strings).
// Each icon is a list of paths; `fill` paths are filled, the rest stroked.
export interface IconPath {
  d: string;
  fill?: boolean;
  width?: number;
}

const stroke = (d: string, width = 1.5): IconPath => ({ d, width });
const fill = (d: string): IconPath => ({ d, fill: true });
const circle = (cx: number, cy: number, r: number) => `M${cx - r} ${cy}a${r} ${r} 0 1 0 ${2 * r} 0a${r} ${r} 0 1 0 ${-2 * r} 0z`;

export const ICONS = {
  flag: [stroke('M4 1.8v12.6', 1.8), fill('M4.6 2.4h8.2l-2.1 3.1 2.1 3.1H4.6z')],
  flagOutline: [stroke('M4 1.8v12.6', 1.8), stroke('M4.9 2.6h7.4l-1.8 2.9 1.8 2.9H4.9z')],
  check: [stroke('M3.2 8.6l3.1 3.1 6.6-7', 2.2)],
  cross: [stroke('M4.2 4.2l7.6 7.6M11.8 4.2l-7.6 7.6', 2.2)],
  clock: [stroke(circle(8, 8, 5.9), 1.6), stroke('M8 4.9V8l2.2 1.5', 1.6)],
  chain: [stroke('M6.6 9.4 5 11a2.2 2.2 0 0 1-3.1-3.1l1.6-1.6M9.4 6.6 11 5a2.2 2.2 0 0 1 3.1 3.1l-1.6 1.6', 1.7), stroke('M5.6 1.8v1.8M1.8 5.6h1.8M10.4 14.2v-1.8M14.2 10.4h-1.8')],
  archive: [stroke('M2.8 2.6h10.4a1 1 0 0 1 1 1v1.4a1 1 0 0 1-1 1H2.8a1 1 0 0 1-1-1V3.6a1 1 0 0 1 1-1z'), stroke('M3 6v6.4a1 1 0 0 0 1 1h8a1 1 0 0 0 1-1V6M6.4 8.6h3.2')],
  unarchive: [stroke('M3.5 6.5A5 5 0 1 1 3 9.5', 1.6), stroke('M3.2 2.8v3.9h3.9', 1.6)],
  pin: [stroke('M6 2h4l-.6 4 2.6 2.4H4L6.6 6z', 1.4), stroke('M8 8.4V14', 1.4)],
  pinned: [fill('M6 2h4l-.6 4 2.6 2.4H4L6.6 6z'), stroke('M6 2h4l-.6 4 2.6 2.4H4L6.6 6zM8 8.4V14', 1.4)],
  bell: [stroke('M4 11V7.2a4 4 0 0 1 8 0V11l1.2 1.3H2.8z', 1.4), stroke('M6.6 13.6a1.5 1.5 0 0 0 2.8 0', 1.4)],
  list: [stroke('M2.5 3.5h11M2.5 8h11M2.5 12.5h7', 1.6)],
  board: [stroke('M2.8 2.3h1.4a1 1 0 0 1 1 1v9.4a1 1 0 0 1-1 1H2.8a1 1 0 0 1-1-1V3.3a1 1 0 0 1 1-1zM7.3 2.3h1.4a1 1 0 0 1 1 1v5.6a1 1 0 0 1-1 1H7.3a1 1 0 0 1-1-1V3.3a1 1 0 0 1 1-1zM11.8 2.3h1.4a1 1 0 0 1 1 1v7.6a1 1 0 0 1-1 1h-1.4a1 1 0 0 1-1-1V3.3a1 1 0 0 1 1-1z', 1.4)],
  pencil: [stroke('M10.8 2.6l2.6 2.6-7.7 7.7-3.2.6.6-3.2z')],
  sliders: [stroke('M2 4.5h7M12 4.5h2M2 11.5h2M7 11.5h7'), stroke(circle(10.5, 4.5, 1.6)), stroke(circle(5.5, 11.5, 1.6))],
  plus: [stroke('M8 3v10M3 8h10', 1.6)],
  gear: [stroke(circle(8, 8, 2.1), 1.4), stroke('M13 9a5 5 0 0 0 0-2l1.3-1-1.3-2.3-1.6.6a5.1 5.1 0 0 0-1.7-1L9.3 1.6H6.7l-.3 1.7a5.1 5.1 0 0 0-1.7 1l-1.6-.6L1.7 6 3 7a5 5 0 0 0 0 2l-1.3 1L3 12.3l1.6-.6a5.1 5.1 0 0 0 1.7 1l.3 1.7h2.6l.3-1.7a5.1 5.1 0 0 0 1.7-1l1.6.6 1.3-2.3z', 1.2)],
  filter: [stroke('M2 3h12l-4.5 5.5V13l-3 1.5V8.5z', 1.6)],
  read: [stroke(circle(8, 8, 6.2)), stroke('M5.3 8.2l1.9 1.9 3.6-3.9', 1.6)],
  unread: [stroke(circle(8, 8, 6.2)), fill(circle(8, 8, 2.4))],
  expand: [stroke('M9.5 2.5h4v4M6.5 13.5h-4v-4M13.5 2.5 9 7M2.5 13.5 7 9')],
  collapse: [stroke('M13.5 6.5h-4v-4M2.5 9.5h4v4M9.5 6.5 14 2M6.5 9.5 2 14')],
  shield: [stroke('M8 1.6 13.2 3.6v4.1c0 3.2-2.2 5.6-5.2 6.7-3-1.1-5.2-3.5-5.2-6.7V3.6z')],
  shieldOff: [stroke('M8 1.6 13.2 3.6v4.1c0 3.2-2.2 5.6-5.2 6.7-3-1.1-5.2-3.5-5.2-6.7V3.6z'), stroke('M2.5 2.5l11 11', 1.6)],
  close: [stroke('M4 4l8 8M12 4l-8 8', 1.6)],
} satisfies Record<string, IconPath[]>;

export type IconName = keyof typeof ICONS;
