// Tiny i18n helper. French is the default language.

import fr from "./locales/fr.json";
import en from "./locales/en.json";
import it from "./locales/it.json";
import es from "./locales/es.json";
import ru from "./locales/ru.json";
import ptBR from "./locales/pt-BR.json";

const LOCALES: Record<string, Record<string, string>> = {
  fr, en, it, es, ru, "pt-BR": ptBR,
};

let current: Record<string, string> = fr;

export function setLang(code: string): void {
  current = LOCALES[code] ?? fr;
  applyDom();
}

export function t(key: string): string {
  return current[key] ?? key;
}

/** Apply translations to all elements carrying data-i18n* attributes. */
function applyDom(): void {
  document.querySelectorAll<HTMLElement>("[data-i18n]").forEach((el) => {
    el.textContent = t(el.dataset.i18n!);
  });
  document.querySelectorAll<HTMLInputElement>("[data-i18n-ph]").forEach((el) => {
    el.placeholder = t(el.dataset.i18nPh!);
  });
  document.querySelectorAll<HTMLElement>("[data-i18n-title]").forEach((el) => {
    el.title = t(el.dataset.i18nTitle!);
  });
}
