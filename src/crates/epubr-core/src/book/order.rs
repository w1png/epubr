use serde_json::Value;

use super::label;

/// Repair local inversions among ordinary paragraphs in the same column.
/// Other block types, group boundaries and page breaks keep their Docling order.
pub(super) fn repair(items: &mut [&Value]) {
    let mut offset = 0;
    while offset < items.len() {
        let Some(first) = Region::from_item(items[offset]) else {
            offset += 1;
            continue;
        };
        let mut run = vec![(items[offset], first)];
        for &item in &items[offset + 1..] {
            let Some(region) = Region::from_item(item) else {
                break;
            };
            if !first.same_column(region) {
                break;
            }
            run.push((item, region));
        }
        if run.windows(2).any(|pair| pair[0].1.top > pair[1].1.top) {
            run.sort_by(|(_, a), (_, b)| a.top.total_cmp(&b.top));
            // Glyph boxes can protrude slightly into the neighboring line.
            // Larger overlaps make the physical order ambiguous.
            if run.windows(2).all(|pair| {
                let (_, above) = pair[0];
                let (_, below) = pair[1];
                let shorter = (above.bottom - above.top).min(below.bottom - below.top);
                let allowance = 3.0_f64.min(shorter * 0.25);
                above.bottom <= below.top + allowance
            }) {
                for (target, &(item, _)) in items[offset..].iter_mut().zip(&run) {
                    *target = item;
                }
            }
        }
        offset += run.len();
    }
}

#[derive(Clone, Copy)]
struct Region<'a> {
    parent: &'a str,
    page: u64,
    top_left: bool,
    left: f64,
    right: f64,
    top: f64,
    bottom: f64,
}

impl<'a> Region<'a> {
    fn from_item(item: &'a Value) -> Option<Self> {
        if !matches!(label(item), "text" | "paragraph")
            || item["children"]
                .as_array()
                .is_some_and(|items| !items.is_empty())
        {
            return None;
        }
        let [provenance] = item["prov"].as_array()?.as_slice() else {
            return None;
        };
        let page = provenance["page_no"].as_u64().filter(|&page| page > 0)?;
        let bbox = &provenance["bbox"];
        let top_left = match bbox["coord_origin"].as_str()? {
            "BOTTOMLEFT" => false,
            "TOPLEFT" => true,
            _ => return None,
        };
        let direction = if top_left { 1.0 } else { -1.0 };
        let region = Self {
            parent: item["parent"]["$ref"].as_str()?,
            page,
            top_left,
            left: bbox["l"].as_f64()?,
            right: bbox["r"].as_f64()?,
            top: direction * bbox["t"].as_f64()?,
            bottom: direction * bbox["b"].as_f64()?,
        };
        (region.left < region.right && region.top < region.bottom).then_some(region)
    }

    fn same_column(self, other: Self) -> bool {
        let overlap = self.right.min(other.right) - self.left.max(other.left);
        let width = (self.right - self.left).max(other.right - other.left);
        self.page == other.page
            && self.parent == other.parent
            && self.top_left == other.top_left
            && overlap >= width * 0.85
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn paragraph(text: &str, page: u64, bbox: [f64; 4]) -> Value {
        let [l, t, r, b] = bbox;
        json!({
            "label": "text",
            "text": text,
            "parent": {"$ref": "#/body"},
            "prov": [{"page_no": page, "bbox": {"l": l, "t": t, "r": r, "b": b, "coord_origin": "BOTTOMLEFT"}}]
        })
    }

    #[test]
    fn repairs_inverted_paragraphs_without_crossing_page_or_column_boundaries() {
        let blocks = [
            paragraph("lower", 183, [51.16, 119.70, 416.56, 84.57]),
            paragraph("upper", 183, [51.16, 176.94, 416.56, 118.40]),
            paragraph("continuation", 184, [51.16, 595.88, 416.56, 478.79]),
            paragraph("left column", 184, [51.16, 170.0, 230.0, 120.0]),
            paragraph("right column", 184, [250.0, 220.0, 416.56, 175.0]),
        ];
        let mut items: Vec<_> = blocks.iter().collect();
        repair(&mut items);
        assert_eq!(
            items,
            [&blocks[1], &blocks[0], &blocks[2], &blocks[3], &blocks[4]]
        );
    }

    #[test]
    fn repairs_a_run_with_slightly_overlapping_glyph_boxes() {
        let blocks = [
            paragraph("read", 201, [51.16, 433.25, 416.56, 327.87]),
            paragraph("write", 201, [51.16, 560.76, 416.56, 430.65]),
            paragraph("data lines", 201, [51.16, 327.87, 416.56, 212.07]),
            paragraph("inverted buffer", 201, [51.16, 119.70, 416.56, 49.44]),
            paragraph("buffer", 201, [51.16, 212.07, 416.56, 119.70]),
        ];
        let mut items: Vec<_> = blocks.iter().collect();
        repair(&mut items);
        assert_eq!(
            items,
            [&blocks[1], &blocks[0], &blocks[2], &blocks[4], &blocks[3]]
        );
    }

    #[test]
    fn retains_order_when_regions_overlap_substantially() {
        let blocks = [
            paragraph("first", 1, [50.0, 200.0, 400.0, 100.0]),
            paragraph("second", 1, [50.0, 250.0, 400.0, 180.0]),
        ];
        let mut items: Vec<_> = blocks.iter().collect();
        repair(&mut items);
        assert_eq!(items, [&blocks[0], &blocks[1]]);
    }
}
