"""校验代码里用到的所有 i18n key 都存在于 zh/en 资源中。

## 为什么需要它

`t("host.connect")` 的入参是**普通字符串**，TypeScript 无法知道它是否存在。
key 缺失时 i18next 会把 key 名原样返回，界面上就出现 `host.connect` 这种字面量 ——
编译完全通过、测试也不会失败，只有肉眼才会发现。

这个脚本把「资源里的 key」与「代码里用到的 key」双向比对，缺一条就退出码 1。

## 用法

```bash
python scripts/check-i18n.py          # 检查，缺失则退出码 1
python scripts/check-i18n.py --list   # 额外列出全部 key（排查用）
```
"""
import io
import glob
import re
import sys

LOCALES = {"zh": "src/i18n/locales/zh.ts", "en": "src/i18n/locales/en.ts"}


def load_keys(path: str) -> set[str]:
    """把嵌套的 TS 对象展平成 `a.b.c` 形式的 key 集合。

    只解析本项目的固定风格（`key: value` 与 `key: {`），不做通用 TS 解析 ——
    资源文件的写法是我们自己控制的，保持简单比引入解析器更可靠。
    """
    text = io.open(path, encoding="utf-8").read()
    keys: set[str] = set()
    stack: list[tuple[int, str]] = []  # (缩进空格数, 段名)

    for line in text.split("\n"):
        stripped = line.strip()
        if not stripped or stripped.startswith(("//", "*", "/*")):
            continue
        indent = len(line) - len(line.lstrip())

        m_block = re.match(r"(\w+)\s*:\s*\{$", stripped)
        if m_block:
            while stack and stack[-1][0] >= indent:
                stack.pop()
            stack.append((indent, m_block.group(1)))
            continue

        m_leaf = re.match(r"(\w+)\s*:\s*", stripped)
        if m_leaf:
            while stack and stack[-1][0] >= indent:
                stack.pop()
            parts = [s for _, s in stack] + [m_leaf.group(1)]
            keys.add(".".join(parts))

    return keys


def collect_used() -> set[str]:
    """收集代码里引用的 key。"""
    used: set[str] = set()
    for f in glob.glob("src/**/*.tsx", recursive=True) + glob.glob("src/**/*.ts", recursive=True):
        if "i18n/locales" in f:
            continue
        s = io.open(f, encoding="utf-8").read()
        # t("...") 与 t('...')
        used |= set(re.findall(r"t\(\s*[\"']([^\"']+)[\"']", s))
        # 常量表里的 labelKey / titleKey / descKey
        for field in ("labelKey", "titleKey", "descKey"):
            used |= set(re.findall(field + r"\s*:\s*[\"']([^\"']+)[\"']", s))

    # 过滤掉非 key 的字符串（单段名、含斜杠/空格的路径等）
    return {k for k in used if re.match(r"^[a-zA-Z][\w.]*$", k) and "." in k}


def main() -> int:
    list_all = "--list" in sys.argv
    used = collect_used()
    zh_keys = load_keys(LOCALES["zh"])
    en_keys = load_keys(LOCALES["en"])

    bad = False

    # ① 代码用到但资源缺失（症状：界面显示 key 名）
    for loc, keys in (("zh", zh_keys), ("en", en_keys)):
        missing = sorted(k for k in used if k not in keys)
        if missing:
            bad = True
            print(f"[{loc}] 缺失 {len(missing)} 个 key（界面会显示 key 名）：")
            for k in missing:
                print(f"    {k}")
        else:
            print(f"[{loc}] OK — {len(keys)} 个 key，代码用到的 {len(used)} 个全部存在")

    # ② zh/en 结构不一致（en 少翻译时会违反 `Messages` 类型，tsc 会抓；
    #    这里额外做一次运行时比对，因为 tsc 只在校验时跑）
    only_zh = sorted(zh_keys - en_keys)
    only_en = sorted(en_keys - zh_keys)
    if only_zh or only_en:
        bad = True
        if only_zh:
            print(f"[结构] zh 有而 en 缺 {len(only_zh)} 个：")
            for k in only_zh:
                print(f"    {k}")
        if only_en:
            print(f"[结构] en 有而 zh 缺 {len(only_en)} 个：")
            for k in only_en:
                print(f"    {k}")

    # ③ 资源里存在但代码从未使用（不是错误，只是提示）
    unused = sorted(zh_keys - used)
    if unused:
        print(f"[提示] {len(unused)} 个 key 未被代码引用（可能是预留或已废弃）")
        if list_all:
            for k in unused:
                print(f"    {k}")

    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
