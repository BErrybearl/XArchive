interface SearchAccount { screen_name: string; display_name: string }

// A task stays global for stop/navigation, but its metrics belong to its account.
export function progressForAccount<T extends { target: { user_id: string; screen_name: string } | null }>(
  progress: T | null, account: { user_id: string; screen_name: string } | undefined,
): T | null {
  if (!progress?.target || !account) return null;
  const target = progress.target;
  return target.user_id && account.user_id
    ? target.user_id === account.user_id ? progress : null
    : target.screen_name.toLowerCase() === account.screen_name.toLowerCase() ? progress : null;
}

// cancel_login releases pending before reading the saved state. These errors are
// from that final read, so an unreadable old file must not keep the UI pending.
export function cancelledLoginState<T extends { saved: boolean; pending: boolean; account: unknown; verified_at: number | null }>(
  current: T, error: unknown,
): T {
  const readErrors = [
    '无法读写本机登录设置，原有登录状态保持不变',
    '无法加密或解密登录状态，请在原 Windows 用户下重试或重新登录',
    '本机登录状态不完整或格式不兼容，请重新登录',
  ];
  return typeof error === 'string' && readErrors.includes(error)
    ? { ...current, pending: false, saved: false, account: null, verified_at: null } : current;
}

// Keep the accepted names and homepage URLs aligned with platform::target_name.
export function accountName(raw: string): string | null {
  let name = raw.trim();
  if (name.includes('://')) {
    try {
      const url = new URL(name);
      if (url.protocol !== 'https:' || !['x.com', 'www.x.com', 'twitter.com', 'www.twitter.com'].includes(url.hostname)
        || url.username || url.password || url.port) return null;
      name = url.pathname.replace(/^\/+|\/+$/g, '');
    } catch { return null; }
  } else name = name.replace(/^@/, '');
  return /^[a-zA-Z0-9_]{1,15}$/.test(name)
    && !['home', 'explore', 'notifications', 'messages', 'settings', 'i', 'search'].includes(name.toLowerCase()) ? name : null;
}

export function filterAccounts<T extends SearchAccount>(accounts: T[], query: string): T[] {
  const term = (accountName(query) ?? query.trim().replace(/^@/, '')).toLowerCase();
  return accounts.filter(a => a.screen_name.toLowerCase().includes(term) || a.display_name.toLowerCase().includes(term));
}

export function accountToAdd(accounts: SearchAccount[], query: string): string | null {
  const name = accountName(query);
  return name && !accounts.some(a => a.screen_name.toLowerCase() === name.toLowerCase()) ? name : null;
}

export function bytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1048576) return `${(n / 1024).toFixed(n < 10240 ? 1 : 0)} KB`;
  if (n < 1073741824) return `${(n / 1048576).toFixed(1)} MB`;
  return `${(n / 1073741824).toFixed(2)} GB`;
}
