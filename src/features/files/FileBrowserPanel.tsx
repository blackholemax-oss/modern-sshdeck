/**
 * 远程文件面板（SFTP）。
 *
 * ## 交互模型
 *
 * 单栏「目录 → 文件」两级：点目录进下一层，点文件在右侧/下方预览。
 * 不做双栏（本地↔远程）拖拽 —— 那需要先有本机文件浏览，属于更大的工程；
 * 这里先解决「看一眼远端文件内容」这个最高频需求。
 *
 * ## 安全边界
 *
 * 后端 `normalize_path` 拒绝 `..`，所以这里不需要再做路径校验；
 * 但面包屑仍然只显示**后端返回的真实路径**（canonicalize 后的），
 * 不自己拼接猜测 —— 否则符号链接会让显示与实际不符。
 */
import { useEffect, useState } from "react";
import {
  AlertTriangle,
  ChevronLeft,
  File as FileIcon,
  FileText,
  Folder,
  Link2,
  Loader2,
  RefreshCw,
} from "lucide-react";
import { useTranslation } from "react-i18next";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { ScrollArea } from "@/components/ui/scroll-area";
import { api, SshdeckError } from "@/shared/api";
import type { DirListing, FilePreview, RemoteEntry } from "@/shared/ipc";

/** 人类可读的文件大小。 */
function fmtSize(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1048576).toFixed(1)} MB`;
  return `${(n / 1073741824).toFixed(2)} GB`;
}

const KIND_ICON: Record<string, React.ReactNode> = {
  dir: <Folder className="size-3.5 text-sky-500" />,
  file: <FileIcon className="size-3.5 text-muted-foreground" />,
  symlink: <Link2 className="size-3.5 text-violet-500" />,
};

export function FileBrowserPanel({ sessionId }: { sessionId: string }) {
  const { t } = useTranslation();
  const [listing, setListing] = useState<DirListing | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [preview, setPreview] = useState<FilePreview | null>(null);
  const [previewing, setPreviewing] = useState(false);

  const load = async (path: string) => {
    setLoading(true);
    setError(null);
    setPreview(null);
    try {
      setListing(await api.fsListDir(sessionId, path));
    } catch (err) {
      setError(err instanceof SshdeckError ? `${err.code}: ${err.message}` : String(err));
      setListing(null);
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    void load("/");
  }, [sessionId]);

  const openEntry = async (e: RemoteEntry) => {
    if (e.kind === "dir") {
      void load(e.path);
      return;
    }
    setPreviewing(true);
    setError(null);
    try {
      setPreview(await api.fsReadFile(sessionId, e.path));
    } catch (err) {
      setError(err instanceof SshdeckError ? `${err.code}: ${err.message}` : String(err));
    } finally {
      setPreviewing(false);
    }
  };

  const parent = listing?.parent;
  const nothing = !loading && listing && listing.entries.length === 0;

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      {/* 工具栏 */}
      <div className="flex shrink-0 items-center gap-1.5 border-b border-border px-2 py-1.5">
        <Button
          variant="ghost"
          size="sm"
          className="h-6 w-6 p-0"
          disabled={!parent}
          onClick={() => parent && void load(parent)}
          title={t("files.up")}
        >
          <ChevronLeft className="size-3.5" />
        </Button>
        <span className="min-w-0 flex-1 truncate font-mono text-[11px] text-muted-foreground">
          {listing?.path ?? "/"}
        </span>
        <Button
          variant="ghost"
          size="sm"
          className="h-6 w-6 p-0"
          onClick={() => void load(listing?.path ?? "/")}
          title={t("common.refresh")}
        >
          {loading ? (
            <Loader2 className="size-3.5 animate-spin" />
          ) : (
            <RefreshCw className="size-3.5" />
          )}
        </Button>
      </div>

      {error ? (
        <div className="mx-2 mt-2 flex items-start gap-1.5 rounded-md border border-destructive/40 bg-destructive/10 px-2 py-1.5 text-[11px] text-destructive">
          <AlertTriangle className="mt-0.5 size-3 shrink-0" />
          <span className="break-all">{error}</span>
        </div>
      ) : null}

      <ScrollArea className="min-h-0 flex-1">
        {/* 目录列表 */}
        {listing?.entries.length ? (
          <div className="p-1">
            {listing.entries.map((e) => (
              <button
                key={e.path}
                type="button"
                onClick={() => void openEntry(e)}
                className="flex w-full items-center gap-1.5 rounded px-1.5 py-1 text-left text-[11px] hover:bg-accent"
              >
                {KIND_ICON[e.kind] ?? KIND_ICON.file}
                <span className="min-w-0 flex-1 truncate">{e.name}</span>
                {e.kind !== "dir" ? (
                  <span className="shrink-0 font-mono text-[10px] text-muted-foreground">
                    {fmtSize(e.size)}
                  </span>
                ) : null}
              </button>
            ))}
          </div>
        ) : null}

        {nothing ? (
          <p className="px-2 py-3 text-[11px] text-muted-foreground">
            {t("files.emptyDir")}
          </p>
        ) : null}

        {/* 文件预览 */}
        {previewing ? (
          <p className="px-2 py-3 text-[11px] text-muted-foreground">{t("files.previewing")}</p>
        ) : null}

        {preview ? (
          <div className="border-t border-border p-2">
            <div className="mb-1.5 flex items-center gap-1.5">
              {preview.isText ? (
                <FileText className="size-3.5 text-muted-foreground" />
              ) : (
                <FileIcon className="size-3.5 text-muted-foreground" />
              )}
              <span className="min-w-0 flex-1 truncate font-mono text-[10px]">
                {preview.path.split("/").pop()}
              </span>
              <Badge variant="outline" className="h-4 px-1 text-[9px]">
                {fmtSize(preview.size)}
              </Badge>
            </div>

            {preview.reason ? (
              <p className="mb-1 rounded border border-amber-500/40 bg-amber-500/10 px-1.5 py-1 text-[10px] text-amber-600 dark:text-amber-400">
                {preview.reason}
              </p>
            ) : null}

            {preview.content !== undefined ? (
              <pre className="max-h-72 overflow-auto rounded border border-border bg-muted/30 p-1.5 font-mono text-[10px] leading-relaxed whitespace-pre-wrap break-all">
                {preview.content}
              </pre>
            ) : null}
          </div>
        ) : null}
      </ScrollArea>
    </div>
  );
}
