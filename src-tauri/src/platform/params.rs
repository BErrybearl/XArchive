use super::error::{PlatformError, Result};
use crate::storage;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    path::Path,
};
use tempfile::NamedTempFile;
use url::Url;

pub const OPERATIONS: [&str; 4] = ["Viewer", "UserByScreenName", "UserMedia", "UserTweets"];
const MAX_PARAMS: u64 = 256 * 1024;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Params {
    pub version: u32,
    pub query_ids: BTreeMap<String, String>,
    pub public_bearer: String,
    pub features: BTreeMap<String, BTreeMap<String, bool>>,
    pub field_toggles: BTreeMap<String, BTreeMap<String, bool>>,
}

pub fn defaults() -> Params {
    serde_json::from_str(include_str!("defaults.json"))
        .expect("checked built-in platform parameters")
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}

fn validate(params: &Params) -> Result<()> {
    if params.version != 1
        || !OPERATIONS
            .iter()
            .all(|op| params.query_ids.get(*op).is_some_and(|id| valid_id(id)))
        || (!params.public_bearer.is_empty()
            && (params.public_bearer.len() > 4096
                || !params
                    .public_bearer
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"%_-".contains(&b))))
    {
        return Err(PlatformError::interface());
    }
    Ok(())
}

// Maps merge recursively, so a single feature/query override keeps all defaults.
fn merge(base: &mut Value, patch: Value) {
    if let (Some(target), Value::Object(source)) = (base.as_object_mut(), &patch) {
        for (key, value) in source {
            if let Some(old) = target.get_mut(key) {
                merge(old, value.clone());
            } else {
                target.insert(key.clone(), value.clone());
            }
        }
    } else {
        *base = patch;
    }
}

pub fn load(root: &Path) -> (Params, Vec<String>) {
    let fallback = defaults();
    let path = root.join("platform.json");
    let attempt = || -> Result<Params> {
        storage::check_path(&path).map_err(|_| PlatformError::local())?;
        let mut bytes = Vec::new();
        fs::File::open(&path)
            .map_err(|_| PlatformError::local())?
            .take(MAX_PARAMS + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| PlatformError::local())?;
        if bytes.len() as u64 > MAX_PARAMS {
            return Err(PlatformError::interface());
        }
        let patch: Value =
            serde_json::from_slice(&bytes).map_err(|_| PlatformError::interface())?;
        if !patch.is_object() {
            return Err(PlatformError::interface());
        }
        let mut base = serde_json::to_value(&fallback).map_err(|_| PlatformError::interface())?;
        merge(&mut base, patch);
        let params: Params =
            serde_json::from_value(base).map_err(|_| PlatformError::interface())?;
        validate(&params)?;
        Ok(params)
    };
    match attempt() {
        Ok(params) => (params, vec!["已读取 platform.json 覆盖参数".into()]),
        Err(_) => (
            fallback,
            vec![if path.exists() {
                "platform.json 损坏、无效或无法读取，使用内置参数；原文件未改动".into()
            } else {
                "platform.json 缺失，使用内置参数；未创建文件".into()
            }],
        ),
    }
}

pub fn commit_refresh(
    root: &Path,
    original: &Params,
    discovered: Result<Discovery>,
) -> Result<Params> {
    let discovered = discovered?; // Failed or incomplete discovery never touches disk.
    if !OPERATIONS.iter().all(|op| discovered.ids.contains_key(*op)) {
        return Err(PlatformError::interface());
    }
    let mut updated = original.clone();
    updated.query_ids.extend(discovered.ids);
    if let Some(bearer) = discovered.bearer {
        updated.public_bearer = bearer;
    }
    if updated.public_bearer.is_empty() {
        return Err(PlatformError::interface());
    }
    validate(&updated)?;
    let path = root.join("platform.json");
    storage::check_path(&path).map_err(|_| PlatformError::local())?;
    let bytes = serde_json::to_vec_pretty(&updated).map_err(|_| PlatformError::local())?;
    let mut file = NamedTempFile::new_in(root).map_err(|_| PlatformError::local())?;
    file.write_all(&bytes).map_err(|_| PlatformError::local())?;
    file.as_file()
        .sync_all()
        .map_err(|_| PlatformError::local())?;
    file.persist(path).map_err(|_| PlatformError::local())?;
    Ok(updated)
}

#[derive(Default)]
pub struct Discovery {
    pub ids: BTreeMap<String, String>,
    pub bearer: Option<String>,
}

impl Discovery {
    pub fn scan(&mut self, text: &str) {
        let forward = Regex::new(r#"(?:queryId|\"queryId\")\s*:\s*\"([A-Za-z0-9_-]+)\"\s*,\s*(?:operationName|\"operationName\")\s*:\s*\"(Viewer|UserByScreenName|UserMedia|UserTweets)\""#).expect("static regex");
        let reverse = Regex::new(r#"(?:operationName|\"operationName\")\s*:\s*\"(Viewer|UserByScreenName|UserMedia|UserTweets)\"\s*,\s*(?:queryId|\"queryId\")\s*:\s*\"([A-Za-z0-9_-]+)\""#).expect("static regex");
        for m in forward.captures_iter(text) {
            self.ids.entry(m[2].into()).or_insert_with(|| m[1].into());
        }
        for m in reverse.captures_iter(text) {
            self.ids.entry(m[1].into()).or_insert_with(|| m[2].into());
        }
        // Same heuristic as the preserved Python code, not proof of online validity.
        let bearer =
            Regex::new(r"(?:^|[^A-Za-z0-9])(AAAAA[A-Za-z0-9%]{40,})").expect("static regex");
        if self.bearer.is_none() {
            self.bearer = bearer.captures(text).map(|m| m[1].to_owned());
        }
    }
    pub fn complete(&self) -> bool {
        OPERATIONS.iter().all(|op| self.ids.contains_key(*op)) && self.bearer.is_some()
    }
}

pub fn script_urls(html: &str) -> Vec<Url> {
    let pattern = Regex::new(r#"https://abs\.twimg\.com/[^\s\"'<>]+\.js"#).expect("static regex");
    let mut urls = Vec::new();
    for m in pattern.find_iter(html) {
        let Ok(url) = Url::parse(m.as_str()) else {
            continue;
        };
        if url.origin().ascii_serialization() == "https://abs.twimg.com"
            && (url.path().starts_with("/responsive-web/") || url.path().starts_with("/x-web/"))
            && !urls.contains(&url)
        {
            urls.push(url);
        }
    }
    urls.sort_by_key(|url| !url.path().contains("/main."));
    urls.truncate(24);
    urls
}
