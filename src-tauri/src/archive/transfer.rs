use super::*;
use std::{
    io::{Read, Write},
    sync::mpsc,
};
use tempfile::NamedTempFile;

fn label(item: &Retry) -> &'static str {
    match item.media.kind.as_str() {
        "image" => "图片",
        "video" => "视频",
        _ => "GIF",
    }
}
enum TransferError {
    Media(download::DownloadError),
    Local,
}
impl From<download::DownloadError> for TransferError {
    fn from(e: download::DownloadError) -> Self {
        Self::Media(e)
    }
}
fn stage(
    mut input: Box<dyn Read>,
    root: &Path,
    mut bytes: impl FnMut(u64),
) -> std::result::Result<NamedTempFile, TransferError> {
    let mut f = NamedTempFile::new_in(root.join("temporary")).map_err(|_| TransferError::Local)?;
    let mut buf = [0u8; 64 * 1024];
    let mut total = 0;
    loop {
        let n = input
            .read(&mut buf)
            .map_err(|_| download::DownloadError::EmptyOrTruncated)?;
        if n == 0 {
            break;
        }
        f.write_all(&buf[..n]).map_err(|_| TransferError::Local)?;
        total += n as u64;
        bytes(n as u64);
    }
    if total == 0 {
        return Err(download::DownloadError::EmptyOrTruncated.into());
    }
    Ok(f)
}
fn commit(
    store: &mut Store,
    target: &Account,
    item: &Retry,
    result: std::result::Result<NamedTempFile, TransferError>,
    options: &Options,
    p: &mut Progress,
    notify: &mut impl FnMut(Progress),
) -> Result<()> {
    p.page_item += 1;
    p.current_item = format!(
        "{} · 推文 {} · {}/{}",
        label(item),
        item.tweet.id,
        p.page_item,
        p.page_total
    );
    match result {
        Ok(f) => {
            let length = f
                .as_file()
                .metadata()
                .map_err(|_| PlatformError::local())?
                .len();
            let source = MediaSource {
                target: user(target),
                author: user(&item.tweet.author),
                tweet_id: &item.tweet.id,
                text: Some(&item.tweet.text),
                published_at: item.tweet.published_at.as_deref(),
                media_key: &item.media.key,
                original_filename: &item.media.filename,
                original_url: &item.media.url,
                media_type: &item.media.kind,
                mime_type: &item.media.mime,
            };
            let saved = store
                .save_media(&source, f.reopen().map_err(|_| PlatformError::local())?)
                .map_err(|_| PlatformError::local())?;
            clear_retry(store, &target.user_id, item)?;
            if saved.reused {
                p.reused += 1;
                media_log(p, "复用 已有媒体", false, notify);
            } else {
                p.added += 1;
                media_log(
                    p,
                    format!(
                        "下载 {} {}/{} · {} KB",
                        label(item),
                        p.page_item,
                        p.page_total,
                        length.div_ceil(1024)
                    ),
                    false,
                    notify,
                );
            }
        }
        Err(TransferError::Local) => {
            return Err(PlatformError::local());
        }
        Err(TransferError::Media(error)) => {
            fail_retry(
                store,
                &target.user_id,
                item,
                error.message(),
                options.preferences.retry_limit,
            )?;
            refresh_failures(store, target, options, p)?;
            let terminal = retry_attempts(store, &target.user_id, item)?
                .is_some_and(|n| n >= options.preferences.retry_limit);
            media_log(
                p,
                format!(
                    "失败 · {}（{}）",
                    error.message(),
                    if terminal {
                        "已达上限"
                    } else {
                        "将重试"
                    }
                ),
                true,
                notify,
            );
        }
    }
    refresh_failures(store, target, options, p)?;
    Ok(())
}
enum Event {
    Bytes(u64),
    Done(usize, std::result::Result<NamedTempFile, TransferError>),
}
// Workers receive only media URLs and a scratch directory. No API session or SQLite connection.
#[allow(clippy::too_many_arguments)]
pub(super) fn download_batch(
    store: &mut Store,
    target: &Account,
    items: Vec<Retry>,
    options: &Options,
    cancel: &AtomicBool,
    backend: &mut impl Backend,
    p: &mut Progress,
    notify: &mut impl FnMut(Progress),
) -> Result<()> {
    let mut ready = Vec::new();
    for item in items {
        if source_exists(store, &target.user_id, &item, p)? {
            clear_retry(store, &target.user_id, &item)?;
            p.skipped += 1;
            p.existing += 1;
            p.page_item += 1;
            media_log(p, "复用 已有媒体", false, notify);
        } else if retry_attempts(store, &target.user_id, &item)?
            .is_some_and(|n| n >= options.preferences.retry_limit)
        {
            p.page_item += 1;
            media_log(p, "跳过 已达重试上限", false, notify);
        } else {
            ready.push(item);
        }
    }
    let root = store.root().to_owned();
    storage::check_path(&root.join("temporary")).map_err(|_| PlatformError::local())?;
    if backend.worker().is_none() {
        for item in ready {
            if stopped(cancel) {
                break;
            }
            p.stage = "下载与入库".into();
            p.current_item = format!("{} · 推文 {}", label(&item), item.tweet.id);
            push(p, false, notify);
            let result = backend
                .media(&item.media)
                .map_err(TransferError::from)
                .and_then(|input| {
                    stage(input, &root, |n| {
                        p.bytes += n;
                        push(p, false, notify);
                    })
                });
            if let Err(e) = commit(store, target, &item, result, options, p, notify) {
                media_log(p, "失败 · 无法保存本机媒体", true, notify);
                return Err(e);
            }
        }
        return Ok(());
    }
    std::thread::scope(|scope| {
        let (tx, rx) = mpsc::channel();
        let mut next = 0;
        let mut active = 0;
        let mut fatal = None;
        loop {
            while next < ready.len()
                && active < options.preferences.concurrency
                && !stopped(cancel)
                && fatal.is_none()
            {
                let index = next;
                next += 1;
                active += 1;
                let mut worker = backend.worker().ok_or_else(PlatformError::local)?;
                let media = ready[index].media.clone();
                let root = root.clone();
                let tx = tx.clone();
                p.stage = "下载与入库".into();
                p.current_item =
                    format!("{} · 推文 {}", label(&ready[index]), ready[index].tweet.id);
                push(p, false, notify);
                scope.spawn(move || {
                    let mut pending = 0;
                    let mut last = Instant::now();
                    let result = worker
                        .url(&media.url)
                        .and_then(|url| worker.open(url, reqwest::header::HeaderMap::new()))
                        .map_err(TransferError::from)
                        .and_then(|input| {
                            stage(input, &root, |n| {
                                pending += n;
                                if last.elapsed() >= Duration::from_millis(50) {
                                    let _ = tx.send(Event::Bytes(pending));
                                    pending = 0;
                                    last = Instant::now();
                                }
                            })
                        });
                    if pending > 0 {
                        let _ = tx.send(Event::Bytes(pending));
                    }
                    let _ = tx.send(Event::Done(index, result));
                });
            }
            if active == 0 {
                break;
            }
            match rx.recv().map_err(|_| PlatformError::local())? {
                Event::Bytes(n) => {
                    p.bytes += n;
                    push(p, false, notify);
                }
                Event::Done(index, result) => {
                    active -= 1;
                    if let Err(e) = commit(store, target, &ready[index], result, options, p, notify)
                    {
                        media_log(p, "失败 · 无法保存本机媒体", true, notify);
                        fatal = Some(e);
                    }
                }
            }
        }
        if let Some(e) = fatal { Err(e) } else { Ok(()) }
    })
}
