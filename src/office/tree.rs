//! A small element tree for the parts `clean` edits: read within the XML limits, edited in
//! place and written back with every prefix, declaration and attribute as it was.

use std::iter::once;
use std::rc::Rc;

use quick_xml::events::Event;

use super::xml::{self, Reader};
use crate::report::Error;

/// One part as a tree: what comes before the root (the declaration, comments) and the root.
#[derive(Debug, Clone)]
pub struct Doc {
    pub prolog: String,
    pub root: El,
    /// Whether the part is UTF-16, and big-endian: it is written back as it was read.
    utf16: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct El {
    /// The qualified name as written, e.g. `w:p`.
    pub name: String,
    pub ns: Rc<str>,
    pub attrs: Vec<Attr>,
    pub kids: Vec<Node>,
}

#[derive(Debug, Clone)]
pub struct Attr {
    /// The qualified name as written; `xmlns` declarations are attributes too.
    pub name: String,
    pub ns: Rc<str>,
    pub value: String,
}

#[derive(Debug, Clone)]
pub enum Node {
    El(El),
    Text(String),
    /// A comment or processing instruction as written.
    Raw(String),
}

pub const MC: &str = "http://schemas.openxmlformats.org/markup-compatibility/2006";

impl Doc {
    pub fn parse(bytes: &[u8], part: &str) -> Result<Self, Error> {
        let text = xml::decode(bytes, part)?;
        let utf16 = xml::utf16(bytes).map(|(_, big)| big);
        let mut r = Reader::new(&text, part);
        let mut prolog = String::new();
        let mut open: Vec<El> = vec![];
        let mut uris: Vec<Rc<str>> = vec![];
        let mut intern = |s: &str| match uris.iter().find(|u| &***u == s) {
            Some(u) => u.clone(),
            None => {
                uris.push(s.into());
                uris[uris.len() - 1].clone()
            }
        };
        loop {
            let ev = r.event()?;
            let node = match &ev {
                Event::Eof => return Err(Error::bad_xml(part)),
                Event::Start(e) | Event::Empty(e) => {
                    let attrs = r
                        .raw_attrs(e)?
                        .into_iter()
                        .map(|(ns, name, value)| Attr {
                            name,
                            ns: intern(&ns),
                            value,
                        })
                        .collect();
                    let el = El {
                        name: e.name().as_ref().to_string(),
                        ns: intern(r.ns(e)),
                        attrs,
                        kids: vec![],
                    };
                    if matches!(ev, Event::Start(_)) {
                        open.push(el);
                        continue;
                    }
                    Node::El(el)
                }
                Event::End(_) => {
                    let el = open.pop().ok_or_else(|| Error::bad_xml(part))?;
                    if open.is_empty() {
                        return Ok(Self {
                            prolog,
                            root: el,
                            utf16,
                        });
                    }
                    Node::El(el)
                }
                Event::Comment(c) => Node::Raw(format!("<!--{}-->", &**c)),
                Event::PI(p) => Node::Raw(format!("<?{}?>", &**p)),
                Event::Decl(d) => Node::Raw(format!("<?{}?>", &**d)),
                ev => match xml::text(ev) {
                    Some(t) => Node::Text(t.into_owned()),
                    None => continue,
                },
            };
            match (open.last_mut(), node) {
                (Some(parent), Node::Text(t)) => match parent.kids.last_mut() {
                    Some(Node::Text(last)) => last.push_str(&t),
                    _ => parent.kids.push(Node::Text(t)),
                },
                (Some(parent), node) => parent.kids.push(node),
                (None, Node::El(root)) => {
                    return Ok(Self {
                        prolog,
                        root,
                        utf16,
                    });
                }
                (None, Node::Raw(raw)) => prolog += &raw,
                (None, Node::Text(t)) => prolog += &t,
            }
        }
    }

    pub fn write(&self) -> Vec<u8> {
        let mut out = self.prolog.clone();
        self.root.write(&mut out);
        match self.utf16 {
            None => out.into_bytes(),
            Some(big) => once(0xFEFF)
                .chain(out.encode_utf16())
                .flat_map(|u| if big { u.to_be_bytes() } else { u.to_le_bytes() })
                .collect(),
        }
    }
}

fn escape(s: &str, out: &mut String, attr: bool) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' if attr => out.push_str("&quot;"),
            '\t' if attr => out.push_str("&#9;"),
            '\n' if attr => out.push_str("&#10;"),
            '\r' => out.push_str("&#13;"),
            c => out.push(c),
        }
    }
}

impl El {
    /// A new element named like `like` (same prefix and namespace) with the local name `local`.
    pub fn like(like: &El, local: &str) -> El {
        let name = match like.name.split_once(':') {
            Some((prefix, _)) => format!("{prefix}:{local}"),
            None => local.into(),
        };
        El {
            name,
            ns: like.ns.clone(),
            attrs: vec![],
            kids: vec![],
        }
    }

    pub fn local(&self) -> &str {
        self.name.rsplit(':').next().unwrap_or_default()
    }

    pub fn is(&self, ns: &str, local: &str) -> bool {
        self.local() == local && &*self.ns == ns
    }

    /// An attribute's value by namespace ("" for none) and local name.
    pub fn attr(&self, ns: &str, local: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|a| &*a.ns == ns && a.local() == local && !a.declares())
            .map(|a| a.value.as_str())
    }

    /// Sets an attribute that the element already has.
    pub fn set(&mut self, ns: &str, local: &str, value: &str) {
        if let Some(a) = self
            .attrs
            .iter_mut()
            .find(|a| &*a.ns == ns && a.local() == local)
        {
            a.value = value.into();
        }
    }

    /// Removes the attributes `drop` picks; gives how many.
    pub fn drop_attrs(&mut self, drop: impl Fn(&Attr) -> bool) -> usize {
        let n = self.attrs.len();
        self.attrs.retain(|a| a.declares() || !drop(a));
        n - self.attrs.len()
    }

    pub fn els(&self) -> impl Iterator<Item = &El> {
        self.kids.iter().filter_map(|n| match n {
            Node::El(e) => Some(e),
            _ => None,
        })
    }

    pub fn els_mut(&mut self) -> impl Iterator<Item = &mut El> {
        self.kids.iter_mut().filter_map(|n| match n {
            Node::El(e) => Some(e),
            _ => None,
        })
    }

    pub fn child(&self, ns: &str, local: &str) -> Option<&El> {
        self.els().find(|e| e.is(ns, local))
    }

    pub fn child_mut(&mut self, ns: &str, local: &str) -> Option<&mut El> {
        self.els_mut().find(|e| e.is(ns, local))
    }

    /// Whether this element or one inside it is `pick`.
    pub fn has(&self, pick: &impl Fn(&El) -> bool) -> bool {
        pick(self) || self.els().any(|e| e.has(pick))
    }

    /// Every element inside, depth first, this one first.
    pub fn each(&self, f: &mut impl FnMut(&El)) {
        f(self);
        for e in self.els() {
            e.each(f);
        }
    }

    /// Every element inside, depth first, this one first, to change.
    pub fn each_mut(&mut self, f: &mut impl FnMut(&mut El)) {
        f(self);
        for e in self.els_mut() {
            e.each_mut(f);
        }
    }

    /// The text inside, joined.
    pub fn text(&self) -> String {
        let mut out = String::new();
        self.text_into(&mut out);
        out
    }

    fn text_into(&self, out: &mut String) {
        for n in &self.kids {
            match n {
                Node::Text(t) => out.push_str(t),
                Node::El(e) => e.text_into(out),
                Node::Raw(_) => {}
            }
        }
    }

    /// Removes every element inside that `pick` picks (not looking inside it). One picked
    /// inside an `mc:Choice` or `mc:Fallback` takes its whole `mc:AlternateContent` with it.
    /// Gives how many were picked.
    pub fn prune(&mut self, pick: &impl Fn(&El) -> bool) -> usize {
        let mut n = 0;
        self.kids.retain_mut(|k| {
            let Node::El(e) = k else { return true };
            if pick(e) {
                n += 1;
                return false;
            }
            if e.is(MC, "AlternateContent") && e.els().any(|b| b.has(pick)) {
                n += e.els().map(|b| b.count(pick)).sum::<usize>();
                return false;
            }
            n += e.prune(pick);
            true
        });
        n
    }

    /// How many elements inside `pick` picks (not looking inside them), this one included.
    pub fn count(&self, pick: &impl Fn(&El) -> bool) -> usize {
        if pick(self) {
            1
        } else {
            self.els().map(|e| e.count(pick)).sum()
        }
    }

    /// Replaces every element inside that `pick` picks by its children.
    pub fn unwrap(&mut self, pick: &impl Fn(&El) -> bool) -> usize {
        let mut n = 0;
        let mut kids = Vec::with_capacity(self.kids.len());
        for k in std::mem::take(&mut self.kids) {
            match k {
                Node::El(mut e) => {
                    n += e.unwrap(pick);
                    if pick(&e) {
                        n += 1;
                        kids.extend(e.kids);
                    } else {
                        kids.push(Node::El(e));
                    }
                }
                k => kids.push(k),
            }
        }
        self.kids = kids;
        n
    }

    fn write(&self, out: &mut String) {
        out.push('<');
        out.push_str(&self.name);
        for a in &self.attrs {
            out.push(' ');
            out.push_str(&a.name);
            out.push_str("=\"");
            escape(&a.value, out, true);
            out.push('"');
        }
        if self.kids.is_empty() {
            out.push_str("/>");
            return;
        }
        out.push('>');
        for k in &self.kids {
            match k {
                Node::El(e) => e.write(out),
                Node::Text(t) => escape(t, out, false),
                Node::Raw(r) => out.push_str(r),
            }
        }
        out.push_str("</");
        out.push_str(&self.name);
        out.push('>');
    }
}

impl Attr {
    pub fn local(&self) -> &str {
        self.name.rsplit(':').next().unwrap_or_default()
    }

    /// Whether this is an `xmlns` declaration.
    pub fn declares(&self) -> bool {
        self.name == "xmlns" || self.name.starts_with("xmlns:")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: &str = "urn:w";

    fn doc(xml: &str) -> Doc {
        Doc::parse(xml.as_bytes(), "p").unwrap()
    }

    fn text(d: &Doc) -> String {
        String::from_utf8(d.write()).unwrap()
    }

    #[test]
    fn a_part_writes_back_as_it_was_read() {
        let xml = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<w:a xmlns:w=\"urn:w\" w:x=\"1 &amp; &quot;2&quot;&#10;\"><!-- c --><w:b>x &lt; y &amp; z</w:b><w:c/><?pi d?></w:a>";
        assert_eq!(text(&doc(xml)), xml);
        let d = doc("<a><![CDATA[<b>]]>&#65;</a>");
        assert_eq!(text(&d), "<a>&lt;b&gt;A</a>");
        assert_eq!(d.root.text(), "<b>A");
    }

    #[test]
    fn names_resolve_and_edits_keep_prefixes() {
        let mut d = doc(
            "<x:a xmlns:x=\"urn:w\" xmlns:r=\"urn:r\"><b xmlns=\"urn:w\" r:id=\"rId1\"/><x:c><x:d/></x:c></x:a>",
        );
        let b = d.root.els().next().unwrap();
        assert!(b.is(W, "b"));
        assert_eq!(b.attr("urn:r", "id"), Some("rId1"));
        assert_eq!(d.root.prune(&|e: &El| e.is(W, "d")), 1);
        let e = El::like(&d.root, "e");
        d.root.child_mut(W, "c").unwrap().kids.push(Node::El(e));
        assert_eq!(
            text(&d),
            "<x:a xmlns:x=\"urn:w\" xmlns:r=\"urn:r\"><b xmlns=\"urn:w\" r:id=\"rId1\"/><x:c><x:e/></x:c></x:a>"
        );
        assert_eq!(d.root.unwrap(&|e: &El| e.is(W, "c")), 1);
        assert!(text(&d).ends_with("<x:e/></x:a>"));
    }

    #[test]
    fn pruning_inside_alternate_content_takes_all_of_it() {
        let mut d = doc(&format!(
            "<a xmlns:mc=\"{MC}\" xmlns:w=\"urn:w\"><mc:AlternateContent><mc:Choice Requires=\"x\"><w:p/></mc:Choice><mc:Fallback><w:p/></mc:Fallback></mc:AlternateContent><w:q/></a>"
        ));
        assert_eq!(d.root.prune(&|e: &El| e.is(W, "p")), 2);
        assert_eq!(
            text(&d),
            format!("<a xmlns:mc=\"{MC}\" xmlns:w=\"urn:w\"><w:q/></a>")
        );
    }

    #[test]
    fn limits_still_hold() {
        let deep = "<a>".repeat(257) + &"</a>".repeat(257);
        assert_eq!(
            Doc::parse(deep.as_bytes(), "p").unwrap_err().code,
            "too_large"
        );
        assert_eq!(Doc::parse(b"<a>", "p").unwrap_err().code, "damaged");
    }
}
