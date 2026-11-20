//! Reading a document's outline tree.

use crate::object::dict::keys::{A, D, DEST, DESTS, FIRST, KIDS, NAMES, NEXT, OUTLINES, S, TITLE};
use crate::object::{Array, Dict, Name, Object, ObjectIdentifier, String as PdfString};
use crate::page::Pages;
use crate::xref::XRef;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec::Vec;

const MAX_OUTLINE_ITEMS: usize = 10_000;
const MAX_TREE_DEPTH: usize = 64;

/// A PDF outline entry and its child entries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutlineItem {
    /// The displayed title.
    pub title: String,
    /// The zero-based destination page, when the entry has an internal page destination.
    pub page_index: Option<usize>,
    /// The nested entries beneath this item.
    pub children: Vec<Self>,
}

pub(crate) fn read_outlines(xref: &XRef, pages: &Pages<'_>) -> Vec<OutlineItem> {
    let Some(catalog) = xref.get::<Dict<'_>>(xref.root_id()) else {
        return Vec::new();
    };
    let Some(outline_root) = catalog.get::<Dict<'_>>(OUTLINES) else {
        return Vec::new();
    };

    let page_indices = pages
        .iter()
        .enumerate()
        .filter_map(|(index, page)| page.raw().obj_id().map(|id| (id, index)))
        .collect();
    let named_destinations = collect_named_destinations(&catalog);
    let mut reader = OutlineReader {
        page_indices,
        named_destinations,
        visited: BTreeSet::new(),
        remaining_items: MAX_OUTLINE_ITEMS,
    };

    outline_root
        .get::<Dict<'_>>(FIRST)
        .map(|first| reader.read_siblings(first, 0))
        .unwrap_or_default()
}

struct OutlineReader<'a> {
    page_indices: BTreeMap<ObjectIdentifier, usize>,
    named_destinations: BTreeMap<Vec<u8>, Object<'a>>,
    visited: BTreeSet<ObjectIdentifier>,
    remaining_items: usize,
}

impl OutlineReader<'_> {
    fn read_siblings(&mut self, first: Dict<'_>, depth: usize) -> Vec<OutlineItem> {
        if depth >= MAX_TREE_DEPTH || self.remaining_items == 0 {
            return Vec::new();
        }

        let mut result = Vec::new();
        let mut current = Some(first);
        while let Some(item) = current {
            if self.remaining_items == 0 {
                break;
            }
            self.remaining_items -= 1;

            if let Some(id) = item.obj_id()
                && !self.visited.insert(id)
            {
                break;
            }

            let next = item.get::<Dict<'_>>(NEXT);
            let title = item
                .get::<PdfString<'_>>(TITLE)
                .map(|value| decode_pdf_text(value.as_bytes()))
                .unwrap_or_default();
            let page_index = item
                .get::<Object<'_>>(DEST)
                .or_else(|| self.go_to_destination(&item))
                .and_then(|destination| self.destination_page(destination, 0));
            let children = item
                .get::<Dict<'_>>(FIRST)
                .map(|first_child| self.read_siblings(first_child, depth + 1))
                .unwrap_or_default();

            result.push(OutlineItem {
                title,
                page_index,
                children,
            });
            current = next;
        }

        result
    }

    fn go_to_destination<'a>(&self, item: &Dict<'a>) -> Option<Object<'a>> {
        let action = item.get::<Dict<'_>>(A)?;
        let action_type = action.get::<Name<'_>>(S)?;
        (action_type.as_ref() == b"GoTo")
            .then(|| action.get::<Object<'_>>(D))
            .flatten()
    }

    fn destination_page(&self, destination: Object<'_>, depth: usize) -> Option<usize> {
        if depth >= 8 {
            return None;
        }

        match destination {
            Object::Array(array) => array
                .iter::<Object<'_>>()
                .next()
                .and_then(|page| self.page_object_index(page)),
            Object::Dict(dict) => dict
                .get::<Object<'_>>(D)
                .and_then(|nested| self.destination_page(nested, depth + 1)),
            Object::Name(name) => self
                .named_destinations
                .get(name.as_ref())
                .cloned()
                .and_then(|nested| self.destination_page(nested, depth + 1)),
            Object::String(name) => self
                .named_destinations
                .get(name.as_bytes())
                .cloned()
                .and_then(|nested| self.destination_page(nested, depth + 1)),
            Object::Number(number) => usize::try_from(number.as_i64())
                .ok()
                .filter(|page| *page < self.page_indices.len()),
            _ => None,
        }
    }

    fn page_object_index(&self, page: Object<'_>) -> Option<usize> {
        match page {
            Object::Dict(dict) => dict
                .obj_id()
                .and_then(|id| self.page_indices.get(&id).copied()),
            Object::Number(number) => usize::try_from(number.as_i64())
                .ok()
                .filter(|page| *page < self.page_indices.len()),
            _ => None,
        }
    }
}

fn collect_named_destinations<'a>(catalog: &Dict<'a>) -> BTreeMap<Vec<u8>, Object<'a>> {
    let mut destinations = BTreeMap::new();

    if let Some(legacy) = catalog.get::<Dict<'_>>(DESTS) {
        for name in legacy.keys().take(MAX_OUTLINE_ITEMS) {
            if let Some(destination) = legacy.get::<Object<'_>>(name.as_ref()) {
                destinations.insert(name.as_ref().to_vec(), destination);
            }
        }
    }

    if let Some(names) = catalog
        .get::<Dict<'_>>(NAMES)
        .and_then(|names| names.get::<Dict<'_>>(DESTS))
    {
        let mut visited = BTreeSet::new();
        let mut remaining = MAX_OUTLINE_ITEMS;
        collect_name_tree(&names, &mut destinations, &mut visited, &mut remaining, 0);
    }

    destinations
}

fn collect_name_tree<'a>(
    node: &Dict<'a>,
    destinations: &mut BTreeMap<Vec<u8>, Object<'a>>,
    visited: &mut BTreeSet<ObjectIdentifier>,
    remaining: &mut usize,
    depth: usize,
) {
    if depth >= MAX_TREE_DEPTH || *remaining == 0 {
        return;
    }
    if let Some(id) = node.obj_id()
        && !visited.insert(id)
    {
        return;
    }
    *remaining -= 1;

    if let Some(entries) = node.get::<Array<'_>>(NAMES) {
        let mut values = entries.iter::<Object<'_>>();
        while *remaining > 0 {
            let Some(key) = values.next() else { break };
            let Some(value) = values.next() else { break };
            if let Some(key) = destination_key(&key) {
                destinations.insert(key, value);
                *remaining -= 1;
            }
        }
    }

    if let Some(kids) = node.get::<Array<'_>>(KIDS) {
        for child in kids.iter::<Dict<'_>>() {
            collect_name_tree(&child, destinations, visited, remaining, depth + 1);
            if *remaining == 0 {
                break;
            }
        }
    }
}

fn destination_key(object: &Object<'_>) -> Option<Vec<u8>> {
    match object {
        Object::Name(name) => Some(name.as_ref().to_vec()),
        Object::String(value) => Some(value.as_bytes().to_vec()),
        _ => None,
    }
}

fn decode_pdf_text(bytes: &[u8]) -> String {
    if let Some(encoded) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let units = encoded
            .chunks_exact(2)
            .map(|pair| u16::from_be_bytes([pair[0], pair[1]]));
        return char::decode_utf16(units)
            .map(|value| value.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect();
    }
    if let Some(encoded) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        let units = encoded
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]));
        return char::decode_utf16(units)
            .map(|value| value.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect();
    }

    bytes
        .iter()
        .filter_map(|byte| pdf_doc_character(*byte))
        .collect()
}

fn pdf_doc_character(byte: u8) -> Option<char> {
    const SPECIALS: [u16; 33] = [
        0x2022, 0x2020, 0x2021, 0x2026, 0x2014, 0x2013, 0x0192, 0x2044, 0x2039, 0x203A, 0x2212,
        0x2030, 0x201A, 0x201E, 0x201C, 0x201D, 0x2018, 0x2019, 0xFB01, 0xFB02, 0x0141, 0x0152,
        0x0160, 0x0178, 0x017D, 0x0131, 0x0142, 0x0153, 0x0161, 0x017E, 0, 0, 0x20AC,
    ];

    match byte {
        0x00..=0x17 | 0x9D..=0x9E => None,
        0x18..=0x1F => {
            const MARKS: [u32; 8] = [
                0x02D8, 0x02C7, 0x02C6, 0x02D9, 0x02DD, 0x02DB, 0x02DA, 0x02DC,
            ];
            char::from_u32(MARKS[usize::from(byte - 0x18)])
        }
        0x7F..=0x9F => {
            let value = SPECIALS[usize::from(byte - 0x7F)];
            (value != 0).then(|| char::from_u32(u32::from(value)))?
        }
        _ => Some(char::from(byte)),
    }
}
