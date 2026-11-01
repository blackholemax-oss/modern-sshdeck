/**
 * 单个终端 pane —— xterm.js 的命令式封装。
 *
 * 关键约束（见 ARCHITECTURE §6.3）：
 * - 每个 pane **各自**订阅 `terminal:data` / `terminal:closed`，只处理 `sessionId`
 *   匹配自己的事件。终端输出绝不进 React state，直接 `term.write`。
 * - 输入：`term.onData` → 16ms 合并节流 → `api.sendInput`。
 * - 尺寸：`ResizeObserver` → 100ms 去抖 → `fit()` → `api.resize`。
 * - 清理：`term.dispose()` + 取消所有 unlisten + 清空定时器。`dispose` 幂等，
 *   StrictMode 下 effect 跑两次也安全。
 *
 * 一个容易踩的坑：终端实例会因字号/布局变化被重建，而订阅 effect 不一定同时重跑。
 * 因此所有回调里都通过 `termRef.current` 取**当前**实例，绝不在 effect 顶部捕获。
 */
import { useEffect, useRef, useState } from "react";
import { ChevronDown, ChevronUp, Terminal as TerminalIcon, X } from "lucide-react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { FitAddon } from "@xterm/addon-fit";
import { SearchAddon } from "@xterm/addon-search";
import { WebLinksAddon } from "@xterm/addon-web-links";
import { Terminal } from "@xterm/xterm";

import { api, events, SshdeckError } from "@/shared/api";
import type { SessionInfo } from "@/shared/ipc";
import { resolveTheme, useSettingsStore } from "@/stores/settings";

import { releaseConsumer, takePending } from "./pendingOutput";
import {
  TERMINAL_FONT_FAMILY,
  TERMINAL_FONT_SIZE,
  TERMINAL_SCROLLBACK,
  terminalTheme,
  decodeBase64,
} from "./terminalOptions";

/** 输入合并窗口。 */
const INPUT_FLUSH_MS = 16;
/** 尺寸变化去抖窗口。 */
const RESIZE_DEBOUNCE_MS = 100;

export interface TerminalPaneProps {
  sessionId: string | null;
  /** 会话元信息（用于 pane 角落的 user@host 展示）。 */
  session?: SessionInfo;
  /** 当前聚焦的 pane（用于高亮边框与自动聚焦）。 */
  active?: boolean;
  /** 点击空位时触发（由上层选择主机）。 */
  onRequestConnect?: () => void;
}

export function TerminalPane({
  sessionId,
  session,
  active = false,
  onRequestConnect,
}: TerminalPaneProps) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const termRef = useRef<Terminal | null>(null);
  const searchRef = useRef<SearchAddon | null>(null);
  /** 供 effect 内回调读取最新会话，避免因 sessionId 变化重建终端。 */
  const sessionIdRef = useRef<string | null>(sessionId);
  // 终端外观来自设置：字号 / 字体族 / 回滚 / 光标样式都是可配置项。
  const fontSize = useSettingsStore((s) => s.config?.terminal.fontSize);
  const scrollback = useSettingsStore((s) => s.config?.terminal.scrollback);
  const fontFamily = useSettingsStore((s) => s.config?.terminal.fontFamily);
  const cursorStyle = useSettingsStore((s) => s.config?.terminal.cursorStyle);
  const cursorBlink = useSettingsStore((s) => s.config?.terminal.cursorBlink);
  const themePref = useSettingsStore((s) => s.config?.ui.theme) ?? "dark";
  const resolvedTheme = resolveTheme(themePref);

  const [searchOpen, setSearchOpen] = useState(false);
  const [searchTerm, setSearchTerm] = useState("");

  sessionIdRef.current = sessionId;

  const hasSession = sessionId !== null;

  // ① 终端实例生命周期：只在「会话 / 字号 / scrollback」变化时重建。
  //    换绑会话时也必须重建，否则上一个会话的屏幕内容会残留。
  useEffect(() => {
    if (!hasSession) return;
    const container = containerRef.current;
    if (!container) return;

    const term = new Terminal({
      // 空字符串表示用内置字体栈
      fontFamily: fontFamily && fontFamily.trim() ? fontFamily : TERMINAL_FONT_FAMILY,
      fontSize: fontSize ?? TERMINAL_FONT_SIZE,
      scrollback: scrollback ?? TERMINAL_SCROLLBACK,
      theme: terminalTheme(resolvedTheme),
      cursorStyle: cursorStyle ?? "block",
      cursorBlink: cursorBlink ?? true,
      allowProposedApi: true,
      convertEol: false,
      macOptionIsMeta: true,
    });

    const fitAddon = new FitAddon();
    const searchAddon = new SearchAddon();
    const linksAddon = new WebLinksAddon((_event, uri) => {
      // 外链交给系统浏览器；失败静默，不打断终端使用。
      void openUrl(uri).catch(() => undefined);
    });

    term.loadAddon(fitAddon);
    term.loadAddon(searchAddon);
    term.loadAddon(linksAddon);
    term.open(container);

    termRef.current = term;
    searchRef.current = searchAddon;

    let disposed = false;
    let inputBuffer = "";
    let inputTimer: number | undefined;
    let resizeTimer: number | undefined;
    let lastCols = 0;
    let lastRows = 0;

    const pushResize = () => {
      const sid = sessionIdRef.current;
      if (!sid) return;
      if (term.cols === lastCols && term.rows === lastRows) return;
      lastCols = term.cols;
      lastRows = term.rows;
      void api.resize(sid, term.cols, term.rows).catch((err: unknown) => {
        // 会话已断开时 resize 必然失败，无需打扰用户。
        if (err instanceof SshdeckError && err.code === "NOT_FOUND") return;
      });
    };

    const flushInput = () => {
      inputTimer = undefined;
      const chunk = inputBuffer;
      inputBuffer = "";
      if (!chunk) return;
      const sid = sessionIdRef.current;
      if (!sid) return;
      // 乐观发送：失败只记录不弹窗（终端已本地回显）。
      void api.sendInput(sid, chunk).catch(() => undefined);
    };

    const dataDisposable = term.onData((data) => {
      if (!sessionIdRef.current) return;
      inputBuffer += data;
      if (inputTimer === undefined) {
        inputTimer = window.setTimeout(flushInput, INPUT_FLUSH_MS);
      }
    });

    // Ctrl/Cmd+F 打开 pane 内搜索条（不把该键送给远端 shell）。
    term.attachCustomKeyEventHandler((event) => {
      if (
        event.type === "keydown" &&
        (event.ctrlKey || event.metaKey) &&
        event.key.toLowerCase() === "f"
      ) {
        setSearchOpen(true);
        return false;
      }
      return true;
    });

    const observer = new ResizeObserver(() => {
      if (resizeTimer !== undefined) window.clearTimeout(resizeTimer);
      resizeTimer = window.setTimeout(() => {
        resizeTimer = undefined;
        if (disposed) return;
        try {
          fitAddon.fit();
        } catch {
          // 容器尺寸为 0（隐藏/未布局）时 fit 会抛错，忽略。
          return;
        }
        pushResize();
      }, RESIZE_DEBOUNCE_MS);
    });
    observer.observe(container);

    // 首次布局：等一帧再 fit，避免容器尚未拿到真实尺寸。
    const raf = window.requestAnimationFrame(() => {
      if (disposed) return;
      try {
        fitAddon.fit();
      } catch {
        return;
      }
      pushResize();
    });

    return () => {
      disposed = true;
      window.cancelAnimationFrame(raf);
      observer.disconnect();
      if (inputTimer !== undefined) window.clearTimeout(inputTimer);
      if (resizeTimer !== undefined) window.clearTimeout(resizeTimer);
      dataDisposable.dispose();
      linksAddon.dispose();
      searchAddon.dispose();
      fitAddon.dispose();
      term.dispose();
      if (termRef.current === term) termRef.current = null;
      if (searchRef.current === searchAddon) searchRef.current = null;
    };
  }, [sessionId, fontSize, scrollback, fontFamily, cursorStyle, cursorBlink, resolvedTheme]);

  // ② 挂载前已被暂存的输出回放：`ssh_connect` 返回时远端提示符往往已经下发，
  //    早于 React 挂载 pane。这里同步取回（同时把自己登记为该会话的消费者，
  //    之后全局捕获不再为它缓冲，避免重复写入）。
  useEffect(() => {
    if (!sessionId) return;
    const buffered = takePending(sessionId);
    const term = termRef.current;
    if (term) {
      for (const chunk of buffered) term.write(chunk);
    }
    return () => {
      releaseConsumer(sessionId);
    };
  }, [sessionId]);

  // ③ 本 pane 专属的 IPC 订阅：只认自己的 sessionId。
  //    回调内始终读 termRef.current —— 字号变化会重建终端而本 effect 不重跑。
  useEffect(() => {
    if (!sessionId) return;

    const writeChunk = (data: string) => {
      const term = termRef.current;
      if (!term) return;
      const bytes = decodeBase64(data);
      if (bytes) term.write(bytes);
      else term.write(data);
    };

    let disposed = false;
    let unlistenData: (() => void) | undefined;
    let unlistenClosed: (() => void) | undefined;

    void (async () => {
      const offData = await events.onTerminalData((payload) => {
        if (payload.sessionId !== sessionId) return;
        writeChunk(payload.data);
      });
      if (disposed) {
        offData();
        return;
      }
      unlistenData = offData;

      const offClosed = await events.onTerminalClosed((payload) => {
        if (payload.sessionId !== sessionId) return;
        const term = termRef.current;
        if (!term) return;
        const line = payload.reason
          ? `[连接已断开] ${payload.reason}`
          : "[连接已断开]";
        term.writeln(`\r\n\x1b[33m${line}\x1b[0m`);
      });
      if (disposed) {
        offClosed();
        return;
      }
      unlistenClosed = offClosed;
    })();

    return () => {
      disposed = true;
      unlistenData?.();
      unlistenClosed?.();
    };
  }, [sessionId]);

  // ④ 聚焦：激活 pane 时把焦点交给 xterm。
  useEffect(() => {
    if (active && sessionId) termRef.current?.focus();
  }, [active, sessionId]);

  if (!hasSession) {
    return (
      <button
        type="button"
        onClick={onRequestConnect}
        className="flex h-full w-full flex-col items-center justify-center gap-1 transition-colors"
        style={{
          background: resolvedTheme === "light" ? "#fafafa" : "#1e1e1e",
          color: resolvedTheme === "light" ? "#6b7280" : "#737373",
        }}
      >
        <TerminalIcon className="size-5 opacity-50" />
        <span className="text-sm">点击连接</span>
        <span className="text-xs opacity-60">在左侧选择主机</span>
      </button>
    );
  }

  return (
    <div
      className={
        "relative h-full w-full overflow-hidden border " +
        (active ? "border-ring" : "border-transparent")
      }
      onMouseDown={() => {
        // 点击 pane 任意位置把焦点交回 xterm。
        termRef.current?.focus();
      }}
    >
      <div ref={containerRef} className="h-full w-full" />

      {session ? (
        <div className="pointer-events-none absolute top-0.5 left-2 max-w-[80%] truncate px-1 text-[11px] text-neutral-400 opacity-60">
          {session.username}@{session.host}:{session.port}
        </div>
      ) : null}

      {searchOpen ? (
        <div className="absolute top-1 right-1 flex items-center gap-1 rounded-md border border-neutral-700 bg-neutral-800/95 px-1 py-0.5 shadow">
          <input
            autoFocus
            value={searchTerm}
            onChange={(e) => setSearchTerm(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                if (e.shiftKey) searchRef.current?.findPrevious(searchTerm);
                else searchRef.current?.findNext(searchTerm);
              } else if (e.key === "Escape") {
                setSearchOpen(false);
                termRef.current?.focus();
              }
            }}
            placeholder="搜索终端输出"
            className="h-6 w-40 bg-transparent px-1 text-xs text-neutral-100 outline-none placeholder:text-neutral-500"
          />
          <button
            type="button"
            title="下一个"
            className="rounded p-0.5 text-neutral-400 hover:text-neutral-100"
            onClick={() => searchRef.current?.findNext(searchTerm)}
          >
            <ChevronDown className="size-3.5" />
          </button>
          <button
            type="button"
            title="上一个"
            className="rounded p-0.5 text-neutral-400 hover:text-neutral-100"
            onClick={() => searchRef.current?.findPrevious(searchTerm)}
          >
            <ChevronUp className="size-3.5" />
          </button>
          <button
            type="button"
            title="关闭搜索"
            className="rounded p-0.5 text-neutral-400 hover:text-neutral-100"
            onClick={() => {
              setSearchOpen(false);
              searchRef.current?.clearDecorations();
              termRef.current?.focus();
            }}
          >
            <X className="size-3.5" />
          </button>
        </div>
      ) : null}
    </div>
  );
}
