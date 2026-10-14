use super::*;
use std::{fs, io::Write};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct MediaTypes {
    pub image: bool,
    pub video: bool,
    pub gif: bool,
}
impl Default for MediaTypes {
    fn default() -> Self {
        Self {
            image: true,
            video: true,
            gif: true,
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct Preferences {
    pub media_types: Option<MediaTypes>,
    pub include_retweets: bool,
    pub date_start: Option<String>,
    pub date_end: Option<String>,
    pub image_quality: String,
    pub video_quality: String,
    pub request_pace: String,
    pub concurrency: usize,
    pub retry_limit: u32,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            media_types: None,
            include_retweets: true,
            date_start: None,
            date_end: None,
            image_quality: "orig".into(),
            video_quality: "highest".into(),
            request_pace: "standard".into(),
            concurrency: 2,
            retry_limit: 3,
        }
    }
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Settings {
    pub target: String,
    pub include_timeline: bool,
    #[serde(flatten)]
    pub preferences: Preferences,
}
// Dates refer to UTC calendar days. End is inclusive in the UI.
pub fn date_seconds(raw: &str) -> Option<u64> {
    let p: Vec<_> = raw.split('-').collect();
    if p.len() != 3 || p[0].len() != 4 || p[1].len() != 2 || p[2].len() != 2 {
        return None;
    }
    let y: u32 = p[0].parse().ok()?;
    let m: usize = p[1].parse().ok()?;
    let d: u32 = p[2].parse().ok()?;
    let leap = y.is_multiple_of(4) && (!y.is_multiple_of(100) || y.is_multiple_of(400));
    let days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if !(1970..=9999).contains(&y) || !(1..=12).contains(&m) || d == 0 || d > days[m - 1] {
        return None;
    }
    let mut n = 0u64;
    for year in 1970..y {
        n += if year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400)) {
            366
        } else {
            365
        };
    }
    n += days[..m - 1].iter().map(|d| *d as u64).sum::<u64>() + d as u64 - 1;
    Some(n * 86400)
}
impl Preferences {
    pub fn validate(&self) -> std::result::Result<(), String> {
        if self
            .media_types
            .as_ref()
            .is_some_and(|m| !m.image && !m.video && !m.gif)
        {
            return Err("至少选择一种媒体类型".into());
        }
        self.validate_fields()
    }
    pub fn validate_fields(&self) -> std::result::Result<(), String> {
        if !(1..=3).contains(&self.concurrency)
            || !(1..=5).contains(&self.retry_limit)
            || !["orig", "large"].contains(&self.image_quality.as_str())
            || !["highest", "medium"].contains(&self.video_quality.as_str())
            || !["standard", "careful"].contains(&self.request_pace.as_str())
        {
            return Err("归档选项无效".into());
        }
        for raw in [&self.date_start, &self.date_end].into_iter().flatten() {
            if date_seconds(raw).is_none() {
                return Err("日期格式无效".into());
            }
        }
        if self
            .date_start
            .as_ref()
            .zip(self.date_end.as_ref())
            .is_some_and(|(s, e)| s > e)
        {
            return Err("起始日期不能晚于结束日期".into());
        }
        Ok(())
    }
    pub fn request_delay(&self) -> Duration {
        Duration::from_secs(if self.request_pace == "careful" { 2 } else { 1 })
    }
    pub fn within_dates(&self, tweet: &ArchivedTweet) -> bool {
        if self.date_start.is_none() && self.date_end.is_none() {
            return true;
        }
        tweet.timestamp.is_some_and(|t| {
            self.date_start
                .as_deref()
                .and_then(date_seconds)
                .is_none_or(|s| t >= s)
                && self
                    .date_end
                    .as_deref()
                    .and_then(date_seconds)
                    .is_none_or(|e| t < e + 86400)
        })
    }
    pub fn select_quality(&self, media: &Attachment) -> Attachment {
        let mut m = media.clone();
        if m.kind == "image" {
            if let Ok(mut u) = url::Url::parse(&m.url) {
                let pairs: Vec<_> = u
                    .query_pairs()
                    .filter(|(k, _)| k != "name")
                    .map(|(k, v)| (k.into_owned(), v.into_owned()))
                    .collect();
                u.set_query(None);
                u.query_pairs_mut()
                    .extend_pairs(pairs)
                    .append_pair("name", &self.image_quality);
                m.url = u.into();
            }
        } else if !m.variants.is_empty() {
            let mut variants = m.variants.clone();
            variants.sort_by_key(|v| v.0);
            variants.dedup_by_key(|v| v.0);
            let index = if self.video_quality == "medium" && variants.len() >= 3 {
                (variants.len() - 1) / 2
            } else {
                variants.len() - 1
            };
            m.url = variants[index].1.clone();
            if let Ok(u) = url::Url::parse(&m.url) {
                if let Some(f) = u.path_segments().and_then(|mut s| s.next_back()) {
                    m.filename = f.into();
                }
            }
        }
        m
    }
}
impl Options {
    pub fn validate(&self) -> std::result::Result<(), String> {
        self.preferences.validate_fields()?;
        if self.mode != Mode::Retry
            && !self.include_timeline
            && self
                .preferences
                .media_types
                .as_ref()
                .is_some_and(|m| !m.image && !m.video && !m.gif)
        {
            return Err("至少选择一种内容".into());
        }
        Ok(())
    }
    pub fn accepts(&self, kind: &str) -> bool {
        self.preferences.media_types.as_ref().map_or_else(
            || self.filter.accepts(kind),
            |m| match kind {
                "image" => m.image,
                "video" => m.video,
                "gif" => m.gif,
                _ => false,
            },
        )
    }
    pub fn scope_key(&self) -> String {
        let p = &self.preferences;
        let types = if let Some(m) = &p.media_types {
            match (m.image, m.video, m.gif) {
                (true, true, true) => "all".into(),
                (true, false, false) => "image".into(),
                (false, true, true) => "video".into(),
                _ => format!("types{}{}{}", m.image as u8, m.video as u8, m.gif as u8),
            }
        } else {
            self.filter.key().into()
        };
        if p.include_retweets && p.date_start.is_none() && p.date_end.is_none() {
            types
        } else {
            format!(
                "{types}:rt{}:{}:{}",
                p.include_retweets as u8,
                p.date_start.as_deref().unwrap_or(""),
                p.date_end.as_deref().unwrap_or("")
            )
        }
    }
    pub fn baseline_key(&self) -> String {
        format!(
            "{}:timeline{}",
            self.scope_key(),
            self.include_timeline as u8
        )
    }
}
pub fn settings_path() -> storage::Result<std::path::PathBuf> {
    Ok(storage::default_root()?
        .parent()
        .ok_or_else(|| storage::StorageError::Invalid("无法定位本机设置".into()))?
        .join("archive-options.json"))
}
pub fn load_settings(path: &Path) -> std::result::Result<Settings, String> {
    storage::check_path(path).map_err(|e| e.to_string())?;
    if !path.exists() {
        return Ok(Settings::default());
    }
    let bytes = fs::read(path).map_err(|_| "无法读取归档设置")?;
    let settings: Settings = serde_json::from_slice(&bytes).map_err(|_| "归档设置格式无效")?;
    settings.preferences.validate()?;
    Ok(settings)
}
pub fn save_settings(path: &Path, settings: &Settings) -> std::result::Result<(), String> {
    settings.preferences.validate()?;
    storage::check_path(path).map_err(|e| e.to_string())?;
    let parent = path.parent().ok_or("无法定位本机设置")?;
    fs::create_dir_all(parent).map_err(|_| "无法保存归档设置")?;
    let mut f = tempfile::NamedTempFile::new_in(parent).map_err(|_| "无法保存归档设置")?;
    f.write_all(&serde_json::to_vec_pretty(settings).map_err(|_| "无法保存归档设置")?)
        .map_err(|_| "无法保存归档设置")?;
    f.as_file().sync_all().map_err(|_| "无法保存归档设置")?;
    f.persist(path).map_err(|_| "无法保存归档设置")?;
    Ok(())
}
