<script lang="ts">
  import { resetTime, type ExportOptions, type ExportProgress } from './program';
  import { bytes } from './ui';

  interface Props {
    compact?: boolean;
    scopeLabel: string;
    exportMode: ExportOptions['mode'];
    exportTypes: ExportOptions['media_types'];
    exportDateStart: string;
    exportDateEnd: string;
    exportDirectory: string | null;
    exportProgress: ExportProgress | null;
    exportBusy: boolean;
    exportStarting: boolean;
    exportDisabledReason: string;
    exportNotice: string;
    exportError: string;
    native: boolean;
    locked: boolean;
    loginPending: boolean;
    busy: string;
    exportAccountName: (id: string | null) => string;
    selectExportDirectory: () => Promise<void>;
    beginExport: () => Promise<void>;
    requestExportStop: () => Promise<void>;
    showExportDirectory: (directory: string) => Promise<void>;
  }
  let { compact = false, scopeLabel, exportMode = $bindable(), exportTypes = $bindable(),
    exportDateStart = $bindable(), exportDateEnd = $bindable(), exportDirectory,
    exportProgress, exportBusy, exportStarting, exportDisabledReason, exportNotice, exportError,
    native, locked, loginPending, busy, exportAccountName, selectExportDirectory,
    beginExport, requestExportStop, showExportDirectory }: Props = $props();
</script>

{#snippet options()}
<div class="export-fields">
              <div><span class="field-label">账号</span><p class="export-scope">{scopeLabel}</p></div>
              <div><label for="export-mode">模式</label><select id="export-mode" bind:value={exportMode} disabled={locked}><option value="full">全量 · 新建目录</option><option value="incremental">增量 · 补充上次目录</option></select></div>
            </div>
            <fieldset class="export-types" disabled={locked}><legend>媒体类型</legend>
              {#each [['image','图片'],['video','视频'],['gif','GIF']] as [kind,label]}
                {@const k=kind as 'image'|'video'|'gif'}
                <label class="check-row"><input type="checkbox" bind:checked={exportTypes[k]}/><span>{label}</span></label>
              {/each}
            </fieldset>
            <label for="export-date-start">日期范围 <span class="muted">UTC，可选</span></label>
            <div class="date-pair"><input id="export-date-start" type="date" aria-label="导出起始日期" bind:value={exportDateStart} disabled={locked}/><span>至</span><input type="date" aria-label="导出结束日期" bind:value={exportDateEnd} disabled={locked}/></div>
{/snippet}
{#snippet output()}
            <div class="export-output"><label for="export-directory">输出目录</label><div><input id="export-directory" value={exportDirectory??''} readonly placeholder="选择后记住" title={exportDirectory??''}/><button disabled={!native||locked||loginPending} onclick={selectExportDirectory}>选择</button></div></div>

            {#if !compact}
            <p class="help">导出目录须在数据目录之外。增量保留旧文件；用户改动或删除的文件不会覆盖或恢复。换输出目录请用全量。</p>
            {/if}
            <div class="actions">{#if exportBusy}<button class="danger-button" disabled={exportStarting} onclick={requestExportStop}>停止导出</button>{:else}<button disabled={!!exportDisabledReason} onclick={beginExport}>开始导出</button>{/if}{#if exportDirectory}<button disabled={exportBusy||!!busy} onclick={()=>showExportDirectory(exportDirectory!)}>打开输出目录</button>{/if}</div>
            {#if !exportBusy&&exportDisabledReason}<p class="inline-warning">{exportDisabledReason}</p>{/if}
{/snippet}
<div class="export-panel">
  <p class="help">可选：复制到其他位置或按条件挑选。媒体在本机数据目录中即可直接浏览。</p>
  {#if compact}
    {@render output()}
    <details class="export-options"><summary>导出高级选项 · {exportMode==='full'?'全量':'增量'} · {Object.values(exportTypes).filter(Boolean).length} 类媒体 · {exportDateStart||exportDateEnd ? `${exportDateStart||'不限'} 至 ${exportDateEnd||'不限'}` : '不限日期'}</summary>
      {@render options()}
            <p class="help">导出目录须在数据目录之外。增量保留旧文件；用户改动或删除的文件不会覆盖或恢复。换输出目录请用全量。</p>
    </details>
  {:else}
    {@render options()}
    {@render output()}
  {/if}
            {#if exportNotice}<p class="help" role="status">{exportNotice}</p>{/if}
            {#if exportError||exportProgress?.error}<p class="inline-warning" role="alert">{exportError||exportProgress?.error}</p>{/if}
            {#if exportProgress}
              <div class="export-result" role="status" aria-live="polite">
                <h3>{exportProgress.stage}{#if exportProgress.running&&exportProgress.account}<span class="muted"> · @{exportAccountName(exportProgress.account)}</span>{/if}</h3>
                <progress aria-label="导出处理进度" value={exportProgress.done} max={exportProgress.total||1}></progress>
                <p class="export-metrics">已处理 {exportProgress.done} / {exportProgress.total} · 导出 {exportProgress.copied} · 跳过 {exportProgress.skipped}</p>
                <p class="help">已复制 {bytes(exportProgress.bytes)} · 已核对 {bytes(exportProgress.checked_bytes)}{#if exportProgress.finished_at} · {resetTime(exportProgress.finished_at)}{/if}</p>
                {#if exportProgress.running}<p class="path-label">{exportProgress.current_item||'准备导出…'}</p>{/if}
                {#each exportProgress.accounts as result (result.directory)}<div class="export-account-result"><p>@{exportAccountName(result.account)} · 索引 {result.indexed} 个文件</p><p class="path-label">{result.directory}</p><button class="text-button" disabled={exportBusy} onclick={()=>showExportDirectory(result.directory)}>打开此输出目录</button></div>{/each}
              </div>
              {#if exportProgress.issues.length}
                <details class="export-issues" open={!exportProgress.running}><summary>保留或跳过的文件 · {exportProgress.issues.length}</summary>
                  {#each exportProgress.issues as issue}<div><p>@{exportAccountName(issue.account)} · {issue.message}</p><p class="path-label">{issue.path}</p></div>{/each}
                </details>
              {/if}
            {/if}
</div>
