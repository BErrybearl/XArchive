use flate2::read::DeflateDecoder;
use serde::Serialize;
use std::{io::Read, sync::OnceLock};

const PROJECT: &str = include_str!(concat!(env!("OUT_DIR"), "/project-license.txt"));
const NOTICES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/notices.deflate"));
static SECTIONS: OnceLock<Result<Vec<(String, String)>, String>> = OnceLock::new();

#[derive(Serialize)]
pub struct LicenseNotices {
    titles: Vec<String>,
    text: String,
    section: usize,
}

fn sections() -> Result<&'static Vec<(String, String)>, String> {
    SECTIONS
        .get_or_init(|| {
            let mut text = String::new();
            DeflateDecoder::new(NOTICES)
                .read_to_string(&mut text)
                .map_err(|_| "无法读取内嵌许可声明".to_owned())?;
            let mut sections = vec![("XArchive · 项目许可".into(), PROJECT.into())];
            let mut boundaries = vec![0];
            boundaries.extend(text.match_indices("\n### ").map(|(index, _)| index));
            boundaries.push(text.len());
            for range in boundaries.windows(2) {
                let body = &text[range[0]..range[1]];
                let title = body
                    .trim_start()
                    .lines()
                    .next()
                    .unwrap_or("第三方声明")
                    .trim_start_matches('#')
                    .trim()
                    .to_owned();
                sections.push((title, body.to_owned()));
            }
            Ok(sections)
        })
        .as_ref()
        .map_err(Clone::clone)
}

pub fn read(section: usize) -> Result<LicenseNotices, String> {
    let sections = sections()?;
    let (_, text) = sections.get(section).ok_or("许可章节不存在")?;
    Ok(LicenseNotices {
        titles: sections.iter().map(|(title, _)| title.clone()).collect(),
        text: text.clone(),
        section,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_notices_preserve_every_byte_and_reject_invalid_section() {
        let sections = sections().unwrap();
        let restored: String = sections[1..]
            .iter()
            .map(|(_, text)| text.as_str())
            .collect();
        assert_eq!(restored, include_str!("../../THIRD_PARTY_NOTICES.md"));
        assert_eq!(read(0).unwrap().text, PROJECT);
        assert!(read(sections.len()).is_err());
        assert!(
            sections[1..]
                .iter()
                .all(|(_, text)| text.len() < 128 * 1024)
        );
    }
}
