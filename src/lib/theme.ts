export type ThemePreference = 'light' | 'dark' | 'system';
const themeKey = 'xarchive-theme';

export function parseTheme(value: string | null): ThemePreference {
  return value === 'dark' || value === 'system' ? value : 'light';
}

export function resolveTheme(preference: ThemePreference, systemDark: boolean): 'light' | 'dark' {
  return preference === 'system' ? systemDark ? 'dark' : 'light' : preference;
}

export function readTheme(storage?: Pick<Storage, 'getItem'>): ThemePreference {
  try { return parseTheme((storage ?? localStorage).getItem(themeKey)); } catch { return 'light'; }
}

export function writeTheme(preference: ThemePreference, storage?: Pick<Storage, 'setItem'>): void {
  try { (storage ?? localStorage).setItem(themeKey, preference); } catch { /* Theme still works for this session. */ }
}

export function applyTheme(preference: ThemePreference): void {
  document.documentElement.dataset.theme = resolveTheme(preference, window.matchMedia('(prefers-color-scheme: dark)').matches);
}
