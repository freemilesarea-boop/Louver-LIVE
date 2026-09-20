//! Broadcast metadata: what the viewer sees on the YouTube watch page.
//!
//! Everything here is validated against YouTube's documented limits *before* a
//! request is made, so a too-long title is a clear message in the app rather
//! than a 400 from Google with a field path in it.

use crate::error::{ErrorCode, LouverError, Result};
use serde::{Deserialize, Serialize};

/// YouTube rejects titles over 100 characters.
pub const MAX_TITLE_CHARS: usize = 100;
/// And descriptions over 5000.
pub const MAX_DESCRIPTION_CHARS: usize = 5000;
/// Tags are limited by their *combined* length, not by count. A tag containing
/// a space is quoted by YouTube, which costs two more characters — counted
/// here so a list that passes validation is not rejected by the API.
pub const MAX_TAGS_TOTAL_CHARS: usize = 500;
/// A single tag may not exceed this.
pub const MAX_TAG_CHARS: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Privacy {
    Public,
    Unlisted,
    Private,
}

impl Privacy {
    pub fn as_api(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Unlisted => "unlisted",
            Self::Private => "private",
        }
    }
    pub fn from_api(s: &str) -> Self {
        match s {
            "public" => Self::Public,
            "private" => Self::Private,
            _ => Self::Unlisted,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Public => "공개",
            Self::Unlisted => "일부공개",
            Self::Private => "비공개",
        }
    }
}

/// YouTube category ids. Only the handful a music channel would pick — the
/// full list is long and mostly irrelevant here.
pub const CATEGORIES: &[(&str, &str)] = &[
    ("10", "음악"),
    ("24", "엔터테인먼트"),
    ("22", "인물 및 블로그"),
    ("20", "게임"),
    ("26", "노하우/스타일"),
    ("27", "교육"),
    ("28", "과학기술"),
];

pub fn category_label(id: &str) -> &'static str {
    CATEGORIES.iter().find(|(c, _)| *c == id).map(|(_, l)| *l).unwrap_or("기타")
}

/// What the user typed in 방송 설정, before it becomes API requests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BroadcastMetadata {
    pub title: String,
    pub description: String,
    pub tags: Vec<String>,
    pub category_id: String,
    pub privacy: Privacy,
}

impl Default for BroadcastMetadata {
    fn default() -> Self {
        Self {
            title: String::new(),
            description: String::new(),
            tags: Vec::new(),
            category_id: "10".into(), // Music
            privacy: Privacy::Unlisted,
        }
    }
}

/// Characters a tag list costs against YouTube's 500-character budget.
///
/// A tag with a space in it is stored quoted, so it costs two extra. Commas
/// between tags count too. Getting this wrong means the app accepts a list the
/// API then rejects, which is the failure this function exists to prevent.
pub fn tags_cost(tags: &[String]) -> usize {
    let mut total = 0;
    for (i, t) in tags.iter().enumerate() {
        if i > 0 {
            total += 1; // the separator
        }
        total += t.chars().count() + if t.contains(' ') { 2 } else { 0 };
    }
    total
}

/// Remove blanks and duplicates, keeping the order the user arranged.
///
/// Case-insensitive: YouTube treats `Lofi` and `lofi` as the same tag, so
/// keeping both would silently waste the budget.
pub fn normalize_tags(tags: &[String]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for t in tags {
        let t = t.trim();
        if t.is_empty() {
            continue;
        }
        if seen.insert(t.to_lowercase()) {
            out.push(t.to_string());
        }
    }
    out
}

impl BroadcastMetadata {
    /// Check against YouTube's limits. Counts characters, not bytes: a Korean
    /// title of 100 characters is 300 bytes and is perfectly legal.
    pub fn validate(&self) -> Result<()> {
        let invalid = |detail: String| LouverError::with_detail(ErrorCode::YoutubeMetadataInvalid, detail);

        if self.title.trim().is_empty() {
            return Err(invalid("제목을 입력해주세요".into()));
        }
        let title_len = self.title.chars().count();
        if title_len > MAX_TITLE_CHARS {
            return Err(invalid(format!("제목이 {title_len}자입니다 (최대 {MAX_TITLE_CHARS}자)")));
        }
        // YouTube rejects these outright in titles and descriptions.
        if self.title.contains('<') || self.title.contains('>') {
            return Err(invalid("제목에 < 또는 > 를 쓸 수 없습니다".into()));
        }
        let desc_len = self.description.chars().count();
        if desc_len > MAX_DESCRIPTION_CHARS {
            return Err(invalid(format!("설명이 {desc_len}자입니다 (최대 {MAX_DESCRIPTION_CHARS}자)")));
        }
        if self.description.contains('<') || self.description.contains('>') {
            return Err(invalid("설명에 < 또는 > 를 쓸 수 없습니다".into()));
        }
        for t in &self.tags {
            let n = t.chars().count();
            if n > MAX_TAG_CHARS {
                return Err(invalid(format!("태그 '{t}'가 {n}자입니다 (최대 {MAX_TAG_CHARS}자)")));
            }
        }
        let cost = tags_cost(&self.tags);
        if cost > MAX_TAGS_TOTAL_CHARS {
            return Err(invalid(format!(
                "태그 전체 길이가 {cost}자입니다 (최대 {MAX_TAGS_TOTAL_CHARS}자). 공백이 있는 태그는 따옴표 2자를 더 씁니다"
            )));
        }
        if self.category_id.is_empty() {
            return Err(invalid("카테고리를 선택해주세요".into()));
        }
        Ok(())
    }

    /// Normalize in place, so what is stored is what will be sent.
    pub fn cleaned(&self) -> Self {
        Self {
            title: self.title.trim().to_string(),
            description: self.description.trim_end().to_string(),
            tags: normalize_tags(&self.tags),
            category_id: self.category_id.clone(),
            privacy: self.privacy,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta() -> BroadcastMetadata {
        BroadcastMetadata { title: "ROOM.".into(), ..Default::default() }
    }

    #[test]
    fn a_hundred_korean_characters_is_a_legal_title() {
        // 300 bytes, 100 characters. Counting bytes would reject this wrongly.
        let m = BroadcastMetadata { title: "가".repeat(100), ..meta() };
        assert!(m.validate().is_ok());
        let m = BroadcastMetadata { title: "가".repeat(101), ..meta() };
        assert!(m.validate().is_err());
    }

    #[test]
    fn an_empty_title_is_rejected_before_the_api_sees_it() {
        assert!(BroadcastMetadata { title: "   ".into(), ..Default::default() }.validate().is_err());
    }

    #[test]
    fn angle_brackets_are_rejected_because_youtube_rejects_them() {
        assert!(BroadcastMetadata { title: "lofi <3".into(), ..meta() }.validate().is_err());
        assert!(BroadcastMetadata { description: "a > b".into(), ..meta() }.validate().is_err());
    }

    #[test]
    fn duplicate_tags_are_removed_case_insensitively_keeping_order() {
        let tags = normalize_tags(&[
            "lofi".into(),
            "Jazz".into(),
            "  ".into(),
            "LOFI".into(),
            "chill".into(),
            "jazz".into(),
        ]);
        assert_eq!(tags, vec!["lofi", "Jazz", "chill"]);
    }

    #[test]
    fn a_tag_with_a_space_costs_two_extra_characters() {
        // YouTube stores it quoted. A list that ignores this passes validation
        // here and is rejected by the API, which is the bug being prevented.
        assert_eq!(tags_cost(&["lofi".into()]), 4);
        assert_eq!(tags_cost(&["work music".into()]), 12); // 10 + 2 quotes
        assert_eq!(tags_cost(&["lofi".into(), "jazz".into()]), 9); // 4 + 1 + 4
    }

    #[test]
    fn the_tag_budget_is_enforced_on_the_combined_length() {
        // 80 tags of six characters, plus 79 separators: 559 against a 500
        // budget. (Sixty would have cost 419 and passed.)
        let many: Vec<String> = (0..80).map(|i| format!("tag{i:03}")).collect();
        assert_eq!(tags_cost(&many), 559);
        let m = BroadcastMetadata { tags: many, ..meta() };
        let err = m.validate().unwrap_err();
        assert_eq!(err.code_str, "LL-YOUTUBE-006");
        assert!(err.detail.unwrap().contains("태그 전체 길이"));
    }

    #[test]
    fn privacy_round_trips_through_the_api_spelling() {
        for p in [Privacy::Public, Privacy::Unlisted, Privacy::Private] {
            assert_eq!(Privacy::from_api(p.as_api()), p);
        }
    }
}
