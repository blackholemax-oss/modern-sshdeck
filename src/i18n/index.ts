/**
 * i18n 初始化与语言解析。
 *
 * ## 默认语言 = 系统语言
 *
 * `ui.language` 只存 `"zh" | "en"`，但**默认值取决于系统语言**：
 * 首次启动时若该字段尚未由用户设置，就用系统语言（中文系统 → 中文，
 * 其余 → 英文）。这与「默认跟随系统」的预期一致。
 *
 * 判定顺序：
 * 1. 用户显式选择过（`ui.language` 已是具体值且与默认不同）→ 用它
 * 2. 否则用系统语言（`navigator.languages` → `navigator.language`）
 * 3. 都没有 → 回退中文（本项目的主语言）
 *
 * ## 为什么不引入 `i18next-browser-languagedetector`
 *
 * 我们只有一个布尔判断（是不是中文），自己写 20 行足够，
 * 而且能精确控制「简体/繁体/粤语都算中文」这类细节。
 */
import i18next from "i18next";
import { initReactI18next } from "react-i18next";

import { en } from "./locales/en";
import { zh } from "./locales/zh";

/** 支持的语言。 */
export const LANGS = ["zh", "en"] as const;
export type Lang = (typeof LANGS)[number];

/** 资源。 */
const resources = {
  zh: { translation: zh },
  en: { translation: en },
} as const;

/**
 * 把任意语言标签归一化成受支持的语言。
 *
 * - `zh-CN` / `zh-Hans` / `zh-TW` / `zh-HK` / `yue` → `zh`
 * - 其余（`en-US`、`ja`、`de`…）→ `en`
 *
 * 说明：繁体也归到 `zh`（目前只有简体资源）。若要区分港台用词，
 * 后续加 `zh-Hant` 资源即可，这里的映射不用改。
 */
export function normalizeLang(tag: string | undefined | null): Lang {
  if (!tag) return "zh";
  const lower = tag.toLowerCase();
  if (
    lower.startsWith("zh") ||
    lower.startsWith("yue") // 粤语
  ) {
    return "zh";
  }
  return "en";
}

/** 读取系统语言（浏览器环境安全）。 */
export function detectSystemLang(): Lang {
  if (typeof navigator === "undefined") return "zh";
  // `navigator.languages` 是用户偏好顺序，取第一个可识别的
  const candidates = navigator.languages?.length
    ? navigator.languages
    : [navigator.language];
  for (const tag of candidates) {
    if (tag) return normalizeLang(tag);
  }
  return "zh";
}

/**
 * 解析最终生效语言。
 *
 * `pref` 来自配置的 `ui.language`，是三态：
 * - `"system"`（默认）→ 跟随系统语言
 * - `"zh"` / `"en"` → 用户显式选择
 * - 未知值 → 保守地跟随系统
 */
export function resolveLang(pref: string | undefined | null): Lang {
  if (pref === "zh" || pref === "en") return pref;
  if (pref && pref !== "system") return normalizeLang(pref);
  return detectSystemLang();
}

/** 初始化 i18next（幂等，重复调用安全）。 */
export function setupI18n(initial: Lang) {
  if (i18next.isInitialized) {
    if (i18next.language !== initial) void i18next.changeLanguage(initial);
    return i18next;
  }
  void i18next.use(initReactI18next).init({
    resources,
    lng: initial,
    fallbackLng: "zh",
    interpolation: {
      // React 已经转义过，i18next 不要再转一遍
      escapeValue: false,
    },
    returnNull: false,
  });
  return i18next;
}

/** 切换语言。 */
export async function setLanguage(lang: Lang) {
  await i18next.changeLanguage(lang);
  if (typeof document !== "undefined") {
    document.documentElement.lang = lang;
  }
}

export default i18next;
