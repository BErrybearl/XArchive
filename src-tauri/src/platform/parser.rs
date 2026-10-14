use super::error::{Category, PlatformError, Result};
use crate::auth::Account;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashSet;
use url::Url;

pub fn target_name(raw: &str) -> Result<String> {
    let raw = raw.trim();
    let name = if raw.contains("://") {
        let url = Url::parse(raw).map_err(|_| PlatformError::new(Category::InvalidTarget))?;
        if url.scheme() != "https"
            || !matches!(
                url.host_str(),
                Some("x.com" | "www.x.com" | "twitter.com" | "www.twitter.com")
            )
            || !url.username().is_empty()
            || url.password().is_some()
            || url.port().is_some()
        {
            return Err(PlatformError::new(Category::InvalidTarget));
        }
        let path = url.path().trim_matches('/');
        if path.contains('/') {
            return Err(PlatformError::new(Category::InvalidTarget));
        }
        path.to_owned()
    } else {
        raw.strip_prefix('@').unwrap_or(raw).to_owned()
    };
    if name.is_empty()
        || name.len() > 15
        || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        || [
            "home",
            "explore",
            "notifications",
            "messages",
            "settings",
            "i",
            "search",
        ]
        .contains(&name.to_ascii_lowercase().as_str())
    {
        return Err(PlatformError::new(Category::InvalidTarget));
    }
    Ok(name)
}

pub fn account(result: &Value) -> Result<Account> {
    let id = result
        .get("rest_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.len() <= 32 && s.bytes().all(|b| b.is_ascii_digit()));
    let name = result
        .pointer("/core/screen_name")
        .or_else(|| result.pointer("/legacy/screen_name"))
        .and_then(Value::as_str);
    let (Some(id), Some(name)) = (id, name) else {
        return Err(PlatformError::interface());
    };
    target_name(name).map_err(|_| PlatformError::interface())?;
    let display = result
        .pointer("/core/name")
        .or_else(|| result.pointer("/legacy/name"))
        .and_then(Value::as_str)
        .unwrap_or(name);
    Ok(Account {
        user_id: id.into(),
        screen_name: name.into(),
        display_name: display.chars().take(200).collect(),
    })
}

pub fn viewer(value: &Value) -> Result<Account> {
    account(
        value
            .pointer("/data/viewer/user_results/result")
            .ok_or_else(PlatformError::interface)?,
    )
}

pub fn target(value: &Value) -> Result<Account> {
    let user = value
        .pointer("/data/user")
        .ok_or_else(PlatformError::interface)?;
    if user.is_null() {
        return Err(PlatformError::new(Category::TargetUnavailable));
    }
    let result = user.get("result").ok_or_else(PlatformError::interface)?;
    if result.is_null()
        || result
            .get("__typename")
            .and_then(Value::as_str)
            .is_some_and(|t| ["UserUnavailable", "UserTombstone"].contains(&t))
    {
        return Err(PlatformError::new(Category::TargetUnavailable));
    }
    account(result)
}

#[derive(Default, Debug, Serialize, PartialEq, Eq)]
pub struct MediaStats {
    pub tweets: usize,
    pub images: usize,
    pub videos: usize,
    pub gifs: usize,
    pub unavailable: usize,
    pub has_next_page: bool,
}

// Kept inside the parser only. No URL or raw media object crosses IPC.
pub fn best_mp4(media: &Value) -> Option<&str> {
    media
        .pointer("/video_info/variants")?
        .as_array()?
        .iter()
        .filter(|v| v.get("content_type").and_then(Value::as_str) == Some("video/mp4"))
        .filter(|v| v.get("url").and_then(Value::as_str).is_some())
        .max_by_key(|v| v.get("bitrate").and_then(Value::as_u64).unwrap_or(0))?
        .get("url")?
        .as_str()
}

fn unwrap_tweet(mut result: &Value) -> &Value {
    for _ in 0..8 {
        if result.get("__typename").and_then(Value::as_str) == Some("TweetWithVisibilityResults") {
            if let Some(tweet) = result.get("tweet") {
                result = tweet;
                continue;
            }
        }
        break;
    }
    result
}

fn item(content: &Value, stats: &mut MediaStats, seen: &mut HashSet<String>) -> Result<()> {
    if content.get("cursorType").and_then(Value::as_str) == Some("Bottom") {
        stats.has_next_page |= content
            .get("value")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty());
        return Ok(());
    }
    if let Some(items) = content
        .get("items")
        .or_else(|| content.get("moduleItems"))
        .and_then(Value::as_array)
    {
        for entry in items {
            item(entry.get("item").unwrap_or(entry), stats, seen)?;
        }
        return Ok(());
    }
    if let Some(inner) = content.get("itemContent") {
        return item(inner, stats, seen);
    }
    let Some(result) = content.pointer("/tweet_results/result") else {
        let kind = content.get("itemType").and_then(Value::as_str);
        if content.get("tweet_results").is_some()
            || matches!(kind, Some("TimelineTweet" | "TimelineTombstone"))
        {
            stats.unavailable += 1;
            return Ok(());
        }
        if content.get("cursorType").is_some()
            || matches!(
                kind,
                Some("TimelineUser" | "TimelinePrompt" | "TimelineMessagePrompt")
            )
        {
            return Ok(());
        }
        return Err(PlatformError::interface());
    };
    let outer = unwrap_tweet(result);
    if outer.is_null()
        || outer
            .get("__typename")
            .and_then(Value::as_str)
            .is_some_and(|t| ["TweetTombstone", "TweetUnavailable"].contains(&t))
    {
        stats.unavailable += 1;
        return Ok(());
    }
    let Some(id) = outer
        .get("rest_id")
        .and_then(Value::as_str)
        .or_else(|| outer.pointer("/legacy/id_str").and_then(Value::as_str))
    else {
        return Err(PlatformError::interface());
    };
    if !seen.insert(id.to_owned()) {
        return Ok(());
    }
    stats.tweets += 1;
    // Retweets count once as returned timeline posts; inspect original media.
    // Quoted posts are not traversed: they are separate posts, not this post's attachments.
    let mut tweet = outer;
    for _ in 0..8 {
        if let Some(original) = tweet.pointer("/legacy/retweeted_status_result/result") {
            tweet = unwrap_tweet(original);
        } else {
            break;
        }
    }
    if tweet
        .get("__typename")
        .and_then(Value::as_str)
        .is_some_and(|t| ["TweetTombstone", "TweetUnavailable"].contains(&t))
    {
        stats.unavailable += 1;
        return Ok(());
    }
    let Some(legacy) = tweet.get("legacy").filter(|v| v.is_object()) else {
        return Err(PlatformError::interface());
    };
    let media = legacy
        .pointer("/extended_entities/media")
        .or_else(|| legacy.pointer("/entities/media"));
    if let Some(media) = media {
        let list = media.as_array().ok_or_else(PlatformError::interface)?;
        for entry in list {
            match entry.get("type").and_then(Value::as_str) {
                Some("photo") => stats.images += 1,
                Some("video") => {
                    let _selected = best_mp4(entry);
                    stats.videos += 1;
                }
                Some("animated_gif") => {
                    let _selected = best_mp4(entry);
                    stats.gifs += 1;
                }
                _ => return Err(PlatformError::interface()),
            }
        }
    }
    Ok(())
}

pub fn media_page(value: &Value) -> Result<MediaStats> {
    let instructions = value
        .pointer("/data/user/result/timeline_v2/timeline/instructions")
        .or_else(|| value.pointer("/data/user/result/timeline/timeline/instructions"))
        .or_else(|| value.pointer("/data/user/result/timeline/instructions"))
        .and_then(Value::as_array)
        .ok_or_else(PlatformError::interface)?;
    let mut stats = MediaStats::default();
    let mut seen = HashSet::new();
    let mut recognized = instructions.is_empty();
    for instruction in instructions {
        match instruction.get("type").and_then(Value::as_str) {
            Some("TimelineAddEntries") => {
                recognized = true;
                for entry in instruction
                    .get("entries")
                    .and_then(Value::as_array)
                    .ok_or_else(PlatformError::interface)?
                {
                    item(
                        entry.get("content").ok_or_else(PlatformError::interface)?,
                        &mut stats,
                        &mut seen,
                    )?;
                }
            }
            Some("TimelineReplaceEntry") => {
                recognized = true;
                item(
                    instruction
                        .pointer("/entry/content")
                        .ok_or_else(PlatformError::interface)?,
                    &mut stats,
                    &mut seen,
                )?;
            }
            Some("TimelineAddToModule") => {
                recognized = true;
                for entry in instruction
                    .get("moduleItems")
                    .and_then(Value::as_array)
                    .ok_or_else(PlatformError::interface)?
                {
                    item(entry.get("item").unwrap_or(entry), &mut stats, &mut seen)?;
                }
            }
            Some("TimelineClearCache" | "TimelineTerminateTimeline" | "TimelineShowAlert") => {
                recognized = true;
            }
            _ => {}
        }
    }
    if !recognized {
        return Err(PlatformError::interface());
    }
    Ok(stats)
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Attachment {
    pub key: String,
    pub url: String,
    pub filename: String,
    pub legacy_filename: String,
    pub kind: String,
    pub mime: String,
    #[serde(default)]
    pub variants: Vec<(u64, String)>,
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ArchivedTweet {
    pub id: String,
    pub author: Account,
    pub text: String,
    pub published_at: Option<String>,
    pub timestamp: Option<u64>,
    pub pinned: bool,
    #[serde(default)]
    pub retweet: bool,
    pub media: Vec<Attachment>,
}
#[derive(Default, Debug)]
pub struct ArchivePage {
    pub tweets: Vec<ArchivedTweet>,
    pub next: Option<String>,
    pub unavailable: u64,
}

pub fn image_original(raw: &str) -> Result<String> {
    let mut url = Url::parse(raw).map_err(|_| PlatformError::interface())?;
    let pairs: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(key, _)| key != "name")
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    url.set_query(None);
    url.query_pairs_mut()
        .extend_pairs(pairs)
        .append_pair("name", "orig");
    Ok(url.into())
}

// Parse the public Twitter timestamp without introducing a runtime dependency.
fn tweet_time(raw: &str) -> Option<u64> {
    let parts: Vec<_> = raw.split_whitespace().collect();
    if parts.len() != 6 || parts[4] != "+0000" {
        return None;
    }
    let month = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ]
    .iter()
    .position(|m| *m == parts[1])? as i64
        + 1;
    let day: i64 = parts[2].parse().ok()?;
    let year: i64 = parts[5].parse().ok()?;
    let time: Vec<u64> = parts[3]
        .split(':')
        .map(str::parse)
        .collect::<std::result::Result<_, _>>()
        .ok()?;
    if !(1970..=9999).contains(&year)
        || !(1..=31).contains(&day)
        || time.len() != 3
        || time[0] > 23
        || time[1] > 59
        || time[2] > 59
    {
        return None;
    }
    let y = year - i64::from(month <= 2);
    let era = y / 400;
    let yoe = y - era * 400;
    let mp = month + if month > 2 { -3 } else { 9 };
    let days =
        era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + (153 * mp + 2) / 5 + day - 1 - 719468;
    u64::try_from(days)
        .ok()?
        .checked_mul(86400)?
        .checked_add(time[0] * 3600 + time[1] * 60 + time[2])
}

fn archive_tweet(
    result: &Value,
    pinned: bool,
    retweet: bool,
    page: &mut ArchivePage,
    seen: &mut HashSet<String>,
) -> Result<()> {
    let tweet = unwrap_tweet(result);
    if tweet.is_null()
        || matches!(
            tweet["__typename"].as_str(),
            Some("TweetTombstone" | "TweetUnavailable")
        )
    {
        page.unavailable += 1;
        return Ok(());
    }
    let legacy = tweet
        .get("legacy")
        .filter(|v| v.is_object())
        .ok_or_else(PlatformError::interface)?;
    let id = tweet["rest_id"]
        .as_str()
        .or_else(|| legacy["id_str"].as_str())
        .filter(|s| !s.is_empty() && s.len() <= 32 && s.bytes().all(|b| b.is_ascii_digit()))
        .ok_or_else(PlatformError::interface)?;
    if !seen.insert(id.to_owned()) {
        if pinned {
            if let Some(saved) = page.tweets.iter_mut().find(|t| t.id == id) {
                saved.pinned = true;
            }
        }
        if !retweet {
            if let Some(saved) = page.tweets.iter_mut().find(|t| t.id == id) {
                saved.retweet = false;
            }
        }
        return Ok(());
    }
    let author = account(
        tweet
            .pointer("/core/user_results/result")
            .ok_or_else(PlatformError::interface)?,
    )?;
    let published_at = legacy["created_at"].as_str().map(str::to_owned);
    let timestamp = published_at.as_deref().and_then(tweet_time);
    let mut media = Vec::new();
    if let Some(entries) = legacy
        .pointer("/extended_entities/media")
        .or_else(|| legacy.pointer("/entities/media"))
    {
        for (index, entry) in entries
            .as_array()
            .ok_or_else(PlatformError::interface)?
            .iter()
            .enumerate()
        {
            let (kind, url, mime) = match entry["type"].as_str() {
                Some("photo") => {
                    let url = image_original(
                        entry["media_url_https"]
                            .as_str()
                            .ok_or_else(PlatformError::interface)?,
                    )?;
                    let u = Url::parse(&url).map_err(|_| PlatformError::interface())?;
                    let format = u
                        .query_pairs()
                        .find(|(k, _)| k == "format")
                        .map(|(_, v)| v.into_owned())
                        .unwrap_or_else(|| u.path().rsplit('.').next().unwrap_or("jpg").to_owned());
                    let mime = match format.as_str() {
                        "png" => "image/png",
                        "webp" => "image/webp",
                        "gif" => "image/gif",
                        _ => "image/jpeg",
                    };
                    ("image", url, mime)
                }
                Some("video" | "animated_gif") => (
                    if entry["type"] == "video" {
                        "video"
                    } else {
                        "gif"
                    },
                    best_mp4(entry)
                        .ok_or_else(PlatformError::interface)?
                        .to_owned(),
                    "video/mp4",
                ),
                _ => return Err(PlatformError::interface()),
            };
            let u = Url::parse(&url).map_err(|_| PlatformError::interface())?;
            let mut filename = u
                .path_segments()
                .and_then(|mut s| s.next_back())
                .filter(|s| !s.is_empty())
                .ok_or_else(PlatformError::interface)?
                .to_owned();
            if !filename.contains('.') {
                let ext = u
                    .query_pairs()
                    .find(|(k, _)| k == "format")
                    .map(|(_, v)| v.into_owned())
                    .unwrap_or_else(|| if kind == "image" { "jpg" } else { "mp4" }.into());
                filename.push('.');
                filename.push_str(&ext);
            }
            let ext = filename.rsplit('.').next().unwrap_or("bin");
            media.push(Attachment {
                key: entry["media_key"]
                    .as_str()
                    .or_else(|| entry["id_str"].as_str())
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("slot:{index}")),
                legacy_filename: format!("{id}_{index:02}.{ext}"),
                filename,
                url,
                kind: kind.into(),
                mime: mime.into(),
                variants: entry
                    .pointer("/video_info/variants")
                    .and_then(Value::as_array)
                    .map(|v| {
                        v.iter()
                            .filter(|v| v["content_type"] == "video/mp4")
                            .filter_map(|v| {
                                Some((
                                    v["bitrate"].as_u64().unwrap_or(0),
                                    v["url"].as_str()?.to_owned(),
                                ))
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            });
        }
    }
    page.tweets.push(ArchivedTweet {
        id: id.into(),
        author,
        text: tweet
            .pointer("/note_tweet/note_tweet_results/result/text")
            .and_then(Value::as_str)
            .or_else(|| legacy["full_text"].as_str())
            .unwrap_or("")
            .into(),
        published_at,
        timestamp,
        pinned,
        retweet: retweet || legacy.pointer("/retweeted_status_result/result").is_some(),
        media,
    });
    // Keep the outer retweet metadata and the original post's author/source identity.
    if let Some(original) = legacy.pointer("/retweeted_status_result/result") {
        archive_tweet(original, true, true, page, seen)?;
    }
    Ok(())
}

fn archive_item(
    content: &Value,
    pinned: bool,
    page: &mut ArchivePage,
    seen: &mut HashSet<String>,
) -> Result<()> {
    if content["cursorType"] == "Bottom" {
        let value = content["value"]
            .as_str()
            .ok_or_else(PlatformError::interface)?;
        if value.len() > 8192 {
            return Err(PlatformError::interface());
        }
        if !value.is_empty() {
            page.next = Some(value.into());
        }
        return Ok(());
    }
    if let Some(items) = content
        .get("items")
        .or_else(|| content.get("moduleItems"))
        .and_then(Value::as_array)
    {
        for entry in items {
            archive_item(entry.get("item").unwrap_or(entry), pinned, page, seen)?;
        }
        return Ok(());
    }
    if let Some(inner) = content.get("itemContent") {
        return archive_item(inner, pinned, page, seen);
    }
    if let Some(result) = content.pointer("/tweet_results/result") {
        return archive_tweet(result, pinned, false, page, seen);
    }
    if content.get("tweet_results").is_some()
        || matches!(content["itemType"].as_str(), Some("TimelineTombstone"))
    {
        page.unavailable += 1;
        return Ok(());
    }
    if content.get("cursorType").is_some()
        || matches!(
            content["itemType"].as_str(),
            Some("TimelineUser" | "TimelinePrompt" | "TimelineMessagePrompt")
        )
    {
        return Ok(());
    }
    Err(PlatformError::interface())
}

pub fn archive_page(value: &Value) -> Result<ArchivePage> {
    let instructions = value
        .pointer("/data/user/result/timeline_v2/timeline/instructions")
        .or_else(|| value.pointer("/data/user/result/timeline/timeline/instructions"))
        .or_else(|| value.pointer("/data/user/result/timeline/instructions"))
        .and_then(Value::as_array)
        .ok_or_else(PlatformError::interface)?;
    let mut page = ArchivePage::default();
    let mut seen = HashSet::new();
    let mut recognized = instructions.is_empty();
    for instruction in instructions {
        match instruction["type"].as_str() {
            Some("TimelineAddEntries") => {
                recognized = true;
                for entry in instruction["entries"]
                    .as_array()
                    .ok_or_else(PlatformError::interface)?
                {
                    archive_item(
                        entry.get("content").ok_or_else(PlatformError::interface)?,
                        false,
                        &mut page,
                        &mut seen,
                    )?;
                }
            }
            Some("TimelineReplaceEntry" | "TimelinePinEntry") => {
                recognized = true;
                archive_item(
                    instruction
                        .pointer("/entry/content")
                        .ok_or_else(PlatformError::interface)?,
                    instruction["type"] == "TimelinePinEntry",
                    &mut page,
                    &mut seen,
                )?;
            }
            Some("TimelineAddToModule") => {
                recognized = true;
                for entry in instruction["moduleItems"]
                    .as_array()
                    .ok_or_else(PlatformError::interface)?
                {
                    archive_item(
                        entry.get("item").unwrap_or(entry),
                        false,
                        &mut page,
                        &mut seen,
                    )?;
                }
            }
            Some("TimelineTerminateTimeline") => {
                recognized = true;
                if instruction["direction"] == "Bottom" {
                    page.next = None;
                }
            }
            Some("TimelineClearCache") => {
                recognized = true;
            }
            _ => {}
        }
    }
    if !recognized {
        return Err(PlatformError::interface());
    }
    Ok(page)
}
