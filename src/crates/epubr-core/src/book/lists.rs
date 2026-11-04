use std::collections::HashMap;

use serde_json::Value;

use super::label;

/// Docling can place leading numbers after the text in a list.
/// Require consecutive markers after otherwise complete sentences.
pub(super) fn recover_numbering(items: &[&Value], texts: &mut [String]) -> HashMap<usize, u64> {
    let mut numbering = HashMap::new();
    let mut offset = 0;
    for group in items.chunk_by(|left, right| {
        label(left) == "list_item"
            && label(right) == "list_item"
            && left["parent"]["$ref"].is_string()
            && left["parent"]["$ref"] == right["parent"]["$ref"]
    }) {
        let start = offset;
        offset += group.len();
        if group.len() < 2 || group.iter().any(|item| item["enumerated"] == true) {
            continue;
        }
        let texts = &mut texts[start..offset];
        let Some(markers) = texts
            .iter()
            .map(|text| trailing_number(text))
            .collect::<Option<Vec<_>>>()
        else {
            continue;
        };
        if !markers
            .windows(2)
            .all(|pair| pair[0].1.checked_add(1) == Some(pair[1].1))
        {
            continue;
        }
        for (index, (end, number)) in markers.into_iter().enumerate() {
            texts[index].truncate(end);
            numbering.insert(start + index, number);
        }
    }
    numbering
}

fn trailing_number(text: &str) -> Option<(usize, u64)> {
    let (body, marker) = text.rsplit_once(char::is_whitespace)?;
    let body = body.trim_end();
    let digits = marker.strip_suffix('.')?;
    if !body.ends_with(['.', '?', '!']) || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some((body.len(), digits.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn recovers_displaced_numbers_only_when_the_whole_list_is_consecutive() {
        let item = json!({"label":"list_item","parent":{"$ref":"#/groups/0"}});
        let items = [&item, &item];
        let mut texts = ["Create the program. 1.".to_owned(), "Run it. 2.".to_owned()];
        assert_eq!(
            recover_numbering(&items, &mut texts),
            HashMap::from([(0, 1), (1, 2)])
        );
        assert_eq!(texts, ["Create the program.", "Run it."]);

        let mut texts = ["See note 1.".to_owned(), "See note 2.".to_owned()];
        assert!(recover_numbering(&items, &mut texts).is_empty());
        assert_eq!(texts, ["See note 1.", "See note 2."]);
    }
}
