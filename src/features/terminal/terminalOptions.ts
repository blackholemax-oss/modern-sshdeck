/**
 * xterm 的静态配置与编解码辅助。
 *
 * 这里只放纯函数/常量，不放任何 React 状态：终端输出属于高频数据，不进 React state。
 */
import type { ITheme } from "@xterm/xterm";

/**
 * 显式字体栈：Windows 上 Conhost / Windows Terminal 的字体渲染差异较大，必须定死。
 *
 * 末尾补几个常见的 Nerd Font 变体：远端提示符（Powerlevel10k、starship 等）
 * 会输出私用区图标，若字体缺失会显示为方框。装任一 Nerd Font 即可正常。
 * 顺序：等宽编程字体 → Nerd Font 变体 → 系统等宽兜底。
 */
export const TERMINAL_FONT_FAMILY = [
  '"Cascadia Code"',
  '"Cascadia Mono"',
  '"JetBrainsMono Nerd Font"',
  '"JetBrains Mono"',
  '"FiraCode Nerd Font"',
  '"Fira Code"',
  '"Hack Nerd Font"',
  '"MesloLGS NF"',
  '"Consolas"',
  '"Noto Sans Mono"',
  "monospace",
].join(", ");

export const TERMINAL_FONT_SIZE = 14;

export const TERMINAL_SCROLLBACK = 5000;

/** dark 主题：背景 #1e1e1e，前景 #d4d4d4（与 ARCHITECTURE §6.3 一致）。 */
export const TERMINAL_THEME_DARK: ITheme = {
  background: "#1e1e1e",
  foreground: "#d4d4d4",
  cursor: "#d4d4d4",
  cursorAccent: "#1e1e1e",
  selectionBackground: "#264f78",
  black: "#000000",
  red: "#cd3131",
  green: "#0dbc79",
  yellow: "#e5e510",
  blue: "#2472c8",
  magenta: "#bc3fbc",
  cyan: "#11a8cd",
  white: "#e5e5e5",
  brightBlack: "#666666",
  brightRed: "#f14c4c",
  brightGreen: "#23d18b",
  brightYellow: "#f5f543",
  brightBlue: "#3b8eea",
  brightMagenta: "#d670d6",
  brightCyan: "#29b8db",
  brightWhite: "#e5e5e5",
};

/** light 主题：VS Code Light+ 系配色，保证浅色界面下终端不刺眼。 */
export const TERMINAL_THEME_LIGHT: ITheme = {
  background: "#fafafa",
  foreground: "#333333",
  cursor: "#333333",
  cursorAccent: "#fafafa",
  selectionBackground: "#add6ff",
  black: "#000000",
  red: "#cd3131",
  green: "#00bc00",
  yellow: "#949800",
  blue: "#0451a5",
  magenta: "#bc05bc",
  cyan: "#0598bc",
  white: "#555555",
  brightBlack: "#666666",
  brightRed: "#cd3131",
  brightGreen: "#14ce14",
  brightYellow: "#b5ba00",
  brightBlue: "#0451a5",
  brightMagenta: "#bc05bc",
  brightCyan: "#0598bc",
  brightWhite: "#a5a5a5",
};

/** 兼容旧调用点：默认深色。 */
export const TERMINAL_THEME = TERMINAL_THEME_DARK;

/** 按实际生效主题挑选终端配色。 */
export function terminalTheme(theme: "light" | "dark"): ITheme {
  return theme === "light" ? TERMINAL_THEME_LIGHT : TERMINAL_THEME_DARK;
}

/**
 * 后端把每个输出块 base64 编码后经 `terminal:data` 下发。
 * 解码失败时返回 null，调用方退化为按文本写入（避免丢字）。
 */
export function decodeBase64(data: string): Uint8Array | null {
  try {
    return Uint8Array.from(atob(data), (c) => c.charCodeAt(0));
  } catch {
    return null;
  }
}
