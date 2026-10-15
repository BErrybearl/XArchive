use super::*;
use crate::storage::layout::timestamp;
use std::collections::HashMap;

pub(super) struct Entity {
    pub id: i64,
    pub stored_path: String,
    pub size: u64,
    pub digest: String,
    pub mime: String,
    pub sources: Vec<Source>,
}
pub(super) fn load_entities(store: &Store, account: &str) -> Result<Vec<Entity>> {
    let mut q=store.connection().prepare("SELECT e.entity_id,e.relative_path,e.byte_length,e.sha256,e.mime_type FROM media_entities e WHERE EXISTS(SELECT 1 FROM media_sources s WHERE s.entity_id=e.entity_id AND s.target_user_id=?1) ORDER BY e.entity_id")?;
    let mut entities = q
        .query_map([account], |r| {
            Ok(Entity {
                id: r.get(0)?,
                stored_path: r.get(1)?,
                size: unsigned(r, 2)?,
                digest: r.get(3)?,
                mime: r.get(4)?,
                sources: Vec::new(),
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let entity_positions: HashMap<_, _> = entities
        .iter()
        .enumerate()
        .map(|(i, e)| (e.id, i))
        .collect();
    let mut q=store.connection().prepare("SELECT s.entity_id,s.tweet_id,t.author_user_id,u.screen_name,t.published_at,t.text,s.original_filename,s.original_url,s.media_type,
        coalesce(nullif(s.sequence,0),row_number() OVER (PARTITION BY s.target_user_id,s.tweet_id ORDER BY s.source_id))
        FROM media_sources s JOIN tweets t ON t.tweet_id=s.tweet_id JOIN users u ON u.user_id=t.author_user_id
        WHERE s.target_user_id=?1 ORDER BY s.source_id")?;
    for row in q.query_map([account], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            Source {
                tweet_id: r.get(1)?,
                author_id: r.get(2)?,
                author: r.get(3)?,
                published_at: r.get(4)?,
                text: r.get(5)?,
                original_filename: r.get(6)?,
                original_url: r.get(7)?,
                media_type: r.get(8)?,
                sequence: unsigned(r, 9)?,
                date: None,
                sort_time: None,
            },
        ))
    })? {
        let (id, mut source) = row?;
        if let Some(raw) = &source.published_at {
            source.sort_time = timestamp(store.connection(), raw)?;
            if let Some(t) = source.sort_time {
                source.date = store.connection().query_row(
                    "SELECT strftime('%Y-%m-%d',?1,'unixepoch')",
                    [t],
                    |r| r.get(0),
                )?;
            }
        }
        let position = *entity_positions
            .get(&id)
            .ok_or_else(|| invalid("媒体来源实体缺失"))?;
        entities[position].sources.push(source);
    }
    for entity in &mut entities {
        entity.sources.sort_by(|a, b| {
            a.sort_time
                .is_none()
                .cmp(&b.sort_time.is_none())
                .then_with(|| a.sort_time.cmp(&b.sort_time))
                .then_with(|| a.tweet_id.len().cmp(&b.tweet_id.len()))
                .then_with(|| a.tweet_id.cmp(&b.tweet_id))
                .then_with(|| a.sequence.cmp(&b.sequence))
        });
        if entity.sources.is_empty() {
            return Err(invalid("媒体实体没有来源"));
        }
    }
    Ok(entities)
}
pub(super) fn csv(entries: &[IndexEntry]) -> Result<Vec<u8>> {
    // One row per file. All sources are JSON in the final RFC 4180 quoted field.
    let mut output =
        String::from("\u{feff}path,entity_id,sha256,byte_length,media_type,mime_type,sources\r\n");
    for e in entries {
        let sources =
            serde_json::to_string(&e.sources).map_err(|_| invalid("无法序列化导出来源"))?;
        let fields = [
            e.path.clone(),
            e.entity_id.to_string(),
            e.sha256.clone(),
            e.byte_length.to_string(),
            e.media_type.clone(),
            e.mime_type.clone(),
            sources,
        ];
        output.push_str(
            &fields
                .iter()
                .map(|s| format!("\"{}\"", s.replace('"', "\"\"")))
                .collect::<Vec<_>>()
                .join(","),
        );
        output.push_str("\r\n");
    }
    Ok(output.into_bytes())
}
