import i18n from "i18next";
import { initReactI18next } from "react-i18next";
import zh from "./zh";
import en from "./en";

i18n
  .use(initReactI18next)
  .init({
    resources: {
      zh: { translation: zh },
      en: { translation: en },
    },
    lng: localStorage.getItem("pisci-language") || "zh",
    fallbackLng: "zh",
    interpolation: {
      escapeValue: false,
    },
  });

export default i18n;

/** 切换语言并持久化 */
export function setLanguage(lang: "zh" | "en") {
  i18n.changeLanguage(lang);
  localStorage.setItem("pisci-language", lang);
}
