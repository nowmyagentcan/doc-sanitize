//! Optional content (layers) that the default configuration switches off, and the pages
//! that carry it. The renderer never draws such content, so this reads the content streams.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use lopdf::content::Operation;
use lopdf::{Dictionary, Object, ObjectId};

use super::{Ctx, page_content, page_resources, walk};
use crate::report::{CheckId, Finding, Where};

/// What the default configuration switches off.
#[derive(Default)]
pub(super) struct Off {
    /// Each layer that is off, with the pages that show it.
    layers: BTreeMap<ObjectId, BTreeSet<usize>>,
    /// The pages whose content (not just annotations) carries such a layer.
    pub content: BTreeSet<usize>,
    /// The /OC objects of annotations that are off.
    pub annotations: HashSet<ObjectId>,
}

/// The layers that are off, and the pages and annotations that carry them.
pub(super) fn off(c: &Ctx) -> Off {
    let mut off = Off::default();
    let Some(props) = c.catalog().and_then(|cat| c.dict(cat, b"OCProperties")) else {
        return off;
    };
    let config = c.dict(props, b"D");
    let ids = |key: &[u8]| -> HashSet<ObjectId> {
        let list = config.map(|d| c.array(d, key)).unwrap_or_default();
        list.iter().filter_map(|o| o.as_reference().ok()).collect()
    };
    let layers = Layers {
        c,
        on: ids(b"ON"),
        off: ids(b"OFF"),
        base_off: config.and_then(|d| super::name(d, b"BaseState")).as_deref() == Some("OFF"),
    };
    for (page, dict) in c.pages() {
        let mut hidden = vec![];
        if let Some(content) = page_content(c, dict) {
            walk(
                c,
                &content,
                page_resources(c, dict),
                &mut |op, resources| {
                    hidden.extend(
                        marked(c, op, resources)
                            .map(|id| layers.off_in(id))
                            .unwrap_or_default(),
                    );
                },
            );
        }
        if !hidden.is_empty() {
            off.content.insert(page);
        }
        for (_, a) in c.annots(dict) {
            if let Ok(id) = a.get(b"OC").and_then(Object::as_reference) {
                let layers = layers.off_in(id);
                if !layers.is_empty() {
                    off.annotations.insert(id);
                }
                hidden.extend(layers);
            }
        }
        for id in hidden {
            off.layers.entry(id).or_default().insert(page);
        }
    }
    off
}

pub(super) fn check(c: &Ctx) -> Vec<Finding> {
    off(c)
        .layers
        .into_iter()
        .map(|(id, pages)| {
            let on = super::pages_text(&pages.iter().copied().collect::<Vec<_>>());
            let name = c
                .doc()
                .get_dictionary(id)
                .ok()
                .and_then(|d| c.text(d.get(b"Name").ok()?));
            c.finding(
                (CheckId::HiddenLayers, "hidden_layer"),
                Where::page(pages.first().copied().unwrap_or(1)),
                (1, "layers"),
                format!("1 layer that is switched off, with content on {on}"),
                name,
            )
        })
        .collect()
}

/// The optional content an operator marks or draws: `/OC /name BDC` through the
/// resources' /Properties, or `/name Do` of an XObject with /OC.
fn marked<'a>(c: &Ctx<'a>, op: &Operation, resources: Option<&'a Dictionary>) -> Option<ObjectId> {
    let (kind, at) = match op.operator.as_str() {
        "BDC" if op.operands.first()?.as_name().ok()? == b"OC" => (b"Properties".as_slice(), 1),
        "Do" => (b"XObject".as_slice(), 0),
        _ => return None,
    };
    let name = op.operands.get(at)?.as_name().ok()?;
    let id = c
        .dict(resources?, kind)?
        .get(name)
        .ok()?
        .as_reference()
        .ok()?;
    if kind == b"Properties" {
        return Some(id);
    }
    let xobject = super::as_dict(c.doc().get_object(id).ok()?)?;
    xobject.get(b"OC").ok()?.as_reference().ok()
}

/// The default configuration's layer states.
struct Layers<'c, 'a> {
    c: &'c Ctx<'a>,
    on: HashSet<ObjectId>,
    off: HashSet<ObjectId>,
    base_off: bool,
}

impl Layers<'_, '_> {
    fn is_off(&self, id: ObjectId) -> bool {
        self.off.contains(&id) || (self.base_off && !self.on.contains(&id))
    }

    /// The OFF layers that hide content marked with `id`: the layer itself, or the members
    /// of a membership dictionary whose policy or visibility expression hides it.
    fn off_in(&self, id: ObjectId) -> Vec<ObjectId> {
        let Ok(d) = self.c.doc().get_dictionary(id) else {
            return vec![];
        };
        if !d.has_type(b"OCMD") {
            return if self.is_off(id) { vec![id] } else { vec![] };
        }
        let members = self.members(d);
        let off: Vec<_> = members
            .iter()
            .copied()
            .filter(|&m| self.is_off(m))
            .collect();
        let policy = super::name(d, b"P");
        let hidden = match policy.as_deref() {
            _ if d.has(b"VE") => !off.is_empty(),
            Some("AllOn") => !off.is_empty(),
            Some("AnyOff") => off.is_empty(),
            Some("AllOff") => off.len() < members.len(),
            _ => !members.is_empty() && off.len() == members.len(),
        };
        // Under AnyOff and AllOff, layers that are on hide the content: name the dictionary.
        match (hidden, policy.as_deref()) {
            (false, _) => vec![],
            (true, Some("AnyOff" | "AllOff")) if !d.has(b"VE") => vec![id],
            (true, _) => off,
        }
    }

    /// The layers a membership dictionary names, in /OCGs or its /VE expression.
    fn members(&self, d: &Dictionary) -> Vec<ObjectId> {
        let mut out = vec![];
        let mut pending: Vec<&Object> = d.get(b"OCGs").into_iter().chain(d.get(b"VE")).collect();
        let mut depth = 0;
        while let Some(o) = pending.pop() {
            depth += 1;
            match o {
                Object::Reference(id) if !out.contains(id) => out.push(*id),
                Object::Array(a) if depth < 1000 => pending.extend(a),
                _ => {}
            }
        }
        out
    }
}
