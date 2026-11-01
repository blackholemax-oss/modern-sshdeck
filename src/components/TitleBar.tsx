/**
 * 自定义窗口标题栏。
 *
 * ## 为什么自绘
 *
 * 系统默认标题栏跟随操作系统配色，与应用内的 shadcn 配色脱节：
 * 深色主题下窗口标题栏可能是浅灰，浅色主题下又变成深色，
 * 视觉上是两个不同的应用。这里改为**完全自绘**，颜色直接取自
 * `src/styles/globals.css` 的 shadcn 变量（`--card` / `--border` /
 * `--foreground` / `--muted-foreground`），因此**自动跟随主题切换**，
 * 无需任何额外同步逻辑。
 *
 * ## 拖拽
 *
 * Tauri 要求拖拽区域必须由前端声明。这里用 `data-tauri-drag-region`，
 * 但**只标在空白区域**：如果把按钮也标上，点击会被拖拽逻辑吞掉，
 * 窗口按钮就点不动了。
 *
 * ## 双击最大化
 *
 * `data-tauri-drag-region` 自带双击最大化行为，但仍显式处理一次，
 * 以保证在自定义区域内行为一致。
 */
import { useEffect, useState } from "react";
import { Minus, Square, X, Copy } from "lucide-react";

import { useTranslation } from "react-i18next";

import { cn } from "@/lib/utils";

/** 窗口按钮的通用样式：与 shadcn 的 ghost 按钮一致，但尺寸更紧凑。 */
const BTN =
  "flex h-8 w-11 items-center justify-center text-muted-foreground transition-colors " +
  "hover:bg-accent hover:text-accent-foreground active:bg-accent/80";

export function TitleBar() {
  const { t } = useTranslation();
  const [maximized, setMaximized] = useState(false);
  const [appWindow, setAppWindow] = useState<
    Awaited<ReturnType<typeof getWindow>>
  >(null);

  useEffect(() => {
    let un: (() => void) | undefined;
    let disposed = false;
    (async () => {
      try {
        const w = await getWindow();
        if (!w) return;
        if (disposed) return;
        setAppWindow(w);
        setMaximized(await w.isMaximized());
        // 跟随窗口状态：最大化时按钮应显示「还原」图标
        un = await w.onResized(async () => {
          try {
            setMaximized(await w.isMaximized());
          } catch {
            /* 窗口可能已销毁 */
          }
        });
      } catch {
        /* 非 Tauri 环境（浏览器预览）忽略 */
      }
    })();
    return () => {
      disposed = true;
      un?.();
    };
  }, []);

  const minimize = () => void appWindow?.minimize();
  const toggleMaximize = () => {
    if (!appWindow) return;
    if (maximized) void appWindow.unmaximize();
    else void appWindow.maximize();
  };
  const close = () => void appWindow?.close();

  return (
    <header
      className={cn(
        "flex h-9 shrink-0 select-none items-center",
        // 颜色全部来自主题变量 → 主题切换时自动更新
        "border-b border-border bg-card text-card-foreground",
      )}
      data-tauri-drag-region
      data-window-maximized={maximized ? "true" : "false"}
      onDoubleClick={toggleMaximize}
    >
      {/* 左侧标识：与应用内的字号/颜色体系一致 */}
      <div
        className="flex min-w-0 flex-1 items-center gap-2 pl-3"
        data-tauri-drag-region
      >
        <div className="flex size-4 items-center justify-center rounded-sm bg-primary text-primary-foreground">
          <span className="text-[9px] font-bold leading-none">S</span>
        </div>
        <span className="truncate text-xs font-medium tracking-tight">
          {t("app.title")}
        </span>
      </div>

      {/* 窗口按钮：不标 data-tauri-drag-region，否则点击会被拖拽逻辑吞掉 */}
      <div className="flex items-center">
        <button type="button" className={BTN} onClick={minimize} title={t("window.minimize")}>
          <Minus className="size-3.5" strokeWidth={1.75} />
        </button>
        <button
          type="button"
          className={BTN}
          onClick={toggleMaximize}
          title={maximized ? t("window.restore") : t("window.maximize")}
        >
          {maximized ? (
            <Copy className="size-3.5 -scale-x-100" strokeWidth={1.75} />
          ) : (
            <Square className="size-3.5" strokeWidth={1.75} />
          )}
        </button>
        <button
          type="button"
          className={cn(
            BTN,
            // 关闭按钮用 destructive 语义色，与组件库一致
            "hover:bg-destructive hover:text-destructive-foreground",
          )}
          onClick={close}
          title={t("window.close")}
        >
          <X className="size-3.5" strokeWidth={1.75} />
        </button>
      </div>
    </header>
  );
}

/** 动态取窗口对象（避免在非 Tauri 环境静态导入就报错）。 */
async function getWindow() {
  try {
    const mod = await import("@tauri-apps/api/window");
    return mod.getCurrentWindow();
  } catch {
    return null;
  }
}
