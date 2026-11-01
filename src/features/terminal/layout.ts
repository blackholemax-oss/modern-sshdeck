import type { PaneLayout } from "@/stores/sessions";

/** 每种分屏布局实际渲染的 pane 数量。 */
export const PANE_COUNT: Record<PaneLayout, number> = {
  single: 1,
  "split-h": 2,
  "split-v": 2,
  quad: 4,
};

export const LAYOUTS: PaneLayout[] = ["single", "split-h", "split-v", "quad"];

export const LAYOUT_LABEL: Record<PaneLayout, string> = {
  single: "单屏",
  "split-h": "左右",
  "split-v": "上下",
  quad: "四宫格",
};

export const LAYOUT_GRID: Record<PaneLayout, string> = {
  single: "grid-cols-1 grid-rows-1",
  "split-h": "grid-cols-2 grid-rows-1",
  "split-v": "grid-cols-1 grid-rows-2",
  quad: "grid-cols-2 grid-rows-2",
};
