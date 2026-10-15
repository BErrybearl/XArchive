<script lang="ts">
  import { onMount, tick } from 'svelte';
  import ExportPanel from './lib/ExportPanel.svelte';
  import { accountName, accountToAdd, filterAccounts, bytes, progressForAccount, cancelledLoginState } from './lib/ui';
  import { applyTheme, readTheme, writeTheme, type ThemePreference } from './lib/theme';
  import { listen, type UnlistenFn } from '@tauri-apps/api/event';
  import { readProgramInfo, readLicenseNotices, type LicenseNotices, type ProgramInfo, readStorageStatus, initializeStorage, chooseDataDirectory,
    readLoginStatus, beginLogin, confirmLogin, cancelLogin, manualLogin, clearLoginBrowserProfile,
    refreshPlatformParams, probePlatform, resetTime, chooseLegacyFile, importLegacy, readLegacyStatus, stopLegacyImport,
    archiveAccount, readArchiveStatus, stopArchive, diagnoseConnection, readArchiveSettings, saveArchiveSettings,
    defaultArchiveSettings, listArchivedAccounts, saveProxy, readNetworkRoute, scopeKey,
    addAccount, readAccountSettings, readAccountData, previewAccountDelete, deleteAccountData,
    inspectAccount, resetAccountProgress, openMediaDirectory, sameArchiveOptions,
    chooseExportDirectory, readExportDirectory, openExportDirectory, startExport, readExportStatus, stopExport,
    type ExportProgress, type ExportOptions,
    readLayoutStatus, organizeMedia, stopLayout, type LayoutProgress, type StorageLayout,
    type AccountDataRow, type DeleteScope, type DeletePreview, type DeleteResult, type IntegrityReport,
    type StorageStatus, type LoginStatus, type ArchiveProgress, type ArchivedAccount,
    type ProbeResult, type LegacySelection, type LegacyReport, type LegacyProgress, type DiagnosticReport, type RefreshResult } from './lib/program';

  let native = $state(false);
  let programInfo = $state<ProgramInfo|null>(null);
  let licenses = $state<LicenseNotices|null>(null);
  let licenseLoading = $state(false);
  let licenseError = $state('');
  let licenseQuery = $state('');
  let licenseRevision = 0;
  let licenseText = $state<HTMLTextAreaElement>();
  const licenseChoices = $derived((licenses?.titles??[]).map((title,section)=>({title,section})).filter(item=>item.title.toLowerCase().includes(licenseQuery.trim().toLowerCase())));
  let ready = $state(false);
  let settings = $state(defaultArchiveSettings());
  let accounts = $state<ArchivedAccount[]>([]);
  let storage = $state<StorageStatus|null>(null);
  let login = $state<LoginStatus|null>(null);
  let proxy = $state('');
  let route = $state('系统代理');
  let networkError = $state('');
  let error = $state('');
  let dismissedError=$state(false);
  let notice = $state('');
  let busy = $state('');
  let archivePending = $state(false);
  let archivePreparing = $state(false);
  let progress = $state<ArchiveProgress|null>(null);
  let archiveRevision = 0;
  let listenersReady = $state(false);
  let runMode = $state<'full'|'incremental'|'retry'>('full');
  let accountInput = $state('');
  let tab = $state<'archive'|'data'|'export'>('archive');
  let overview = $state(true);
  let highlighted = $state<'login'|'network'|'data'|'export'|null>(null);
  let theme = $state<ThemePreference>(readTheme());
  let highlightTimer: ReturnType<typeof setTimeout> | undefined;
  let switching = $state(false);
  let accountData = $state<AccountDataRow[]>([]);
  let integrity = $state<IntegrityReport|null>(null);
  let deletion = $state<{account:string;scope:DeleteScope;preview:DeletePreview}|null>(null);
  let deleteResult = $state<DeleteResult|null>(null);
  let disableType = $state(true);
  let confirmation = $state('');
  let selectionVersion=0;
  let savedSnapshot='';
  let logsOpen = $state(false);
  let logPaused = $state(false);
  let logElement = $state<HTMLDivElement>();
  let now = $state(Date.now());
  let manual = $state(false);
  let authToken = $state('');
  let ct0 = $state('');
  let diagnostic = $state<DiagnosticReport|null>(null);
  let probe = $state<ProbeResult|null>(null);
  let refreshed = $state<RefreshResult|null>(null);
  let modal = $state<'about'|'import'|'failures'|'delete'|'reset'|'layout'|null>(null);
  let layoutChoice = $state<StorageLayout>('C');
  let layoutProgress = $state<LayoutProgress|null>(null);
  let layoutStarting = $state(false);
  let layoutRevision = 0;
  let layoutDirectory = $state('');
  let layoutFiles = $state(0);
  const layoutBusy = $derived(layoutStarting || !!layoutProgress?.running);
  let dialog = $state<HTMLDialogElement>();
  let returnFocus:HTMLElement|null = null;
  let selection = $state<LegacySelection|null>(null);
  let importProgress = $state<LegacyProgress|null>(null);
  let importReport = $state<LegacyReport|null>(null);
  let importBusy = $state(false);
  let importError = $state('');
  let exportProgress = $state<ExportProgress|null>(null);
  let exportStarting = $state(false);
  const filteredAccounts = $derived(filterAccounts(accounts, accountInput));
  const addCandidate = $derived(accountToAdd(accounts, accountInput));
  const guideStep = $derived(!ready ? 0 : !storage?.initialized ? 1 : !login?.saved ? 2 : !accounts.length ? 3 : 0);
  let exportMode = $state<ExportOptions['mode']>('full');
  let exportTypes = $state({image:true,video:true,gif:true});
  let exportDateStart = $state('');
  let exportDateEnd = $state('');
  let exportDirectory = $state<string|null>(null);
  let exportError = $state('');
  let exportNotice = $state('');
  let exportRevision = 0;
  let settingsQueue:Promise<void> = Promise.resolve();

  const exportBusy = $derived(exportStarting || !!exportProgress?.running);
  const archiveBusy = $derived(archivePending || !!progress?.running);
  const locked = $derived(!ready || archiveBusy || importBusy || exportBusy || layoutBusy || !!busy || switching);
  const selected = $derived(accounts.find(a => a.screen_name.toLowerCase() === targetName(settings.target).toLowerCase()));
  const visibleProgress = $derived(progressForAccount(progress, selected));
  const exportSelection = $derived(overview || !selected ? 'all' : 'current');
  const dataTaskReason = $derived(layoutBusy ? '整理文件正在进行，归档、导入、导出与删除暂不可用' : exportBusy ? '导出正在进行，归档、导入、删除与重置暂不可用' : archiveBusy ? '归档正在进行，导出、导入、删除与重置暂不可用' : importBusy ? '导入正在进行，归档、导出、删除与重置暂不可用' : busy ? `${busy}正在进行，请稍后操作` : '');
  const exportOptionsError = $derived(!Object.values(exportTypes).some(Boolean) ? '至少选择一种导出媒体类型' : exportDateStart&&exportDateEnd&&exportDateStart>exportDateEnd ? '起始日期不能晚于结束日期' : '');
  const exportDisabledReason = $derived(!native ? '请在桌面应用中导出' : !ready ? '正在读取程序状态' : !listenersReady ? '无法监听任务进度，请重新启动应用' : !storage?.initialized ? '请先创建数据目录' : login?.pending ? '请先完成登录' : archiveBusy||importBusy||layoutBusy||!!busy ? dataTaskReason : switching ? '正在切换账号' : exportSelection==='current'&&!selected?.user_id ? '请选择已归档的账号' : exportSelection==='all'&&!accounts.some(a=>a.user_id) ? '尚无已归档账号' : exportOptionsError || (!exportDirectory ? '请先选择输出目录' : ''));
  const mediaCount = $derived(Object.values(settings.media_types??{}).filter(Boolean).length);
  const optionsError = $derived(settings.date_start && settings.date_end && settings.date_start>settings.date_end ? '起始日期不能晚于结束日期' : mediaCount===0&&!settings.include_timeline ? '至少选择一种内容' : '');
  const disabledReason = $derived(!native ? '请在桌面应用中归档' : !ready ? '正在读取程序状态' : !listenersReady ? '无法监听任务进度，请重新启动应用' : !storage?.initialized ? '请先创建数据目录' : exportBusy||layoutBusy ? dataTaskReason : login?.pending ? '请先完成登录' : !login?.saved ? '请先在概览页登录 X' : !settings.target.trim() ? '请添加并选择账号' : optionsError || (busy || importBusy || switching ? '其他操作正在进行' : ''));
  const baselineWarning = $derived(selected?.full_settings ? !sameArchiveOptions(settings,selected.full_settings) : !!selected?.full_scope && selected.full_scope!==scopeKey(settings));
  const runningHere = $derived(archiveBusy && progress?.target?.screen_name.toLowerCase()===selected?.screen_name.toLowerCase());
  const runningElsewhere = $derived(archiveBusy&&!runningHere);
  const resetAt = $derived(visibleProgress?.error?.reset_at??null);
  const countdown = $derived(resetAt?Math.max(0,Math.ceil(resetAt-now/1000)):null);
  const elapsed = $derived(visibleProgress?.started_at ? Math.max(0,Math.floor(((visibleProgress.finished_at??now/1000)-visibleProgress.started_at))) : 0);
  const taskTitle = $derived(runningHere ? runMode==='retry'?'正在重试失败项':`正在${runMode==='full'?'全量':'增量'} · 第 ${(visibleProgress?.pages??0)+1} 页` : visibleProgress?.outcome==='rate_limit' ? countdown===0?'可以继续':`限流停止${countdown!==null?' · '+duration(countdown):''}` : visibleProgress?.outcome==='date_range' ? '按日期范围结束' : visibleProgress?.outcome==='stopped' ? '已停止' : ['completed','updated','retried'].includes(visibleProgress?.outcome??'') ? visibleProgress?.stage??'完成' : visibleProgress?.outcome ? '未完成' : '空闲');

  function targetName(raw:string) {
    const value=raw.trim();
    if(value.includes('://')) {try{return new URL(value).pathname.replace(/^\/+|\/+$/g,'');}catch{return value;}}
    return value.replace(/^@/,'');
  }
  function duration(n:number) {return n<60?`${n} 秒`:`${Math.floor(n/60)} 分 ${n%60} 秒`;}
  function date(t:number|null) {return t ? new Date(t*1000).toLocaleString('zh-CN',{month:'2-digit',day:'2-digit',hour:'2-digit',minute:'2-digit'}) : '—';}
  function message(e:unknown,fallback:string) {return typeof e==='string'?e:fallback;}
  async function showOverview(card: typeof highlighted = null) {
    ++selectionVersion;switching=false;
    overview=true;highlighted=card;notice='';rememberSelection('__overview__');
    clearTimeout(highlightTimer);
    if(card) {
      await tick();
      const element=document.getElementById(`overview-${card}`);
      element?.scrollIntoView({block:'nearest',inline:'nearest',behavior:'instant'});
      element?.focus({preventScroll:true});
      highlightTimer=setTimeout(()=>highlighted=null,2200);
    }
  }
  function focusAccountInput() {document.getElementById('add-account')?.focus();}
  function key(e:KeyboardEvent) {if(e.key==='Escape'&&!modal) {manual=false;authToken='';ct0='';}}
  function openModal(m:typeof modal, trigger=document.activeElement instanceof HTMLElement?document.activeElement:null) {returnFocus=trigger;modal=m;}
  async function loadLicense(section=0) {
    const revision=++licenseRevision;
    licenseLoading=true;licenseError='';
    try {
      const result=await readLicenseNotices(section);
      if(revision!==licenseRevision)return;
      licenses=result;
      await tick();
      if(licenseText)licenseText.scrollTop=0;
    }catch(e){if(revision===licenseRevision)licenseError=message(e,'无法读取许可声明');}
    finally{if(revision===licenseRevision)licenseLoading=false;}
  }
  function showAbout() {
    openModal('about');
    if(native&&!licenses&&!licenseLoading)void loadLicense();
  }
  async function closeModal() {
    modal=null;
    dialog?.close();
    await tick();
    const target=returnFocus;
    returnFocus=null;
    if(target?.isConnected&&!target.matches(':disabled'))target.focus();
    else document.getElementById('add-account')?.focus();
  }
  function logScroll() {if(logElement) logPaused=logElement.scrollHeight-logElement.clientHeight-logElement.scrollTop>24;}
  async function latest() {logPaused=false;await tick();if(logElement)logElement.scrollTop=logElement.scrollHeight;}
  async function refreshData() {
    const version=selectionVersion;
    const prior=selected;
    storage=await readStorageStatus();accounts=await listArchivedAccounts();
    if(version!==selectionVersion)return;
    const renamed=prior?.user_id?accounts.find(a=>a.user_id===prior.user_id):undefined;
    if(renamed&&renamed.screen_name!==prior?.screen_name) {
      const next=await readAccountSettings(renamed.screen_name);
      if(version!==selectionVersion)return;
      settings={...next,media_types:next.media_types??{image:true,video:true,gif:true}};
      savedSnapshot=JSON.stringify($state.snapshot(settings));rememberSelection(renamed.screen_name);
    }
    if(selected) {
      const data=await readAccountData(selected.screen_name);
      if(version===selectionVersion)accountData=data;
    }
  }
  function queueSettings(snapshot: typeof settings): Promise<void> {
    const value=JSON.stringify(snapshot);
    const version=selectionVersion;
    const save=settingsQueue.catch(()=>{}).then(()=>saveArchiveSettings(snapshot));
    settingsQueue=save.catch(()=>{});
    return save.then(()=>{if(version===selectionVersion)savedSnapshot=value;});
  }
  function rememberSelection(account:string) {try{localStorage.setItem('xarchive-selected-account',account);}catch{/* Keep navigation usable without storage. */}}
  function recalled(key:string) {try{return localStorage.getItem(key);}catch{return null;}}
  async function selectAccount(account:string) {
    const version=++selectionVersion;
    const old=$state.snapshot(settings);
    switching=true;error='';
    try {
      if(ready&&!archiveBusy&&!importBusy&&!exportBusy&&old.target&&JSON.stringify(old)!==savedSnapshot)await queueSettings(old);
      if(version!==selectionVersion)return;
      const [next,data]=await Promise.all([readAccountSettings(account),readAccountData(account)]);
      if(version!==selectionVersion)return;
      settings={...next,media_types:next.media_types??{image:true,video:true,gif:true}};overview=false;accountData=data;integrity=null;deleteResult=null;rememberSelection(account);
      savedSnapshot=JSON.stringify($state.snapshot(settings));
    }catch(e){if(version===selectionVersion)error=message(e,'无法读取账号工作区');}finally{if(version===selectionVersion)switching=false;}
  }
  async function submitAccount(e?:SubmitEvent) {
    e?.preventDefault();
    if(!native||!ready||switching||!!busy)return;
    const name=accountName(accountInput);
    const existing=accounts.find(a=>a.screen_name.toLowerCase()===name?.toLowerCase());
    if(existing){await selectAccount(existing.screen_name);accountInput='';return;}
    if(!storage?.initialized||locked||login?.pending||!addCandidate)return;
    const candidate=addCandidate;
    await perform('添加账号',async()=>{
      await settingsQueue;if(selected)await queueSettings($state.snapshot(settings));
      const next=await addAccount(candidate);accountInput='';accounts=await listArchivedAccounts();
      settings={...next,media_types:next.media_types??{image:true,video:true,gif:true}};
      overview=false;tab='archive';savedSnapshot=JSON.stringify($state.snapshot(settings));rememberSelection(next.target);
      accountData=await readAccountData(next.target);integrity=null;deleteResult=null;notice='账号已登记，点击全量归档开始保存。';
    });
  }
  function scopeLabel(scope:DeleteScope) {return {images:'图片',videos:'视频',gifs:'GIF',text_tweets:'纯文字推文',all:'整个账号'}[scope];}
  async function prepareDelete(scope:DeleteScope) {if(!selected)return;const account=selected.screen_name,trigger=document.activeElement instanceof HTMLElement?document.activeElement:null;await perform('删除预览',async()=>{await queueSettings($state.snapshot(settings));const preview=await previewAccountDelete(account,scope);deletion={account,scope,preview};disableType=true;confirmation='';openModal('delete',trigger);});}
  async function confirmDelete() {if(!deletion)return;const d=deletion;await perform('删除数据',async()=>{await settingsQueue;const result=await deleteAccountData(d.account,d.scope,disableType,confirmation,d.preview.token);closeModal();await refreshData();if(d.scope==='all'){settings=defaultArchiveSettings();overview=true;savedSnapshot='';rememberSelection('__overview__');if(accounts[0])await selectAccount(accounts[0].screen_name);accountData=selected?await readAccountData(selected.screen_name):[];}else{const next=await readAccountSettings(d.account);settings={...next,media_types:next.media_types??{image:true,video:true,gif:true}};savedSnapshot=JSON.stringify($state.snapshot(settings));}deleteResult=result;notice=result.orphaned_files.length?'索引已删除，部分文件未能删除，详情见下方。':'删除完成，索引备份已保存。';});}
  async function checkIntegrity() {if(!selected)return;const account=selected.screen_name;await perform('校验完整性',async()=>{integrity=await inspectAccount(account);});}
  async function confirmReset() {if(!selected)return;const account=selected.screen_name;await perform('重置进度',async()=>{await resetAccountProgress(account);closeModal();await refreshData();notice='全量检查点与完成标记已清除，下次从首页开始。';});}
  async function showMedia(account:string|null=selected?.screen_name??null) {await perform('打开媒体位置',async()=>{await openMediaDirectory(account);});}
  async function refreshLogin() {login=await readLoginStatus();proxy=login.proxy;await refreshRoute();}
  async function refreshRoute() {try {route=await readNetworkRoute();networkError='';}catch(e){networkError=message(e,'无法读取网络设置');route='系统代理';}}
  async function perform(name:string,action:()=>Promise<void>) {busy=name;error='';notice='';try{await action();}catch(e){error=message(e,`${name}未完成，请重试`);}finally{busy='';}}
  async function loginAction(action:'begin'|'confirm'|'cancel') {
    await perform('登录',async()=>{
      try {login=action==='begin'?await beginLogin(proxy):action==='confirm'?await confirmLogin():await cancelLogin();proxy=login.proxy;}
      catch(e){if(action==='cancel'&&login)login=cancelledLoginState(login,e);throw e;}
      finally{await refreshRoute();}
    });
  }
  async function submitManual(e:SubmitEvent) {e.preventDefault();const a=authToken,c=ct0;authToken='';ct0='';await perform('登录',async()=>{login=await manualLogin(a,c,proxy);manual=false;await refreshRoute();});}
  async function clearProfile() {await perform('清除资料',async()=>{await clearLoginBrowserProfile();await refreshLogin();notice='已清除浏览器资料';});}
  async function updateProxy() {await perform('保存代理',async()=>{await saveProxy(proxy);await refreshLogin();diagnostic=null;notice='代理已保存';});}
  async function diagnose() {await perform('连接诊断',async()=>{diagnostic=null;refreshed=null;diagnostic=await diagnoseConnection(settings.target,settings.include_timeline);});}
  async function probeInterface() {await perform('接口探测',async()=>{probe=await probePlatform(settings.target);});}
  async function refreshParams() {await perform('刷新接口',async()=>{refreshed=await refreshPlatformParams();});}
  async function createStorage() {await perform('创建目录',async()=>{storage=await initializeStorage();await refreshData();await refreshLogin();notice='数据目录已创建，下一步登录 X。';});}
  async function changeDirectory() {await perform('切换目录',async()=>{await settingsQueue;if(selected)await queueSettings($state.snapshot(settings));const s=await chooseDataDirectory();if(s){storage=s;layoutChoice=s.layout;layoutProgress=null;overview=true;settings=defaultArchiveSettings();accountData=[];savedSnapshot='';integrity=null;deleteResult=null;progress=null;diagnostic=null;refreshed=null;probe=null;rememberSelection('__overview__');if(s.initialized)await initializeStorage();accounts=await listArchivedAccounts();const saved=await readArchiveSettings();const initial=accounts.find(a=>a.screen_name.toLowerCase()===targetName(saved.target).toLowerCase())??accounts[0];if(initial)await selectAccount(initial.screen_name);await refreshLogin();await refreshData();}});}
  async function startArchive(mode:'full'|'incremental'|'retry') {
    if(!selected||archiveBusy||disabledReason||!ready||!listenersReady)return;
    const snapshot=$state.snapshot(settings),account=selected;
    archivePending=true;archivePreparing=true;
    runMode=mode;tab='archive';busy='准备归档';error='';notice='';dismissedError=false;logPaused=false;
    let started=false;
    try {await queueSettings(snapshot);busy='';started=true;progress={...emptyProgress(),running:true,target:{user_id:account.user_id,screen_name:account.screen_name,display_name:account.display_name}};progress=await archiveAccount(snapshot,mode);}
    catch(e){error=message(e,'归档未完成，已保存的数据保留');if(started&&progress)progress={...progress,running:false,outcome:'incomplete',stage:'归档未完成',summary:error};}
    finally{busy='';try{await refreshData();}catch(e){error ||= message(e,'无法刷新归档统计');}finally{archivePending=false;archivePreparing=false;}}
  }
  function emptyProgress():ArchiveProgress {return {running:false,started_at:Math.floor(Date.now()/1000),finished_at:null,stage:'准备归档',target:null,pages:0,tweets:0,page_item:0,page_total:0,current_item:'',bytes:0,added:0,reused:0,existing:0,date_ended:false,skipped:0,failed:0,pending:0,exhausted:false,outcome:'',summary:'',rate_limits:{},logs:[],failures:[],error:null};}
  async function requestStop() {try{await stopArchive();notice='正在停止，等待当前媒体保存';}catch(e){error=message(e,'停止请求未送达');}}
  async function selectLegacy() {await perform('选择旧库',async()=>{selection=await chooseLegacyFile();if(selection){importReport=null;importError='';}});}
  async function startLegacy() {
    if(!selection)return;const s=selection;selection=null;importBusy=true;importError='';importReport=null;
    importProgress={running:true,stage:'准备导入',done:0,total:0};
    try{importReport=await importLegacy(s);}catch(e){importError=message(e,'导入未完成，已提交的数据保留');}
    finally{
      try{const r=await readLegacyStatus();importProgress=r.progress;}catch{/* Keep the original result. */}
      try{await refreshData();}catch(e){importError ||= message(e,'无法刷新导入统计');}
      finally{importBusy=false;}
    }
  }
  async function stopLegacy() {try{await stopLegacyImport();}catch(e){importError=message(e,'停止请求未送达');}}
  function exportAccountName(id:string|null) {return accounts.find(a=>a.user_id===id)?.screen_name??id??'';}
  function openAccountExport() {tab='export';}
  async function selectExportDirectory() {
    exportError='';exportNotice='';
    await perform('选择导出目录',async()=>{const path=await chooseExportDirectory();if(path)exportDirectory=path;});
  }
  async function beginExport() {
    if(exportBusy||exportDisabledReason||!listenersReady)return;
    const options:ExportOptions={account:exportSelection==='all'?null:selected?.user_id??null,mode:exportMode,media_types:$state.snapshot(exportTypes),date_start:exportDateStart||null,date_end:exportDateEnd||null,output_directory:exportDirectory};
    exportStarting=true;exportError='';exportNotice='';
    try {
      await settingsQueue;
      if(selected&&JSON.stringify($state.snapshot(settings))!==savedSnapshot) {await queueSettings($state.snapshot(settings));}
      const revision=exportRevision;
      const initial=await startExport(options);
      if(exportRevision===revision)exportProgress=initial;
      const statusRevision=exportRevision;
      const status=await readExportStatus();
      if(exportRevision===statusRevision)exportProgress=status;
    }catch(e){exportError=message(e,'导出未完成，请重试');}
    finally{exportStarting=false;}
  }
  async function requestExportStop() {try{await stopExport();exportNotice='正在停止，等待当前文件与索引保存';}catch(e){exportError=message(e,'停止请求未送达');}}
  async function confirmLayout() {
    if(locked||!listenersReady||!storage?.initialized)return;
    await perform('读取整理信息',async()=>{
      await settingsQueue;storage=await readStorageStatus();
      if(!storage?.initialized)throw '数据目录尚未创建';
      layoutDirectory=storage.path;layoutFiles=storage.media;openModal('layout');
    });
  }
  async function beginLayout() {
    if(locked||!layoutDirectory)return;
    await closeModal();layoutStarting=true;error='';
    try {
      const revision=layoutRevision;
      const result=await organizeMedia(layoutChoice,layoutDirectory);
      if(layoutRevision===revision)layoutProgress=result;
      const statusRevision=layoutRevision;const status=await readLayoutStatus();
      if(layoutRevision===statusRevision)layoutProgress=status;
      await refreshData();layoutChoice=storage?.layout??'C';
    }catch(e){error=message(e,'整理未完成，请重新整理文件');}
    finally{layoutStarting=false;}
  }
  async function requestLayoutStop() {try{await stopLayout();notice='正在停止，等待当前文件与索引保存';}catch(e){error=message(e,'停止请求未送达');}}
  async function showExportDirectory(directory:string) {exportError='';try{await openExportDirectory(directory);}catch(e){exportError=message(e,'无法打开导出目录');}}

  $effect(()=>{applyTheme(theme);writeTheme(theme);const media=window.matchMedia('(prefers-color-scheme: dark)');const update=()=>applyTheme(theme);media.addEventListener('change',update);return()=>media.removeEventListener('change',update);});
  $effect(()=>{const snapshot=$state.snapshot(settings);if(!ready||!native||!selected||switching||locked||settings.date_start&&settings.date_end&&settings.date_start>settings.date_end)return;const timer=setTimeout(()=>{void queueSettings(snapshot).catch(e=>{error=message(e,'无法保存账号选项');});},350);return()=>clearTimeout(timer);});
  $effect(()=>{if(!dialog)return;if(modal&&!dialog.open)dialog.showModal();else if(!modal&&dialog.open)dialog.close();});
  $effect(()=>{
    if(!importBusy)return;
    let active=true;
    let timer:ReturnType<typeof setTimeout>;
    const poll=async()=>{
      try{const r=await readLegacyStatus();if(active&&importBusy)importProgress=r.progress;}
      catch{/* The result command remains authoritative. */}
      if(active)timer=setTimeout(poll,500);
    };
    timer=setTimeout(poll,500);
    return()=>{active=false;clearTimeout(timer);};
  });
  $effect(()=>{const count=visibleProgress?.logs.length;const last=visibleProgress?.logs.at(-1);if(logsOpen&&!logPaused&&count&&last){void tick().then(()=>{if(logElement)logElement.scrollTop=logElement.scrollHeight;});}});
  onMount(()=>{
    let active=true;let unlisten:UnlistenFn|undefined;let unlistenExport:UnlistenFn|undefined;let unlistenLayout:UnlistenFn|undefined;const timer=setInterval(()=>now=Date.now(),1000);
    void (async()=>{try {
      programInfo=await readProgramInfo();native=!!programInfo;if(!native){ready=true;return;}
      const stop=await listen<ArchiveProgress>('archive-progress',e=>{if(active){archiveRevision++;progress=e.payload;if(e.payload.running)archivePreparing=false;}});if(active)unlisten=stop;else {stop();return;}
      const stopExportListener=await listen<ExportProgress>('export-progress',e=>{if(active){exportRevision++;exportProgress=e.payload;if(!e.payload.running)exportNotice='';}});if(active)unlistenExport=stopExportListener;else {stopExportListener();return;}
      const stopLayoutListener=await listen<LayoutProgress>('layout-progress',e=>{if(active){layoutRevision++;layoutProgress=e.payload;}});if(active)unlistenLayout=stopLayoutListener;else {stopLayoutListener();return;}
      const layoutStatusRevision=layoutRevision;const layoutStatus=await readLayoutStatus();if(active&&layoutRevision===layoutStatusRevision&&layoutStatus.stage)layoutProgress=layoutStatus;
      listenersReady=true;
      const archiveStatusRevision=archiveRevision;
      const p=await readArchiveStatus();if(active&&archiveRevision===archiveStatusRevision&&p.stage)progress=p;
      const revision=exportRevision;
      const exportStatus=await readExportStatus();if(active&&exportRevision===revision&&exportStatus.stage)exportProgress=exportStatus;
      const saved=await readArchiveSettings();if(!active)return;settings={...saved,target:saved.target||recalled('xarchive-target')||'',media_types:saved.media_types??{image:true,video:true,gif:true}};
      storage=await readStorageStatus();layoutChoice=storage?.layout??'C';if(storage?.initialized)await initializeStorage();
      accounts=await listArchivedAccounts();
      const remembered=recalled('xarchive-selected-account')||saved.target;
      const initial=accounts.find(a=>a.screen_name.toLowerCase()===targetName(remembered).toLowerCase())??accounts.find(a=>a.screen_name.toLowerCase()===targetName(saved.target).toLowerCase())??accounts[0];
      if(initial)await selectAccount(initial.screen_name);else settings.target='';
      if(remembered==='__overview__')overview=true;
      await refreshData();await refreshLogin();
      const output=await readExportDirectory();if(active)exportDirectory=output;
    }catch(e){error=message(e,'无法读取程序状态');}finally{if(active)ready=true;}})();
    return()=>{active=false;unlisten?.();unlistenExport?.();unlistenLayout?.();clearInterval(timer);clearTimeout(highlightTimer);};
  });
</script>

<svelte:head><title>XArchive</title></svelte:head>
<svelte:window onkeydown={key}/>

<main class="workspace" lang="zh-CN">
  <header class="topbar">
    <h1>XArchive<span class="wordmark-dot">.</span></h1>
    <nav aria-label="应用状态与概览导航">
      <button class="capsule" title="查看登录卡片" onclick={()=>showOverview('login')}><span class="dot" class:success={login?.saved} class:warning={login?.pending}></span>{login?.pending?'登录中':login?.account?'@'+login.account.screen_name:'未登录'}</button>
      <button class="capsule" title="查看网络卡片" onclick={()=>showOverview('network')}><span class="dot" class:danger={!!networkError||!!diagnostic?.error}></span>{route}</button>
      <button class="capsule" title="查看本机数据卡片" onclick={()=>showOverview('data')}><span class="dot" class:success={storage?.initialized}></span>{storage?.initialized?bytes(storage.bytes):'未创建数据'}</button>
      <button class="capsule" title="查看导出进度" onclick={()=>showOverview('export')}><span class="dot" class:warning={exportBusy} class:success={exportProgress?.outcome==='completed'} class:danger={exportProgress?.outcome==='incomplete'}></span>{exportBusy?`导出 ${exportProgress?.done??0}/${exportProgress?.total??0}`:'导出'}</button>
      <div class="theme-switch" aria-label="主题选择">
        {#each [['light','浅色'],['dark','深色'],['system','跟随系统']] as [value,label]}
          <button class:active={theme===value} aria-pressed={theme===value} title={value==='system'?'跟随系统主题':`${label}主题`} onclick={()=>theme=value as ThemePreference}>{label}</button>
        {/each}
      </div>
      <button class="capsule" onclick={showAbout}>关于</button>
    </nav>
  </header>
  {#if error||(visibleProgress?.error&&!dismissedError)}
    <div class="banner" role="alert"><span>{error||((visibleProgress?.error?.message??'')+' '+(visibleProgress?.error?.suggestion??''))}</span><button class="icon-button" aria-label="关闭错误提示" onclick={()=>{error='';dismissedError=true;}}>×</button></div>
  {/if}
  <div class="workbench">
    <aside class="account-column" aria-label="账号列表">
      <form class="add-account" onsubmit={submitAccount}>
        <label for="add-account">搜索或添加账号 {#if guideStep===3}<span class="step-badge">第 3 步</span>{/if}</label>
        <input id="add-account" bind:value={accountInput} placeholder="@用户名、显示名或主页链接" disabled={!ready} autocomplete="off" spellcheck="false"/>
        <p class="help">输入检索 · 回车添加或选择</p>{#if guideStep===3}<p class="help">添加第一个账号，开始归档。</p>{/if}
      </form>
      <button class="overview-entry" class:selected={overview||!selected} aria-pressed={overview||!selected} onclick={()=>showOverview()}><svg viewBox="0 0 20 20" aria-hidden="true"><rect x="3" y="3" width="5" height="5" rx="1"/><rect x="12" y="3" width="5" height="5" rx="1"/><rect x="3" y="12" width="5" height="5" rx="1"/><rect x="12" y="12" width="5" height="5" rx="1"/></svg><span>概览</span></button>
      <div class="account-list">
        {#if addCandidate}<button class="account-row add-candidate" disabled={!native||!storage?.initialized||locked||login?.pending} onclick={()=>submitAccount()}><span class="avatar">+</span><span class="account-label"><strong>添加 @{addCandidate}</strong><small>{!storage?.initialized?'请先创建数据目录':login?.pending?'请先完成或取消登录':locked?'请等待当前操作完成':'登记并选中 · 不会自动归档'}</small></span></button>{/if}
        {#each filteredAccounts as a (a.screen_name)}
          <button class="account-row" class:selected={!overview&&selected?.screen_name===a.screen_name} class:running={archiveBusy&&progress?.target?.screen_name.toLowerCase()===a.screen_name.toLowerCase()} disabled={!ready||switching||!!busy} onclick={()=>selectAccount(a.screen_name)}>
            <span class="avatar">{a.screen_name.slice(0,1).toUpperCase()}</span>
            <span class="account-label"><strong>@{a.screen_name}</strong><small>{a.last_full_at||a.last_incremental_at||a.tweets?`${a.media.toLocaleString('zh-CN')} 份媒体`:'未归档'}</small></span>
            {#if archiveBusy&&progress?.target?.screen_name.toLowerCase()===a.screen_name.toLowerCase()}<span class="dot terracotta" title="正在归档"></span>{:else if a.pending+a.failed>0}<span class="dot warning" title={`待重试 ${a.pending} · 已失败 ${a.failed}`}></span>{/if}
          </button>
        {:else}<p class="help account-empty">{accountInput?'没有匹配的账号。':'添加第一个账号，开始建立本机归档。'}</p>{/each}
      </div>
      <p class="column-footer">{accounts.length} 个账号 · 本机保存</p>
    </aside>
    <section class="account-workspace" class:overview-workspace={overview||!selected} aria-label="所选账号工作区" aria-busy={switching}>
      {#if selected&&!overview}
        <header class="account-header">
          <div class="account-title"><h2>@{selected.screen_name}</h2><span class="muted">{selected.display_name}</span></div>
          <div class="workspace-tabs" aria-label="工作区页签"><button class:active={tab==='archive'} aria-pressed={tab==='archive'} onclick={()=>tab='archive'}>归档</button><button class:active={tab==='data'} aria-pressed={tab==='data'} onclick={()=>tab='data'}>数据</button><button class:active={tab==='export'} aria-pressed={tab==='export'} onclick={openAccountExport}>导出</button></div>
          <div class="account-overview"><span>推文 <strong>{selected.tweets}</strong></span><span>媒体 <strong>{selected.media}</strong></span><span>待重试 <strong>{selected.pending}</strong>{#if selected.failed}<small> · {selected.failed} 失败</small>{/if}</span><span>占用 <strong>{bytes(selected.bytes)}</strong></span><span title={selected.last_full_at?resetTime(selected.last_full_at):'—'}>上次全量 <strong>{date(selected.last_full_at)}</strong></span><span title={selected.last_incremental_at?resetTime(selected.last_incremental_at):'—'}>上次增量 <strong>{date(selected.last_incremental_at)}</strong></span></div>
        </header>
        <div class="workspace-scroll">
          {#if tab==='archive'}
            <div class="option-groups">
              <section class="option-group" aria-label="内容选项"><h3>内容</h3>
                {#each [['image','图片'],['video','视频'],['gif','GIF']] as [kind,label]}
                  {@const k=kind as 'image'|'video'|'gif'}
                  <label class="check-row"><input type="checkbox" bind:checked={settings.media_types![k]} disabled={locked||switching}/><span>{label}</span></label>
                {/each}
                <label class="check-row"><input type="checkbox" bind:checked={settings.include_retweets} disabled={locked||switching}/><span>转推中的媒体</span></label>
                <label class="check-row"><input type="checkbox" bind:checked={settings.include_timeline} disabled={locked||switching}/><span>纯文字推文</span></label>
              </section>
              <section class="option-group" aria-label="范围与质量"><h3>范围与质量</h3>
                <label class="small-label" for="date-start">日期范围 <span class="muted">UTC，可选</span></label>
                <div class="date-pair"><input aria-label="起始日期" id="date-start" type="date" value={settings.date_start??''} onchange={e=>settings.date_start=e.currentTarget.value||null} disabled={locked||switching}/><span>至</span><input aria-label="结束日期" type="date" value={settings.date_end??''} onchange={e=>settings.date_end=e.currentTarget.value||null} disabled={locked||switching}/></div>
                <label for="image-quality">图片质量</label><select id="image-quality" bind:value={settings.image_quality} disabled={locked||switching}><option value="orig">原图</option><option value="large">大图</option></select>
                <label for="video-quality">视频质量</label><select id="video-quality" bind:value={settings.video_quality} disabled={locked||switching}><option value="highest">最高码率</option><option value="medium">中等码率</option></select>
              </section>
              <section class="option-group" aria-label="节奏"><h3>节奏</h3>
                <label for="request-pace">请求节奏</label><select id="request-pace" bind:value={settings.request_pace} disabled={locked||switching}><option value="standard">标准</option><option value="careful">稳妥（间隔加倍）</option></select>
                <label for="concurrency">下载并发</label><select id="concurrency" bind:value={settings.concurrency} disabled={locked||switching}>{#each [1,2,3] as n}<option value={n}>{n}</option>{/each}</select>
                <label for="retry-limit">重试上限</label><select id="retry-limit" bind:value={settings.retry_limit} disabled={locked||switching}>{#each [1,2,3,4,5] as n}<option value={n}>{n}</option>{/each}</select>
              </section>
            </div>
            <p class="options-note">选项按账号记住 · 已有来源保留原质量 · 重试上限按累计失败次数计</p>
            <div class="workspace-actions">
              {#if runningHere}<button class="danger-button" disabled={archivePreparing||!progress?.running} onclick={requestStop}>停止归档</button>{:else}<button disabled={archiveBusy||!!disabledReason||!ready} onclick={()=>startArchive('incremental')}>增量归档</button><button class="primary" disabled={archiveBusy||!!disabledReason||!ready} onclick={()=>startArchive('full')}>全量归档</button>{/if}
              {#if baselineWarning}<p class="baseline-hint">与上次全量设置不同，建议先全量补足</p>{/if}
            </div>
            {#if runningElsewhere}<p class="help">@{progress?.target?.screen_name??'其他账号'} 正在归档 <button class="text-button" onclick={()=>{if(progress?.target)void selectAccount(progress.target.screen_name);}}>查看</button></p>{:else if !archiveBusy&&disabledReason}<p class="help">{disabledReason}{#if !login?.saved&&native}<button class="text-button" onclick={()=>showOverview('login')}>登录</button>{/if}</p>{/if}
      <section class="task-card panel" aria-label="当前任务">
        <div class="section-heading"><div class="task-heading"><span class="dot" class:success={taskTitle==='完成'} class:warning={runningHere||visibleProgress?.outcome==='rate_limit'} class:danger={taskTitle==='未完成'}></span><h2>{visibleProgress?taskTitle:selected?.summary?'上次结果':'尚无任务'}</h2>{#if visibleProgress?.target}<span class="muted">@{visibleProgress.target.screen_name}</span>{/if}</div><button class="text-button" aria-expanded={logsOpen} onclick={()=>logsOpen=!logsOpen}>{logsOpen?'收起日志':'展开日志'}<span>{logsOpen?'⌃':'⌄'}</span></button></div>
        <div class="task-progress" class:indeterminate={runningHere&&!visibleProgress?.page_total}><div style:width={`${visibleProgress?.page_total?Math.min(100,visibleProgress.page_item/visibleProgress.page_total*100):taskTitle==='完成'?100:0}%`}></div></div>
        <div class="task-metrics"><span>新增 <strong>{visibleProgress?.added??0}</strong></span><span>复用 <strong>{(visibleProgress?.reused??0)+(visibleProgress?.existing??0)}</strong><span class="metric-note"> · 跳过 {(visibleProgress?.skipped??0)-(visibleProgress?.existing??0)}</span></span><button class="metric-button" disabled={!visibleProgress?.failures.length} onclick={()=>openModal('failures')}>失败 / 待重试 <strong class:failure-count={!!visibleProgress?.failures.length}>{visibleProgress?.failed??0} / {visibleProgress?.pending??0}</strong></button><span class="task-timing">{duration(elapsed)}<span>·</span>{bytes(elapsed?(visibleProgress?.bytes??0)/elapsed:0)}/秒</span></div>
        <p class="current-item" title={visibleProgress?.summary??selected?.summary??''}>{runningHere ? visibleProgress?.current_item||visibleProgress?.stage||'准备归档…' : visibleProgress?.summary||selected?.summary||'选择一个账号，开始保存媒体与来源。'}</p>
        {#if visibleProgress?.finished_at}<p class="task-end">结束于 {resetTime(visibleProgress.finished_at)} · {duration(elapsed)}</p>{/if}
        {#if visibleProgress?.outcome==='rate_limit'}<p class="rate-notice">{countdown===0?'可以继续，请手动启动归档':resetAt?'预计 '+resetTime(resetAt)+' 可继续':'平台未提供恢复时间，请稍后手动尝试'}</p>{/if}
        {#if logsOpen}
          <div class="log-frame"><div class="log-area" bind:this={logElement} onscroll={logScroll} role="log" aria-label="最近 200 条任务日志">
            {#each visibleProgress?.logs??[] as line}<div class="log-line"><time>{line.slice(0,8)}</time><span class="dot" class:danger={line.includes('失败')} class:success={line.includes('下载')||line.includes('复用')} class:warning={line.includes('跳过')}></span><span>{line.slice(9)}</span></div>{:else}<p class="muted">任务开始后显示实时日志。</p>{/each}
          </div>{#if logPaused}<button class="latest-button" onclick={latest}>回到最新 ↓</button>{/if}</div>
        {/if}
      </section>
          {:else if tab==='export'}
            <ExportPanel scopeLabel={exportSelection==='all'?'全部已归档账号':`@${selected?.screen_name??''}`}
              bind:exportMode bind:exportTypes bind:exportDateStart bind:exportDateEnd
              {exportDirectory} {exportProgress} {exportBusy} {exportStarting} {exportDisabledReason} {exportNotice} {exportError}
              {native} {locked} loginPending={!!login?.pending} {busy} {exportAccountName} {selectExportDirectory}
              {beginExport} {requestExportStop} {showExportDirectory}/>
          {:else}
            <table class="data-table"><thead><tr><th>类型</th><th>条数（来源）</th><th>唯一文件</th><th>占用空间</th><th><span class="sr-only">操作</span></th></tr></thead><tbody>
              {#each accountData as row (row.scope)}<tr><td>{scopeLabel(row.scope)}</td><td>{row.sources.toLocaleString('zh-CN')}</td><td>{row.scope==='text_tweets'?'—':row.files.toLocaleString('zh-CN')}</td><td>{row.scope==='text_tweets'?'—':bytes(row.bytes)}</td><td><button class="danger-button" disabled={locked||switching||row.sources===0} onclick={()=>prepareDelete(row.scope)}>删除</button></td></tr>{/each}
            </tbody></table>
            <p class="help">空间按相关唯一媒体文件计算；共享文件可能出现在多个类型或账号中。纯文字条数不含有媒体来源或待下载媒体的推文。</p>
            {#if dataTaskReason}<p class="inline-warning">{dataTaskReason}</p>{/if}
            <div class="maintenance"><h3>维护</h3><div class="maintenance-actions">
              <button disabled={!native||locked||switching||!selected.user_id} onclick={openAccountExport}>导出此账号</button>
              <button disabled={!native||locked||switching||selected.pending===0} onclick={()=>startArchive('retry')}>重试失败项 <span class="muted">{selected.pending}</span></button>
              <button disabled={!native||locked||switching} onclick={checkIntegrity}>{busy==='校验完整性'?'正在检查…':'校验完整性'}</button>
              <button disabled={!native||locked||switching} onclick={()=>openModal('reset')}>重置全量进度</button>
              <button disabled={!native||!!busy} onclick={()=>showMedia()}>打开媒体位置</button>
            </div><p class="help">重试只处理待重试项，不扫描时间线；校验只报告本账号相关文件缺失或变化。</p></div>
            {#if integrity}<div class="integrity-report" role="status"><p>校验完成 · 缺失 {integrity.missing_files.length} · 变化 {integrity.changed_files.length}</p>{#each integrity.missing_files as path}<p class="path-label">缺失：{path}</p>{/each}{#each integrity.changed_files as path}<p class="path-label">变化：{path}</p>{/each}<p class="help">未修复或删除任何文件。</p></div>{/if}
            {#if deleteResult}<div class="delete-report" role="status"><p>已删除 {deleteResult.deleted.sources} 条来源、{deleteResult.deleted.tweet_metadata} 条推文元数据、{deleteResult.deleted.files} 个文件；保留 {deleteResult.deleted.retained_files} 个仍有引用的文件。</p><p class="path-label">索引备份：{deleteResult.backup}</p>{#if deleteResult.orphaned_files.length}<p class="inline-warning">索引已删除，以下文件删除失败，作为孤立文件保留：</p>{#each deleteResult.orphaned_files as path}<p class="path-label">{path}</p>{/each}{/if}</div>{/if}
            <div class="danger-zone"><div><h3>移除整个账号</h3><p class="help">移除本账号登记、选项、归档记录和无剩余引用的文件。</p></div><button class="danger-button" disabled={locked||switching} onclick={()=>prepareDelete('all')}>移除整个账号</button></div>
          {/if}
          {#if notice}<p class="workspace-notice" role="status">{notice}</p>{/if}
        </div>
      {:else}
        <header class="overview-header"><h2>概览</h2>{#if guideStep}<p class="help">创建数据目录 → 登录 → 添加账号</p>{:else}<p class="help">本机归档、连接与导出</p>{/if}</header>
        <div class="overview-grid">
          <section id="overview-data" class="overview-card" class:highlighted={highlighted==='data'} class:first-step={guideStep===1} tabindex="-1" aria-label="本机数据">
            <div class="section-heading"><h2>本机数据</h2>{#if guideStep===1}<span class="step-badge">第 1 步</span>{/if}</div>
            <div class="card-content"><label for="data-directory">数据目录</label><input id="data-directory" readonly value={storage?.path??'正在读取…'} title={storage?.path??''}/>
            <p class="help">状态：{storage?.initialized?'数据目录已就绪':'尚未创建数据目录'}</p>
            {#if !storage?.initialized}<p class="help">将在上方位置创建本机归档数据。创建后再登录 X。</p><button class="create-directory" disabled={!native||!ready||locked} onclick={createStorage}>{busy==='创建目录'?'正在创建…':'创建数据目录'}</button>{/if}
            <div class="actions"><button disabled={!native||!ready||locked||login?.pending} onclick={changeDirectory}>切换目录</button><button disabled={!native||!storage?.initialized||archiveBusy||exportBusy||layoutBusy||!!busy||login?.pending} onclick={()=>openModal('import')}>{importBusy?'查看导入进度':'导入旧库'}</button></div>
            <p class="help">切换目录不会搬移已有数据。</p>
            <p class="help">当前结构：{(storage?.layout??'C')==='C'?'C · 按账号再分类型':'A · 按账号与月份'}</p>
            <label for="storage-layout">存储结构</label><select id="storage-layout" bind:value={layoutChoice} disabled={!native||locked||!storage?.initialized||login?.pending}>
              <option value="C">C · 按账号再分类型（默认）</option><option value="A">A · 按账号与月份</option>
            </select>
            <p class="path-label">C：media/@账号/图片|视频|GIF/2026-10-15_推文ID_1.jpg</p>
            <p class="path-label">A：media/@账号/2026-10/2026-10-15_推文ID_1.jpg</p>
            <div class="actions"><button disabled={!native||locked||!listenersReady||!storage?.initialized||login?.pending} onclick={confirmLayout}>{layoutChoice===storage?.layout?'整理文件':'应用存储结构'}</button><button disabled={!native||!storage?.initialized||layoutBusy||!!busy} onclick={()=>showMedia(null)}>打开媒体文件夹</button></div>
            {#if layoutBusy}<button onclick={requestLayoutStop}>停止整理</button>{/if}
            {#if layoutProgress}<p class="help" role="status">{layoutProgress.stage} · {layoutProgress.done} / {layoutProgress.total} · 已改名 {layoutProgress.renamed} 个</p><progress aria-label="整理文件进度" value={layoutProgress.done} max={layoutProgress.total||1}></progress>{#if layoutProgress.running}<p class="path-label">{layoutProgress.current}</p>{/if}{#if layoutProgress.backup}<p class="path-label">索引备份：{layoutProgress.backup}</p>{/if}{#if layoutProgress.stopped}<p class="help">已完成的改名保留。再次整理可继续统一存储结构。</p>{/if}{#if layoutProgress.error}<p class="inline-warning" role="alert">{layoutProgress.error}</p>{/if}{/if}
            <div class="data-stats"><div><span>推文</span><strong>{storage?.tweets??'—'}</strong></div><div><span>唯一媒体</span><strong>{storage?.media??'—'}</strong></div><div><span>来源</span><strong>{storage?.sources??'—'}</strong></div><div><span>待重试</span><strong>{storage?.pending??'—'}</strong></div></div>
            <p class="help">媒体与索引占用 {bytes(storage?.bytes??0)}。备份时保留整个数据目录。</p>
            {#if dataTaskReason}<p class="inline-warning">{dataTaskReason}</p>{/if}
            {#if deleteResult}<p class="help" role="status">最近删除结果：已删除 {deleteResult.deleted.sources} 条来源，保留 {deleteResult.deleted.retained_files} 个共享文件。</p><p class="path-label">索引备份：{deleteResult.backup}</p>{#each deleteResult.orphaned_files as path}<p class="path-label">未能删除，孤立文件：{path}</p>{/each}{/if}</div>
          </section>
          <section id="overview-login" class="overview-card" class:highlighted={highlighted==='login'} tabindex="-1" aria-label="登录">
            <div class="section-heading"><h2>登录</h2>{#if guideStep===2}<span class="step-badge">第 2 步</span>{/if}</div>
            <div class="card-content">{#if !storage?.initialized}<p class="inline-warning">请先创建数据目录</p><button disabled={!native||!ready||locked} onclick={()=>showOverview('data')}>前往创建数据目录</button>{:else if login?.pending}
              <p class="help">在浏览器完成登录并关闭窗口，然后确认。</p>
              <div class="actions"><button disabled={locked} onclick={()=>loginAction('confirm')}>{busy==='登录'?'正在核对…':'已完成登录'}</button><button disabled={locked} onclick={()=>loginAction('cancel')}>取消</button></div>
            {:else}
              <p class="help">{login?.account?'已保存 @'+login.account.screen_name+' 的登录状态':'在独立浏览器中登录 X'}</p>
              <button disabled={!native||!storage?.initialized||locked} onclick={()=>loginAction('begin')}>{busy==='登录'?'正在处理…':login?.saved?'重新登录':'登录 X'}</button>
            {/if}
            <p class="help">验证时间：{login?.verified_at?resetTime(login.verified_at):'尚未验证'}</p>
            <details class="probe-details other-login"><summary>其他登录方式</summary>
            <button class="text-button disclosure-toggle" aria-expanded={manual} aria-controls="manual-login" disabled={!storage?.initialized||locked||login?.pending} onclick={()=>{manual=!manual;if(!manual){authToken='';ct0='';}}}>{manual?'收起手动凭据':'手动凭据'}</button>
              <div id="manual-login">
            {#if manual}
              <form onsubmit={submitManual} class="manual-form">
                <p class="help">从自己的浏览器 Cookies 复制凭据；核对后保存到本机。</p>
                <label for="auth-token">auth_token</label><input id="auth-token" type="password" bind:value={authToken} autocomplete="off" maxlength="4096" disabled={locked}/>
                <label for="ct0">ct0</label><input id="ct0" type="password" bind:value={ct0} autocomplete="off" maxlength="4096" disabled={locked}/>
                <button type="submit" disabled={!native||!storage?.initialized||locked||!authToken||!ct0}>核对并保存</button>
              </form>
            {/if}
              </div>
            <button class="text-button subtle" disabled={!native||!storage?.initialized||locked} onclick={clearProfile}>清除登录浏览器资料</button>
            <p class="help">请先关闭登录浏览器。应用已保存的登录状态保留。</p>
            </details>
            {#if login?.cleanup_warning}<p class="inline-warning">{login.cleanup_warning}</p>{/if}
            {#if guideStep===3}<div class="next-step"><span class="step-badge">第 3 步</span><p class="help">登录已完成，在左侧添加账号即可开始归档。</p><button onclick={focusAccountInput}>添加账号</button></div>{/if}</div>
          </section>
          <section id="overview-network" class="overview-card" class:highlighted={highlighted==='network'} tabindex="-1" aria-label="网络与连接">
            <div class="section-heading"><h2>网络与连接</h2></div>
            <div class="card-content"><p class="help">当前路线：{route}</p>
            <div class="actions"><button disabled={!native||!storage?.initialized||locked||login?.pending} onclick={updateProxy}>保存代理</button><button disabled={!native||!storage?.initialized||locked||login?.pending} onclick={diagnose}>{busy==='连接诊断'?'正在诊断…':'连接诊断'}</button></div>
<label for="proxy">代理地址</label><input id="proxy" bind:value={proxy} placeholder="http://127.0.0.1:端口" autocomplete="off" spellcheck="false" disabled={locked||login?.pending}/>
            <p class="help">留空跟随系统代理，系统未启用时直连。</p>
            <p class="help">诊断同时检查登录、目标账号与接口首页。</p>
            {#if networkError}<p class="inline-warning">{networkError}</p>{/if}
            {#if diagnostic}
              {#if diagnostic.error}<p class="inline-warning" role="alert">{diagnostic.error.message} {diagnostic.error.suggestion}</p>{/if}
              <details class="probe-details"><summary>诊断明细 · {diagnostic.items.filter(item=>item.status==='通过').length} / {diagnostic.items.length} 通过</summary>
              <div class="diagnostic-results" aria-live="polite">
                {#each diagnostic.items as item}<div><span class="dot" class:success={item.status==='通过'} class:danger={item.status==='未通过'} class:warning={item.status==='限流'}></span><div><strong>{item.label}<span>{item.status}</span></strong><p class="help">{item.detail}</p></div></div>{/each}
                {#each Object.entries(diagnostic.rate_limits) as [op,rate]}<p class="help">{op} · 剩余 {rate.remaining??'未知'} · 恢复 {resetTime(rate.reset_at)}</p>{/each}
              </div>
              </details>
            {/if}
            {#if diagnostic?.error||diagnostic?.items.some(item=>item.status==='未通过')}<button class="text-button" disabled={!native||!storage?.initialized||locked||login?.pending} onclick={refreshParams}>{busy==='刷新接口'?'正在刷新…':'刷新接口参数'}</button>{/if}
            {#if refreshed}<p class="help">{refreshed.ok?'接口参数已刷新':refreshed.error?.message??'刷新未完成'}</p>{/if}
            <details class="probe-details"><summary>独立接口探测</summary><p class="help">使用当前选中账号：{selected?'@'+selected.screen_name:'请先添加并选择账号'}。</p><button disabled={!native||!storage?.initialized||!selected||locked||login?.pending} onclick={probeInterface}>{busy==='接口探测'?'正在探测…':'探测接口首页'}</button>
              {#if probe}<p class="help" role="status">{probe.stage} · {probe.ok?'通过':'未通过'}</p>{#if probe.stats}<p class="help">推文 {probe.stats.tweets} · 图片 {probe.stats.images} · 视频 {probe.stats.videos} · GIF {probe.stats.gifs}</p>{/if}{#each probe.notices as item}<p class="help">{item}</p>{/each}{#if probe.error}<p class="inline-warning">{probe.error.message} {probe.error.suggestion}</p>{/if}{/if}
            </details></div>
          </section>
          <section id="overview-export" class="overview-card" class:highlighted={highlighted==='export'} tabindex="-1" aria-label="导出全部账号">
            <div class="section-heading"><h2>导出全部账号</h2></div>
            <div class="card-content"><ExportPanel compact scopeLabel={exportSelection==='all'?'全部已归档账号':`@${selected?.screen_name??''}`}
              bind:exportMode bind:exportTypes bind:exportDateStart bind:exportDateEnd
              {exportDirectory} {exportProgress} {exportBusy} {exportStarting} {exportDisabledReason} {exportNotice} {exportError}
              {native} {locked} loginPending={!!login?.pending} {busy} {exportAccountName} {selectExportDirectory}
              {beginExport} {requestExportStop} {showExportDirectory}/>
</div>
          </section>
        </div>
        {#if notice}<p class="overview-notice" role="status">{notice}</p>{/if}
      {/if}
    </section>
  </div>
</main>

<dialog bind:this={dialog} oncancel={(e)=>{e.preventDefault();void closeModal();}} onclose={()=>{if(modal)void closeModal();}} aria-label={modal==='layout'?'存储结构':modal==='about'?'关于 XArchive':modal==='import'?'导入旧库':modal==='delete'?'删除账号数据':modal==='reset'?'重置全量进度':'失败与待重试清单'}>
  <div class="dialog-heading"><h2>{modal==='layout'?'确认整理媒体文件':modal==='about'?'关于 XArchive':modal==='import'?'导入旧版数据':modal==='delete'?'删除账号数据':modal==='reset'?'重置全量进度':'失败与待重试'}</h2><button class="icon-button" onclick={closeModal} aria-label="关闭对话框">×</button></div>
  <div class="dialog-content">
    {#if modal==='about'}
      <h3>{programInfo?.name??'XArchive'} <span class="muted">{programInfo?`v${programInfo.version}`:'网页预览'}</span></h3>
      <p class="help">非官方工具，与 X Corp. 无关；仅供个人归档，使用风险自担。</p>
      <h3>开源许可</h3>
      <p class="help">项目许可与第三方声明已内嵌，可按章节查看完整原文。</p>
      {#if licenses}
        <label for="license-search">搜索章节</label><input id="license-search" type="search" bind:value={licenseQuery} placeholder="包名或许可名称"/>
        <label for="license-section">许可章节</label>
        <select id="license-section" value={licenses.section} disabled={licenseLoading} onchange={(e)=>loadLicense(Number(e.currentTarget.value))}>
          {#if !licenseChoices.some(item=>item.section===licenses?.section)}<option value={licenses.section}>{licenses.titles[licenses.section]}</option>{/if}
          {#each licenseChoices as item}<option value={item.section}>{item.title}</option>{/each}
        </select>
        {#if licenseQuery&&!licenseChoices.length}<p class="help">没有匹配的章节。</p>{/if}
        <div class="actions"><button disabled={licenseLoading||licenses.section===0} onclick={()=>loadLicense(licenses!.section-1)}>上一节</button><span class="muted">{licenses.section+1} / {licenses.titles.length}</span><button disabled={licenseLoading||licenses.section===licenses.titles.length-1} onclick={()=>loadLicense(licenses!.section+1)}>下一节</button></div>
        <textarea class="license-text" bind:this={licenseText} readonly value={licenses.text} aria-label="许可全文" aria-busy={licenseLoading}></textarea>
      {/if}
      {#if licenseLoading}<p class="help" role="status">正在读取许可声明…</p>{/if}
      {#if licenseError}<p class="inline-warning" role="alert">{licenseError}</p><button disabled={licenseLoading} onclick={()=>loadLicense(licenses?.section??0)}>重试</button>{/if}
      {#if !native}<p class="help">请在桌面应用中查看内嵌许可声明。</p>{/if}
    {:else if modal==='layout'}
      <p class="help">将按{layoutChoice==='C'?'C · 按账号再分类型':'A · 按账号与月份'}整理：将在本磁盘批量改名，不复制文件，约 {layoutFiles} 个文件。</p>
      <p class="path-label">数据目录：{layoutDirectory}</p><p class="help">执行前在线备份索引。只处理已登记媒体；账号名使用当前名称。可停止，已完成的改名保留，再次整理可继续。</p>
      <div class="actions"><button disabled={locked} onclick={beginLayout}>确认整理</button><button onclick={closeModal}>取消</button></div>
    {:else if modal==='delete'&&deletion}
      <p class="help">@{deletion.account} · {scopeLabel(deletion.scope)}。执行前会在线备份索引；删除后不自动恢复文件。</p>
      <dl class="delete-preview"><div><dt>删除来源记录</dt><dd>{deletion.preview.sources}</dd></div><div><dt>删除推文元数据</dt><dd>{deletion.preview.tweet_metadata}</dd></div><div><dt>解除本账号推文关联</dt><dd>{deletion.preview.tweet_links}</dd></div><div><dt>删除文件</dt><dd>{deletion.preview.files} 个 · {bytes(deletion.preview.bytes)}</dd></div><div><dt>其他来源仍引用，保留文件</dt><dd>{deletion.preview.retained_files} 个</dd></div></dl>
      {#if deletion.scope==='all'}<label for="confirm-account">输入账号名 {deletion.account} 确认移除</label><input id="confirm-account" bind:value={confirmation} disabled={!!busy} autocomplete="off" spellcheck="false"/>
      {:else}<p class="help">本账号全量完成标记将失效，下次需全量补回；删除媒体类型保留推文元数据。</p><label class="check-row"><input type="checkbox" bind:checked={disableType} disabled={!!busy}/><span>同时在此账号的归档选项中取消该类型</span></label>{/if}
      <div class="actions"><button class="danger-button" disabled={locked||deletion.scope==='all'&&confirmation.trim().replace(/^@/,'').toLowerCase()!==deletion.account.toLowerCase()} onclick={confirmDelete}>确认{deletion.scope==='all'?'移除':'删除'}</button><button disabled={!!busy} onclick={closeModal}>取消</button></div>
    {:else if modal==='reset'}<p class="help">清除 @{selected?.screen_name} 的全量检查点与完成标记。推文和媒体保留，下次全量从首页检查。</p><div class="actions"><button class="danger-button" disabled={locked} onclick={confirmReset}>确认重置</button><button onclick={closeModal}>取消</button></div>
    {:else if modal==='failures'}
      <p class="help">每份媒体每次任务尝试一次；下次归档自动处理允许范围内的待重试项。</p>
      {#each visibleProgress?.failures??[] as f}<div class="failure-item"><strong>推文 {f.tweet_id} · {f.kind}</strong><span class="muted">累计失败 {f.attempts} 次 · {f.terminal?'已达上限':'待重试'}</span><p>{f.reason}</p></div>{/each}
    {:else if modal==='import'}
      <p class="help">选择旧库的隔离副本。导入前请关闭旧版程序；核对完成后显示报告。</p>
      {#if dataTaskReason}<p class="inline-warning">{dataTaskReason}</p>{/if}
      <button disabled={!native||locked||login?.pending} onclick={selectLegacy}>{busy==='选择旧库'?'正在选择…':'选择旧库文件'}</button>
      {#if selection}<p class="path-label">旧库：{selection.path}</p><p class="path-label">导入到：{selection.destination}</p><div class="actions"><button disabled={locked} onclick={startLegacy}>确认导入</button><button onclick={()=>selection=null}>取消</button></div>{/if}
      {#if importBusy}<p role="status">{importProgress?.stage} · {importProgress?.done??0} / {importProgress?.total??0}</p><progress value={importProgress?.done??0} max={importProgress?.total||1}></progress><button onclick={stopLegacy}>停止导入</button>{/if}
      {#if importError}<p class="inline-warning" role="alert">{importError}</p>{/if}
      {#if importReport}
        <h3 class:failure-count={!importReport.passed}>核对{importReport.passed?'通过':'未通过'}{importReport.stopped?' · 已停止':''}</h3>
        <div class="report-counts"><p>推文 <strong>{importReport.counts.imported_tweets} / {importReport.counts.old_tweets}</strong></p><p>唯一媒体 <strong>{importReport.counts.imported_media} / {importReport.counts.expected_media}</strong></p><p>来源 <strong>{importReport.counts.imported_sources} / {importReport.counts.old_sources}</strong></p><p>摘要比较 <strong>{importReport.counts.digests_checked}</strong></p></div>
        <p class="help">旧库物理 BLOB：{importReport.counts.old_media_rows} · 新库媒体按账号核对</p>
        {#if importReport.error}<p class="inline-warning">{importReport.error}</p>{/if}
        {#each importReport.notices as n}<p class="help">{n}</p>{/each}
        {#if importReport.missing.length||importReport.mismatches.length}<h3>缺失与不一致</h3>{#each [...importReport.missing,...importReport.mismatches] as issue}<p class="help">{issue}</p>{/each}{/if}
        {#if importReport.pending.length}<h3>旧版待重试队列</h3>{#each importReport.pending as pending}<p>{pending.key}</p><pre>{JSON.stringify(pending.items,null,2)}</pre>{/each}{/if}
        <p class="path-label">完整报告：{importReport.report_path}</p><p class="path-label">只读副本：{importReport.snapshot}</p>
      {/if}
    {/if}
  </div>
</dialog>
