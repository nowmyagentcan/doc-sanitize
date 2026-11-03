//! Cleaning a PDF: each removed class is cut from a copy of the decrypted object graph, then
//! the copy is written as one revision with no encryption and a new /ID, keeping only the
//! objects the trailer reaches.

use std::collections::HashSet;
use std::hash::{BuildHasher, Hasher};

use lopdf::{Dictionary, Document, Object, ObjectId, Stream, StringFormat};

use super::{COMMENTS, Ctx, Pdf, as_dict, bytes, form_fields, has_value, subtype, xmp_of};
use crate::pictures;
use crate::report::{Change, CheckId, Cleaned, Error, Finding, Warning};

/// Removes the checks in `remove` from `pdf`, whose check found `found`.
pub fn clean(pdf: &Pdf, found: &[Finding], remove: &[CheckId]) -> Result<Cleaned, Error> {
    let c = Ctx { pdf, values: false };
    let has = |id: CheckId| remove.contains(&id) && found.iter().any(|f| f.check == id);
    let blocks = blocking(&c, &has);
    if !blocks.is_empty() {
        return Err(Error::cannot_clean(&blocks));
    }
    let leftovers = remove.contains(&CheckId::LeftoverData);
    let mut copy = Copy {
        doc: pdf.doc.clone(),
        pages: pdf.pages.clone(),
        out: Cleaned {
            variant: "pdf",
            ..Cleaned::default()
        },
    };
    let (macros, links) = (has(CheckId::Macros), has(CheckId::ExternalLinks));
    if macros || links {
        copy.actions(macros, links);
    }
    if has(CheckId::Signatures) {
        copy.signatures(&c);
    }
    if has(CheckId::Comments) {
        copy.comments(found, remove);
    }
    if has(CheckId::EmbeddedFiles) {
        copy.embedded_files();
    }
    if has(CheckId::FormData) {
        copy.form_data(&c);
    }
    if has(CheckId::Metadata) {
        copy.metadata(&c);
    }
    if has(CheckId::ImageMetadata) {
        copy.image_metadata();
    }
    if leftovers && has(CheckId::LeftoverData) {
        copy.unused_resources(&c);
    }
    // Objects nothing reached in the input stay only when leftover data is kept.
    let unused: HashSet<ObjectId> = match leftovers {
        true => HashSet::new(),
        false => {
            let reached = bytes::reachable(&pdf.doc);
            pdf.doc
                .objects
                .keys()
                .filter(|id| !reached.contains(id))
                .copied()
                .collect()
        }
    };
    if pdf.encrypted {
        copy.out.warnings.push(Warning::encryption_removed());
    }
    copy.finish(&unused)
}

/// Why each removed check with findings can't be cleaned: (check, reason, hint).
fn blocking(c: &Ctx, has: &impl Fn(CheckId) -> bool) -> Vec<(CheckId, String, String)> {
    let mut out = vec![];
    let dynamic = c
        .catalog()
        .and_then(|cat| c.get(cat, b"NeedsRendering"))
        .is_some_and(|o| o.as_bool().unwrap_or(false));
    if dynamic && has(CheckId::FormData) {
        out.push((
            CheckId::FormData,
            "it is a dynamic XFA form".into(),
            "Save it as a static form in Adobe Acrobat, or run clean again with --keep form-data."
                .into(),
        ));
    }
    for id in [
        CheckId::HiddenText,
        CheckId::FakeRedactions,
        CheckId::HiddenLayers,
    ] {
        if has(id) {
            out.push((
                id,
                "this version can't remove it yet".into(),
                format!("Run clean again with --keep {}.", id.id()),
            ));
        }
    }
    out.sort_by_key(|b| b.0);
    out
}

/// The copy being cleaned.
struct Copy {
    doc: Document,
    pages: Vec<ObjectId>,
    out: Cleaned,
}

/// Calls `f` on every object of the document and everything inside it, outer ones first.
fn each(doc: &mut Document, f: &mut impl FnMut(&mut Object)) {
    fn visit(o: &mut Object, f: &mut impl FnMut(&mut Object)) {
        f(o);
        match o {
            Object::Array(a) => a.iter_mut().for_each(|o| visit(o, f)),
            Object::Dictionary(d) => d.iter_mut().for_each(|(_, o)| visit(o, f)),
            Object::Stream(s) => s.dict.iter_mut().for_each(|(_, o)| visit(o, f)),
            _ => {}
        }
    }
    doc.objects.values_mut().for_each(|o| visit(o, f));
}

/// Calls `f` on every dictionary of the document, stream dictionaries included.
fn each_dict(doc: &mut Document, f: &mut impl FnMut(&mut Dictionary)) {
    each(doc, &mut |o| match o {
        Object::Dictionary(d) => f(d),
        Object::Stream(s) => f(&mut s.dict),
        _ => {}
    });
}

impl Copy {
    /// The dictionary reached from object `id` through `keys`, following references.
    fn dict_at(&mut self, id: ObjectId, keys: &[&[u8]]) -> Option<&mut Dictionary> {
        let (mut owner, mut rest) = (id, keys);
        'jump: loop {
            let mut d = as_dict(self.doc.get_object(owner).ok()?)?;
            for (i, key) in rest.iter().enumerate() {
                match d.get(key).ok()? {
                    Object::Reference(r) => {
                        (owner, rest) = (*r, &rest[i + 1..]);
                        continue 'jump;
                    }
                    Object::Dictionary(inner) => d = inner,
                    _ => return None,
                }
            }
            break;
        }
        let mut d = match self.doc.get_object_mut(owner).ok()? {
            Object::Dictionary(d) => d,
            Object::Stream(s) => &mut s.dict,
            _ => return None,
        };
        for key in rest {
            d = d.get_mut(key).ok()?.as_dict_mut().ok()?;
        }
        Some(d)
    }

    fn catalog_id(&self) -> Option<ObjectId> {
        self.doc
            .trailer
            .get(b"Root")
            .and_then(Object::as_reference)
            .ok()
    }

    /// Drops the annotations `pick` picks (by page number and dictionary) from every page,
    /// with the pop-ups of those dropped; gives how many went.
    fn drop_annots(&mut self, pick: impl Fn(usize, &Dictionary) -> bool) -> usize {
        let mut gone = 0;
        for (i, &page) in self.pages.clone().iter().enumerate() {
            let Some(list) = self.annots(page) else {
                continue;
            };
            let dicts: Vec<Option<(Option<ObjectId>, Dictionary)>> = list
                .iter()
                .map(|a| {
                    let id = a.as_reference().ok();
                    let d = match id {
                        Some(id) => self.doc.get_dictionary(id).ok().cloned(),
                        None => a.as_dict().ok().cloned(),
                    };
                    d.map(|d| (id, d))
                })
                .collect();
            let picked: HashSet<ObjectId> = dicts
                .iter()
                .flatten()
                .filter(|(_, d)| pick(i + 1, d))
                .filter_map(|(id, _)| *id)
                .collect();
            let popups: HashSet<ObjectId> = dicts
                .iter()
                .flatten()
                .filter(|(_, d)| pick(i + 1, d))
                .filter_map(|(_, d)| d.get(b"Popup").and_then(Object::as_reference).ok())
                .collect();
            let keep: Vec<bool> = dicts
                .iter()
                .map(|e| match e {
                    None => true,
                    Some((id, d)) => {
                        let parent = d.get(b"Parent").and_then(Object::as_reference).ok();
                        !(pick(i + 1, d)
                            || id.is_some_and(|id| popups.contains(&id))
                            || subtype(d) == b"Popup"
                                && parent.is_some_and(|p| picked.contains(&p)))
                    }
                })
                .collect();
            gone += keep.iter().filter(|k| !**k).count();
            if let Some(list) = self.annots_mut(page) {
                let mut k = keep.iter();
                list.retain(|_| *k.next().unwrap_or(&true));
            }
        }
        gone
    }

    /// A page's annotations as listed.
    fn annots(&self, page: ObjectId) -> Option<Vec<Object>> {
        match self.doc.get_dictionary(page).ok()?.get(b"Annots").ok()? {
            Object::Array(a) => Some(a.clone()),
            Object::Reference(r) => self.doc.get_object(*r).ok()?.as_array().ok().cloned(),
            _ => None,
        }
    }

    fn annots_mut(&mut self, page: ObjectId) -> Option<&mut Vec<Object>> {
        let target = match self.doc.get_dictionary(page).ok()?.get(b"Annots").ok()? {
            Object::Reference(r) => Some(*r),
            _ => None,
        };
        match target {
            Some(r) => self.doc.get_object_mut(r).ok()?.as_array_mut().ok(),
            None => self
                .doc
                .get_dictionary_mut(page)
                .ok()?
                .get_mut(b"Annots")
                .ok()?
                .as_array_mut()
                .ok(),
        }
    }

    /// `macros` and `external-links`: every reference to a script, a risky action or a link
    /// to another file is cut wherever it sits (/OpenAction, /A, /AA, /Next); an action
    /// takes the actions chained after it. Link annotations left with no action go.
    fn actions(&mut self, macros: bool, links: bool) {
        let bad = |d: &Dictionary| {
            let s = d.get(b"S").and_then(Object::as_name).unwrap_or_default();
            let file_uri = d
                .get(b"URI")
                .ok()
                .and_then(|u| lopdf::decode_text_string(u).ok())
                .is_some_and(|u| u.to_lowercase().starts_with("file:"));
            (macros
                && matches!(
                    s,
                    b"JavaScript" | b"Launch" | b"ImportData" | b"SubmitForm" | b"RichMediaExecute"
                ))
                || (links && (matches!(s, b"GoToR" | b"GoToE") || (s == b"URI" && file_uri)))
        };
        let ids: HashSet<ObjectId> = self
            .doc
            .objects
            .iter()
            .filter(|(_, o)| as_dict(o).is_some_and(bad))
            .map(|(id, _)| *id)
            .collect();
        let cut = |o: &Object| match o {
            Object::Reference(id) => ids.contains(id),
            Object::Dictionary(d) => bad(d),
            _ => false,
        };
        each(&mut self.doc, &mut |o| match o {
            Object::Array(a) => a.retain(|o| !cut(o)),
            Object::Dictionary(_) | Object::Stream(_) => {
                let d = match o {
                    Object::Stream(s) => &mut s.dict,
                    Object::Dictionary(d) => d,
                    _ => unreachable!(),
                };
                let gone: Vec<Vec<u8>> = d
                    .iter()
                    .filter(|(_, v)| cut(v))
                    .map(|(k, _)| k.clone())
                    .collect();
                for k in gone {
                    d.remove(&k);
                }
                if macros {
                    d.remove(b"JavaScript");
                }
            }
            _ => {}
        });
        self.drop_annots(|_, a| subtype(a) == b"Link" && !a.has(b"A") && !a.has(b"Dest"));
    }

    /// `signatures`: signature values and usage rights go.
    fn signatures(&mut self, c: &Ctx) {
        for f in form_fields(c) {
            if f.kind == b"Sig"
                && let Some(Ok(d)) = f.id.map(|id| self.doc.get_dictionary_mut(id))
            {
                d.remove(b"V");
            }
        }
        if let Some(cat) = self.catalog_id() {
            if let Ok(d) = self.doc.get_dictionary_mut(cat) {
                d.remove(b"Perms");
            }
            if let Some(form) = self.dict_at(cat, &[b"AcroForm"]) {
                form.remove(b"SigFlags");
            }
        }
        self.out.changes.push(Change::new(
            "signatures_removed",
            "The digital signature was removed; no signature survives a change.",
        ));
    }

    /// `comments`: comment annotations go with their pop-ups (replies are comments too). One
    /// that covers text found by a kept check stays, so removing it never uncovers text, and
    /// comments then count as kept.
    fn comments(&mut self, found: &[Finding], remove: &[CheckId]) {
        let covered: Vec<(usize, [f64; 4])> = found
            .iter()
            .filter(|f| matches!(f.check, CheckId::HiddenText | CheckId::FakeRedactions))
            .filter(|f| !remove.contains(&f.check))
            .filter_map(|f| Some((f.place.page?, f.place.bbox?)))
            .collect();
        let rect = |a: &Dictionary| -> Option<[f64; 4]> {
            let r = a.get(b"Rect").and_then(Object::as_array).ok()?;
            let n: Vec<f64> = r
                .iter()
                .filter_map(|n| n.as_float().ok())
                .map(f64::from)
                .collect();
            let [x0, y0, x1, y1] = n[..] else { return None };
            Some([x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)])
        };
        let covers = |page: usize, a: &Dictionary| {
            rect(a).is_some_and(|r| {
                covered.iter().any(|(p, b)| {
                    *p == page && r[0] < b[2] && b[0] < r[2] && r[1] < b[3] && b[1] < r[3]
                })
            })
        };
        let stayed = std::cell::Cell::new(false);
        self.drop_annots(|page, a| {
            let comment = COMMENTS.contains(&subtype(a));
            let stays = comment && covers(page, a);
            stayed.set(stayed.get() || stays);
            comment && !stays
        });
        if stayed.get() {
            self.out.kept.push(CheckId::Comments);
        }
    }

    /// `embedded-files`: attachments, file annotations, associated files and portfolios.
    fn embedded_files(&mut self) {
        each_dict(&mut self.doc, &mut |d| {
            d.remove(b"EmbeddedFiles");
            d.remove(b"AF");
            if d.has_type(b"Catalog") {
                d.remove(b"Collection");
            }
        });
        self.drop_annots(|_, a| subtype(a) == b"FileAttachment");
    }

    /// `form-data`: the XFA form, and values no page shows (with their widgets' appearances).
    fn form_data(&mut self, c: &Ctx) {
        let shown: HashSet<ObjectId> = c
            .pages()
            .flat_map(|(_, p)| c.annots(p))
            .filter_map(|(id, _)| id)
            .collect();
        for f in form_fields(c) {
            let hidden = f.widgets.iter().all(|(id, w)| {
                let flags = w.get(b"F").and_then(Object::as_i64).unwrap_or(0);
                flags & (2 | 32) != 0 || !id.is_some_and(|id| shown.contains(&id))
            });
            if f.kind == b"Sig" || !has_value(&f) || !hidden {
                continue;
            }
            let Some(Ok(d)) = f.id.map(|id| self.doc.get_dictionary_mut(id)) else {
                continue;
            };
            let own = d.remove(b"V").is_some();
            d.remove(b"RV");
            d.remove(b"AP");
            if !own {
                // The value is inherited: an empty one of its own hides it.
                let empty = match f.kind {
                    b"Btn" => Object::Name(b"Off".to_vec()),
                    _ => Object::string_literal(""),
                };
                d.set("V", empty);
            }
            for (id, _) in &f.widgets {
                if let Some(Ok(w)) = id.map(|id| self.doc.get_dictionary_mut(id)) {
                    w.remove(b"AP");
                }
            }
        }
        if let Some(cat) = self.catalog_id()
            && let Some(form) = self.dict_at(cat, &[b"AcroForm"])
        {
            form.remove(b"XFA");
        }
    }

    /// `metadata`: /Info, every /Metadata, /PieceInfo, /LastModified and page thumbnails. A
    /// PDF/A file keeps a packet with only its PDF/A (and PDF/UA) identification.
    fn metadata(&mut self, c: &Ctx) {
        let ids: Vec<(String, String)> = c
            .catalog()
            .and_then(|cat| c.get(cat, b"Metadata"))
            .and_then(xmp_of)
            .unwrap_or_default()
            .into_iter()
            .filter(|(k, _)| {
                matches!(
                    k.as_str(),
                    "pdfaid:part" | "pdfaid:conformance" | "pdfuaid:part"
                )
            })
            .collect();
        self.doc.trailer.remove(b"Info");
        each_dict(&mut self.doc, &mut |d| {
            d.remove(b"Metadata");
            d.remove(b"PieceInfo");
            d.remove(b"LastModified");
            if d.has_type(b"Page") {
                d.remove(b"Thumb");
            }
        });
        if !ids.iter().any(|(k, _)| k == "pdfaid:part") {
            return;
        }
        let attrs: String = ids
            .iter()
            .map(|(k, v)| format!(" {k}=\"{}\"", v.replace(['"', '<', '&'], "")))
            .collect();
        let packet = format!(
            "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?><x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description rdf:about=\"\" xmlns:pdfaid=\"http://www.aiim.org/pdfa/ns/id/\" xmlns:pdfuaid=\"http://www.aiim.org/pdfua/ns/id/\"{attrs}/></rdf:RDF></x:xmpmeta><?xpacket end=\"w\"?>"
        );
        let mut dict = Dictionary::new();
        dict.set("Type", Object::Name(b"Metadata".to_vec()));
        dict.set("Subtype", Object::Name(b"XML".to_vec()));
        let stream = Stream::new(dict, packet.into_bytes()).with_compression(false);
        let id = self.doc.add_object(stream);
        if let Some(cat) = self.catalog_id()
            && let Ok(d) = self.doc.get_dictionary_mut(cat)
        {
            d.set("Metadata", id);
        }
        if ids.iter().any(|(k, _)| k == "pdfuaid:part") {
            self.out.warnings.push(Warning::pdfa_title_removed());
        }
    }

    /// `image-metadata`: JPEG streams lose their metadata, the entropy-coded data kept.
    fn image_metadata(&mut self) {
        for o in self.doc.objects.values_mut() {
            let Object::Stream(s) = o else { continue };
            if subtype(&s.dict) != b"Image" || s.filters().unwrap_or_default() != [b"DCTDecode"] {
                continue;
            }
            if pictures::metadata(&s.content).is_some_and(|l| !l.is_empty())
                && let Some(plain) = pictures::strip(&s.content, false)
            {
                s.set_content(plain);
            }
        }
    }

    /// `leftover-data`: XObjects that a page or form names and never draws are dropped from
    /// its resources. A page gets resources of its own, since another page may draw them.
    fn unused_resources(&mut self, c: &Ctx) {
        for (form, names) in super::undrawn_in_forms(c) {
            if let Some(d) = self.dict_at(form, &[b"Resources", b"XObject"]) {
                names.iter().for_each(|n| _ = d.remove(n));
            }
        }
        for (page, resources) in super::undrawn_on_pages(c) {
            if let Ok(d) = self.doc.get_dictionary_mut(page) {
                d.set("Resources", resources);
            }
        }
    }

    /// Writes the copy: the trailer keeps /Root (and /Info when metadata is kept) with a new
    /// /ID, objects nothing reaches go (but those in `unused`, kept leftovers of the input),
    /// then the objects are renumbered and streams compressed.
    fn finish(mut self, unused: &HashSet<ObjectId>) -> Result<Cleaned, Error> {
        let mut trailer = Dictionary::new();
        for key in [b"Root".as_slice(), b"Info"] {
            if let Ok(v) = self.doc.trailer.get(key) {
                trailer.set(key, v.clone());
            }
        }
        let id = Object::String(new_id(), StringFormat::Hexadecimal);
        trailer.set("ID", vec![id.clone(), id]);
        self.doc.trailer = trailer;
        self.doc.encryption_state = None;
        let reached = bytes::reachable(&self.doc);
        self.doc
            .objects
            .retain(|id, _| reached.contains(id) || unused.contains(id));
        self.doc.renumber_objects();
        self.doc.compress();
        let mut bytes = vec![];
        self.doc
            .save_to(&mut bytes)
            .map_err(|_| Error::damaged("its structure can't be written back"))?;
        self.out.bytes = bytes;
        Ok(self.out)
    }
}

/// 16 bytes no other file shares.
fn new_id() -> Vec<u8> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    (0..2)
        .flat_map(|i| {
            let mut h = std::collections::hash_map::RandomState::new().build_hasher();
            h.write_u128(now + i);
            h.finish().to_be_bytes()
        })
        .collect()
}
