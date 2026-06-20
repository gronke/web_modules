//! Read and merge XLIFF 1.2 localisation files (e.g. lit-localize interchange)
//! into a `{ id: translation }` map for runtime use, via [`quick_xml`].

use std::collections::BTreeMap;

use quick_xml::events::Event;
use quick_xml::{Reader, XmlVersion};

use crate::{Error, Result};

/// One `<trans-unit>`: its `id`, source text, and (possibly empty) target text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unit {
    pub id: String,
    pub source: String,
    pub target: String,
}

/// Parse the `<trans-unit>`s of an XLIFF 1.2 document.
pub fn parse_xliff(xml: &str) -> Result<Vec<Unit>> {
    parse_xliff_inner(xml).map_err(|e| Error::I18n(e.to_string()))
}

fn parse_xliff_inner(xml: &str) -> std::result::Result<Vec<Unit>, Box<dyn std::error::Error>> {
    let mut reader = Reader::from_str(xml);
    let mut units = Vec::new();
    let mut id: Option<String> = None;
    let mut source = String::new();
    let mut target = String::new();
    let mut in_source = false;
    let mut in_target = false;

    loop {
        match reader.read_event()? {
            Event::Start(e) => match e.name().as_ref() {
                b"trans-unit" => {
                    id = e
                        .try_get_attribute("id")?
                        .map(|a| {
                            a.normalized_value(XmlVersion::Implicit1_0)
                                .map(|v| v.into_owned())
                        })
                        .transpose()?;
                    source.clear();
                    target.clear();
                }
                b"source" => in_source = true,
                b"target" => in_target = true,
                _ => {}
            },
            Event::Text(t) => {
                let text = t.xml_content(XmlVersion::Implicit1_0)?;
                if in_source {
                    source.push_str(&text);
                } else if in_target {
                    target.push_str(&text);
                }
            }
            Event::End(e) => match e.name().as_ref() {
                b"source" => in_source = false,
                b"target" => in_target = false,
                b"trans-unit" => {
                    if let Some(id) = id.take() {
                        units.push(Unit {
                            id,
                            source: std::mem::take(&mut source),
                            target: std::mem::take(&mut target),
                        });
                    }
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(units)
}

/// Merge XLIFF documents into a `{ id: text }` map, the target if present, else
/// the source. Later documents win on duplicate ids (so module-specific files can
/// override shared ones).
pub fn merge_to_map(docs: &[&str]) -> Result<BTreeMap<String, String>> {
    let mut map = BTreeMap::new();
    for xml in docs {
        for unit in parse_xliff(xml)? {
            let text = if unit.target.is_empty() {
                unit.source
            } else {
                unit.target
            };
            map.insert(unit.id, text);
        }
    }
    Ok(map)
}

/// Render the merged translation map as a pretty JSON object.
pub fn merge_to_json(docs: &[&str]) -> Result<String> {
    serde_json::to_string_pretty(&merge_to_map(docs)?).map_err(|e| Error::I18n(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const XLF: &str = r#"<?xml version="1.0"?>
<xliff version="1.2"><file><body>
<trans-unit id="greeting"><source>Hello</source><target>Hallo</target></trans-unit>
<trans-unit id="bye"><source>Bye</source><target></target></trans-unit>
</body></file></xliff>"#;

    #[test]
    fn parses_and_merges() {
        let units = parse_xliff(XLF).unwrap();
        assert_eq!(units.len(), 2);
        assert_eq!(
            units[0],
            Unit {
                id: "greeting".into(),
                source: "Hello".into(),
                target: "Hallo".into(),
            }
        );
        let map = merge_to_map(&[XLF]).unwrap();
        assert_eq!(map.get("greeting").unwrap(), "Hallo");
        // Empty target falls back to the source.
        assert_eq!(map.get("bye").unwrap(), "Bye");
    }
}
