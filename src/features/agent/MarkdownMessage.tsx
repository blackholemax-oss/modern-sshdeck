/**
 * Agent 消息的 Markdown 渲染。
 *
 * ## 为什么用户消息不渲染
 *
 * 用户输入的**是纯文本**：如果他把 `# 标题` 当字面意思打进去，
 * 渲染成标题反而失真。所以只有 Agent（模型）的输出走 Markdown。
 *
 * ## 安全边界
 *
 * `react-markdown` 默认**不渲染裸 HTML**（没有 `rehype-raw`），
 * 因此模型输出的 `<script>` 之类会被当成文本，不会被激活。
 * 这对「模型可能输出任意内容」的运维 Agent 是必须的。
 *
 * 链接另外做了限制：只允许 http(s)，其余（`javascript:`、`file:`）降级为纯文本。
 *
 * ## 代码块
 *
 * `rehype-highlight` 用 highlight.js 做语法高亮。等宽字体沿用应用的字号体系，
 * 颜色取自主题变量，深/浅色主题下都能读。
 */
import { memo } from "react";
import ReactMarkdown from "react-markdown";
import rehypeHighlight from "rehype-highlight";
import remarkGfm from "remark-gfm";

const ALLOWED_PROTOCOLS = ["http:", "https:", "mailto:"];

export const MarkdownMessage = memo(function MarkdownMessage({ text }: { text: string }) {
  return (
    <div className="markdown-body text-[13px] leading-relaxed">
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        rehypePlugins={[[rehypeHighlight, { detect: false, ignoreMissing: true }]]}
        components={{
          // 链接：只放行安全协议，其余按纯文本渲染
          a({ href, children, ...props }) {
            const ok =
              href !== undefined &&
              ALLOWED_PROTOCOLS.some((p) => href.toLowerCase().startsWith(p));
            if (!ok) return <span>{children}</span>;
            return (
              <a
                {...props}
                href={href}
                target="_blank"
                rel="noreferrer noopener"
                className="text-sky-500 underline underline-offset-2 hover:text-sky-400"
              >
                {children}
              </a>
            );
          },

          // 表格：加横向滚动，避免窄栏里撑破布局
          table({ children }) {
            return (
              <div className="my-2 overflow-x-auto">
                <table className="border-collapse border border-border text-[12px]">
                  {children}
                </table>
              </div>
            );
          },
          th({ children }) {
            return (
              <th className="border border-border bg-muted/50 px-2 py-1 text-left font-medium">
                {children}
              </th>
            );
          },
          td({ children }) {
            return <td className="border border-border px-2 py-1">{children}</td>;
          },

          // 代码块：等宽 + 主题底色；语言由 highlight.js 标注
          pre({ children }) {
            return (
              <pre className="my-2 overflow-x-auto rounded-md border border-border bg-muted/40 p-2 font-mono text-[12px] leading-snug">
                {children}
              </pre>
            );
          },
          code({ className, children, ...props }) {
            // 行内代码没有 hljs 语言类；块级由 <pre> 包裹
            const inline = !className;
            if (inline) {
              return (
                <code className="rounded bg-muted px-1 py-0.5 font-mono text-[12px]">
                  {children}
                </code>
              );
            }
            return (
              <code className={className} {...props}>
                {children}
              </code>
            );
          },

          // 段落/列表：压缩外边距，避免每条消息之间空隙过大
          p({ children }) {
            return <p className="my-1 first:mt-0 last:mb-0">{children}</p>;
          },
          ul({ children }) {
            return <ul className="my-1 list-disc pl-5">{children}</ul>;
          },
          ol({ children }) {
            return <ol className="my-1 list-decimal pl-5">{children}</ol>;
          },
          li({ children }) {
            return <li className="my-0.5">{children}</li>;
          },
          h1({ children }) {
            return <h1 className="my-2 text-[15px] font-semibold">{children}</h1>;
          },
          h2({ children }) {
            return <h2 className="my-2 text-[14px] font-semibold">{children}</h2>;
          },
          h3({ children }) {
            return <h3 className="my-1.5 text-[13px] font-semibold">{children}</h3>;
          },
          blockquote({ children }) {
            return (
              <blockquote className="my-2 border-l-2 border-border pl-2 text-muted-foreground">
                {children}
              </blockquote>
            );
          },
          hr() {
            return <hr className="my-3 border-border" />;
          },
        }}
      >
        {text}
      </ReactMarkdown>
    </div>
  );
});
