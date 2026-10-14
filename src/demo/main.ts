// Only selected by vite's screenshot dev mode; never a production entry.
export {};
localStorage.clear();
localStorage.setItem('xarchive-selected-account', '__overview__');
localStorage.setItem('xarchive-theme', new URLSearchParams(location.search).get('theme') === 'dark' ? 'dark' : 'light');
await import('../main');
