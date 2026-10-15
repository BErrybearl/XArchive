import type { ArchivedAccount, ArchiveSettings, LoginStatus, StorageStatus } from '../lib/program';

const settings: ArchiveSettings = {
  target: '', include_timeline: false, media_types: { image: true, video: true, gif: true },
  include_retweets: true, date_start: null, date_end: null, image_quality: 'orig',
  video_quality: 'highest', request_pace: 'standard', concurrency: 2, retry_limit: 3,
};
const verifiedAt = Date.UTC(2026, 9, 12, 2, 30) / 1000;
const accounts: ArchivedAccount[] = [
  { user_id: '10001', screen_name: 'sample_artist', display_name: '示例画师', tweets: 324, media: 486, bytes: 402653184 },
  { user_id: '10002', screen_name: 'daily_photos', display_name: '每日摄影', tweets: 218, media: 352, bytes: 335544320 },
  { user_id: '10003', screen_name: 'travel_notes', display_name: '旅行手记', tweets: 156, media: 247, bytes: 268435456 },
].map(account => ({ ...account, pending: 0, failed: 0, last_full_at: verifiedAt,
  last_incremental_at: null, full_scope: null, full_settings: null, summary: '归档完成' }));
const storage: StorageStatus = {
  layout: 'C',
  path: 'D:\\XArchive\\data', initialized: true, development: false,
  tweets: 698, media: 1085, sources: 1124, pending: 0,
  bytes: accounts.reduce((sum, account) => sum + account.bytes, 0),
};
const login: LoginStatus = {
  saved: true, pending: false,
  account: { user_id: '10000', screen_name: 'demo_user', display_name: '演示用户' },
  verified_at: verifiedAt, proxy: '', cleanup_warning: null,
};

export function isTauri(): boolean { return true; }

// Deliberately allow only startup reads. Unsupported actions fail without side effects.
export async function invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  let result: unknown;
  switch (command) {
    case 'get_program_info': result = { name: 'XArchive', version: '0.1.0', development: false }; break;
    case 'get_storage_status': case 'initialize_storage': result = storage; break;
    case 'get_login_status': result = login; break;
    case 'get_network_route': result = '系统代理'; break;
    case 'list_archived_accounts': result = accounts; break;
    case 'get_archive_settings': result = settings; break;
    case 'get_account_settings': result = { ...settings, target: args?.account ?? '' }; break;
    case 'get_account_data': result = []; break;
    case 'get_archive_status': case 'get_export_status': result = { running: false, stage: '' }; break;
    case 'get_layout_status': result = { running: false, layout: 'C', stage: '' }; break;
    case 'get_export_directory': result = null; break;
    case 'save_archive_settings': result = undefined; break;
    default: throw new Error(`截图演示模式不支持操作：${command}`);
  }
  return structuredClone(result) as T;
}
