use std::fmt::Write as _;
use std::path::Path;

use docling_pdf::{PdfPage, TextCell, textparse::PageTextParser};
use serde_json::Value;

use super::{clean_text, page_no};

pub(super) struct SourceCode<'a> {
    input: &'a Path,
    parser: Option<Option<PageTextParser>>,
    page: Option<(usize, PdfPage)>,
}

impl<'a> SourceCode<'a> {
    pub(super) fn new(input: &'a Path) -> Self {
        Self {
            input,
            parser: None,
            page: None,
        }
    }

    pub(super) fn recover(&mut self, item: &Value) -> Option<String> {
        let page = usize::try_from(page_no(item)).ok()?.checked_sub(1)?;
        let parser = self
            .parser
            .get_or_insert_with(|| PageTextParser::open(&std::fs::read(self.input).ok()?))
            .as_mut()?;
        if self.page.as_ref().is_none_or(|(cached, _)| *cached != page) {
            self.page = Some((page, parser.text_page(page)));
        }
        let parsed = &self.page.as_ref()?.1;
        let bbox = &item["prov"][0]["bbox"];
        let left = bbox["l"].as_f64()? as f32;
        let right = bbox["r"].as_f64()? as f32;
        let mut top = bbox["t"].as_f64()? as f32;
        let mut bottom = bbox["b"].as_f64()? as f32;
        match bbox["coord_origin"].as_str()? {
            "BOTTOMLEFT" => {
                top = parsed.height - top;
                bottom = parsed.height - bottom;
            }
            "TOPLEFT" => {}
            _ => return None,
        }
        let cells: Vec<_> = parsed
            .word_cells
            .iter()
            .filter(|cell| {
                (left - 1.0..=right + 1.0).contains(&center_x(cell))
                    && (top - 1.0..=bottom + 1.0).contains(&center_y(cell))
                    && !cell.text.trim().is_empty()
            })
            .collect();
        source_lines(cells)
    }
}

fn center_x(cell: &TextCell) -> f32 {
    (cell.l + cell.r) / 2.0
}

fn center_y(cell: &TextCell) -> f32 {
    (cell.t + cell.b) / 2.0
}

fn source_lines(mut cells: Vec<&TextCell>) -> Option<String> {
    let chars: usize = cells.iter().map(|cell| cell.text.chars().count()).sum();
    let width: f32 = cells.iter().map(|cell| cell.r - cell.l).sum();
    if chars == 0 || !width.is_finite() {
        return None;
    }
    let pitch = (width / chars as f32).max(1.0);
    let left = cells.iter().map(|cell| cell.l).reduce(f32::min)?;
    cells.sort_by(|a, b| center_y(a).total_cmp(&center_y(b)));

    // Use the baseline geometry to undo the PDF content stream's drawing
    // order, which can put a closing brace after a running header.
    let mut lines: Vec<Vec<&TextCell>> = Vec::new();
    for cell in cells {
        if let Some(line) = lines.last_mut()
            && (center_y(cell) - center_y(line[0])).abs() <= (cell.b - cell.t) * 0.30
        {
            line.push(cell);
        } else {
            lines.push(vec![cell]);
        }
    }
    let mut advances: Vec<_> = lines
        .windows(2)
        .map(|pair| center_y(pair[1][0]) - center_y(pair[0][0]))
        .collect();
    advances.sort_by(f32::total_cmp);
    let typical_advance = advances.get(advances.len() / 2).copied();
    let mut output = String::new();
    let mut previous = None;
    for mut line in lines {
        let y = center_y(line[0]);
        if let Some(last_y) = previous {
            output.push('\n');
            if typical_advance.is_some_and(|advance| y - last_y > 1.65 * advance) {
                output.push('\n');
            }
        }
        previous = Some(y);
        line.sort_by(|a, b| a.l.total_cmp(&b.l));
        let mut right = left;
        for cell in line {
            let spaces = ((cell.l - right) / pitch).round().clamp(0.0, 120.0) as usize;
            let text = clean_text(&cell.text);
            write!(output, "{:spaces$}{text}", "").unwrap();
            right = cell.r;
        }
    }
    (!output.is_empty()).then_some(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restores_indented_source_lines_in_spatial_order() {
        let cell = |text: &str, l, t| TextCell {
            text: text.into(),
            l,
            t,
            r: l + text.len() as f32 * 4.0,
            b: t + 8.0,
        };
        let cells = [
            cell("}", 10.0, 30.0),
            cell("work();", 18.0, 20.0),
            cell("fn", 10.0, 10.0),
            cell("main(){", 22.0, 10.0),
        ];
        assert_eq!(
            source_lines(cells.iter().collect()).as_deref(),
            Some("fn main(){\n  work();\n}")
        );
    }
}
