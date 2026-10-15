import { invoke, isTauri } from '@tauri-apps/api/core';

export interface ProgramInfo {
  name: string;
  version: string;
  development: boolean;
}

export interface LicenseNotices {
  titles: string[];
  text: string;
  section: number;
}

export async function readLicenseNotices(section = 0): Promise<LicenseNotices> {
  return invoke<LicenseNotices>('get_license_notices', { section });
}

export interface StorageStatus {
  layout: StorageLayout;
  path: string;
  initialized: boolean;
  development: boolean;
  tweets: number;
  media: number;
  sources: number;
  pending: number;
  bytes: number;
}

export async function readProgramInfo(): Promise<ProgramInfo | null> {
  if (!isTauri()) {
    return null;
  }

  return invoke<ProgramInfo>('get_program_info');
}

export type StorageLayout = 'A' | 'C';
export interface LayoutProgress {
  running:boolean; layout:StorageLayout; done:number; total:number; renamed:number;
  stage:string; current:string; backup:string; stopped:boolean; error:string|null;
}
export function readLayoutStatus():Promise<LayoutProgress> {return invoke('get_layout_status');}
export function organizeMedia(layout:StorageLayout,directory:string):Promise<LayoutProgress> {return invoke('organize_media',{layout,directory});}
export function stopLayout():Promise<void> {return invoke('stop_layout');}

export async function readStorageStatus(): Promise<StorageStatus | null> {
  return isTauri() ? invoke<StorageStatus>('get_storage_status') : null;
}

export async function initializeStorage(): Promise<StorageStatus> {
  return invoke<StorageStatus>('initialize_storage');
}

export async function chooseDataDirectory(): Promise<StorageStatus | null> {
  return invoke<StorageStatus | null>('choose_data_directory');
}

export interface LoginStatus {
  saved: boolean;
  pending: boolean;
  account: { user_id: string; screen_name: string; display_name: string } | null;
  verified_at: number | null;
  proxy: string;
  cleanup_warning: string | null;
}

export async function readLoginStatus(): Promise<LoginStatus> {
  return invoke<LoginStatus>('get_login_status');
}

export async function beginLogin(proxy: string): Promise<LoginStatus> {
  return invoke<LoginStatus>('begin_login', { proxy });
}

export async function confirmLogin(): Promise<LoginStatus> {
  return invoke<LoginStatus>('confirm_login');
}

export function manualLogin(authToken: string, ct0: string, proxy: string): Promise<LoginStatus> {
  return invoke<LoginStatus>('manual_login', { authToken, ct0, proxy });
}

export async function cancelLogin(): Promise<LoginStatus> {
  return invoke<LoginStatus>('cancel_login');
}

export async function clearLoginBrowserProfile(): Promise<void> {
  return invoke('clear_login_browser_profile');
}

export type PlatformCategory = 'proxy_network' | 'authentication' | 'interface_changed' | 'rate_limit'
  | 'invalid_target' | 'target_unavailable' | 'local_state' | 'busy';

export interface PlatformError {
  category: PlatformCategory;
  message: string;
  suggestion: string;
  reset_at: number | null;
  http_status: number | null;
  network_route: string | null;
}

export interface ProbeResult {
  ok: boolean;
  stage: string;
  current_account: LoginStatus['account'];
  target_account: LoginStatus['account'];
  stats: { tweets: number; images: number; videos: number; gifs: number; unavailable: number; has_next_page: boolean } | null;
  rate_limits: Record<string, { remaining: number | null; reset_at: number | null }>;
  notices: string[];
  error: PlatformError | null;
}

export interface RefreshResult {
  ok: boolean;
  updated_operations: string[];
  notices: string[];
  error: PlatformError | null;
}

export async function probePlatform(target: string): Promise<ProbeResult> {
  return invoke<ProbeResult>('probe_platform', { target });
}

export async function refreshPlatformParams(): Promise<RefreshResult> {
  return invoke<RefreshResult>('refresh_platform_params');
}

export function platformCategoryLabel(category: PlatformCategory): string {
  return { proxy_network: '代理/网络', authentication: '认证失效或访问验证', interface_changed: '接口变化',
    rate_limit: '限流', invalid_target: '目标输入', target_unavailable: '目标不可访问', local_state: '本机状态', busy: '操作进行中' }[category];
}

export function resetTime(seconds: number | null): string {
  return seconds === null ? '平台未提供' : new Date(seconds * 1000).toLocaleString('zh-CN');
}


export interface LegacySelection { path: string; destination: string }
export interface LegacyProgress { running: boolean; stage: string; done: number; total: number }
export interface LegacyReport {
  passed: boolean; stopped: boolean; destination: string; snapshot: string; report_path: string;
  source_sha256_before: string; source_sha256_after: string;
  counts: { old_tweets: number; old_media_rows: number; expected_media: number; old_sources: number;
    imported_tweets: number; imported_media: number; imported_sources: number; digests_checked: number };
  missing: string[]; mismatches: string[]; pending: { key: string; items: unknown }[];
  notices: string[]; error: string | null;
}
export interface LegacyStatus { progress: LegacyProgress; report: LegacyReport | null }
export async function chooseLegacyFile(): Promise<LegacySelection | null> {
  return invoke<LegacySelection | null>('choose_legacy_file');
}
export async function importLegacy(selection: LegacySelection): Promise<LegacyReport> {
  return invoke<LegacyReport>('import_legacy', { path: selection.path, destination: selection.destination });
}
export async function readLegacyStatus(): Promise<LegacyStatus> {
  return invoke<LegacyStatus>('get_legacy_import_status');
}
export async function stopLegacyImport(): Promise<void> { return invoke('stop_legacy_import'); }

export interface ArchiveProgress {
 started_at: number; finished_at: number|null; page_item: number; page_total: number; current_item: string; bytes: number;
 running: boolean; stage: string; target: LoginStatus['account']; pages: number; tweets: number;
 added: number; reused: number; existing: number; date_ended: boolean; skipped: number; failed: number; pending: number; exhausted: boolean;
 outcome: string; summary: string; rate_limits: ProbeResult['rate_limits']; logs: string[];
 failures: {tweet_id:string;kind:string;attempts:number;reason:string;terminal:boolean}[];
 error: PlatformError|null;
}
export interface ArchiveSettings {
 target: string; include_timeline: boolean;
 media_types: {image:boolean;video:boolean;gif:boolean}|null;
 include_retweets:boolean;date_start:string|null;date_end:string|null;
 image_quality:'orig'|'large';video_quality:'highest'|'medium';request_pace:'standard'|'careful';concurrency:number;retry_limit:number;
}
export const defaultArchiveSettings = ():ArchiveSettings => ({target:'',include_timeline:false,media_types:{image:true,video:true,gif:true},include_retweets:true,date_start:null,date_end:null,image_quality:'orig',video_quality:'highest',request_pace:'standard',concurrency:2,retry_limit:3});
export function archiveAccount(settings:ArchiveSettings,mode:'full'|'incremental'|'retry'):Promise<ArchiveProgress> {
 return invoke('archive_account',{options:{...settings,mode,filter:'all'}});
}
export interface ArchivedAccount {
 user_id:string;screen_name:string;display_name:string;media:number;pending:number;failed:number;
 last_full_at:number|null;last_incremental_at:number|null;full_scope:string|null;summary:string;
 tweets:number;bytes:number;full_settings:ArchiveSettings|null;
}
export function readArchiveSettings():Promise<ArchiveSettings> {return invoke('get_archive_settings');}
export function saveArchiveSettings(settings:ArchiveSettings):Promise<void> {return invoke('save_archive_settings',{settings});}
export function listArchivedAccounts():Promise<ArchivedAccount[]> {return invoke('list_archived_accounts');}
export function saveProxy(proxy:string):Promise<void> {return invoke('save_proxy',{proxy});}
export function readNetworkRoute():Promise<string> {return invoke('get_network_route');}
export function scopeKey(s:ArchiveSettings):string {
 const m=s.media_types??{image:true,video:true,gif:true};
 const t=m.image&&m.video&&m.gif?'all':m.image&&!m.video&&!m.gif?'image':!m.image&&m.video&&m.gif?'video':`types${+m.image}${+m.video}${+m.gif}`;
 const range=s.include_retweets&&!s.date_start&&!s.date_end?t:`${t}:rt${+s.include_retweets}:${s.date_start??''}:${s.date_end??''}`;
 return `${range}:timeline${+s.include_timeline}`;
}
export function readArchiveStatus():Promise<ArchiveProgress> {return invoke('get_archive_status');}
export function stopArchive():Promise<void> {return invoke('stop_archive');}

export interface DiagnosticReport {items:{label:string;status:string;detail:string}[];rate_limits:ProbeResult['rate_limits'];error:PlatformError|null}
export function diagnoseConnection(target:string, includeTimeline:boolean):Promise<DiagnosticReport> {return invoke('diagnose_connection',{target,includeTimeline});}

export type DeleteScope='images'|'videos'|'gifs'|'text_tweets'|'all';
export interface AccountDataRow {scope:DeleteScope;sources:number;files:number;bytes:number}
export interface DeletePreview {sources:number;tweet_metadata:number;tweet_links:number;files:number;bytes:number;retained_files:number;token:string}
export interface DeleteResult {deleted:DeletePreview;backup:string;orphaned_files:string[]}
export interface IntegrityReport {missing_files:string[];changed_files:string[];untracked_files:string[];temporary_files:number}
export function addAccount(account:string):Promise<ArchiveSettings> {return invoke('add_account',{account});}
export function readAccountSettings(account:string):Promise<ArchiveSettings> {return invoke('get_account_settings',{account});}
export function readAccountData(account:string):Promise<AccountDataRow[]> {return invoke('get_account_data',{account});}
export function previewAccountDelete(account:string,scope:DeleteScope):Promise<DeletePreview> {return invoke('preview_account_delete',{account,scope});}
export function deleteAccountData(account:string,scope:DeleteScope,disableType:boolean,confirmation:string,previewToken:string):Promise<DeleteResult> {return invoke('delete_account_data',{account,scope,options:{disable_type:disableType,confirmation,preview_token:previewToken}});}
export function inspectAccount(account:string):Promise<IntegrityReport> {return invoke('inspect_account',{account});}
export function resetAccountProgress(account:string):Promise<void> {return invoke('reset_account_progress',{account});}
export function openMediaDirectory(account:string|null=null):Promise<void> {return invoke('open_media_directory',{account});}
export interface ExportOptions {
 account:string|null;mode:'full'|'incremental';media_types:{image:boolean;video:boolean;gif:boolean};
 date_start:string|null;date_end:string|null;output_directory:string|null;
}
export interface ExportProgress {
 running:boolean;stage:string;outcome:''|'completed'|'completed_with_issues'|'stopped'|'incomplete';
 account:string|null;current_item:string;total:number;done:number;copied:number;skipped:number;
 bytes:number;checked_bytes:number;started_at:number;finished_at:number|null;
 accounts:{account:string;directory:string;indexed:number}[];
 issues:{account:string;entity_id:number;path:string;reason:'modified'|'deleted'|'uncertain'|'conflict'|'source_missing'|'source_changed'|'io';message:string}[];
 error:string|null;logs:string[];
}
export function chooseExportDirectory():Promise<string|null> {return invoke('choose_export_directory');}
export function readExportDirectory():Promise<string|null> {return invoke('get_export_directory');}
export function openExportDirectory(directory:string):Promise<void> {return invoke('open_export_directory',{directory});}
export function startExport(options:ExportOptions):Promise<ExportProgress> {return invoke('start_export',{options});}
export function readExportStatus():Promise<ExportProgress> {return invoke('get_export_status');}
export function stopExport():Promise<void> {return invoke('stop_export');}
export function sameArchiveOptions(a:ArchiveSettings,b:ArchiveSettings):boolean {
 const normalize=(s:ArchiveSettings)=>({...s,target:'',media_types:s.media_types??{image:true,video:true,gif:true}});
 const first=normalize(a),second=normalize(b);
 return (Object.keys(first) as (keyof ArchiveSettings)[]).every(k=>JSON.stringify(first[k])===JSON.stringify(second[k]));
}
