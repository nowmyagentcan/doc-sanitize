//! Reading one XML part within limits: UTF-8 or UTF-16 only, no DOCTYPE, no entities but the
//! five predefined ones, elements nested at most 256 deep, at most 64 namespace declarations
//! on one element and no text or attribute value over 32 MiB. Names are matched by namespace
//! URI, never by prefix.

use std::borrow::Cow;

use quick_xml::errors::Error as XmlError;
use quick_xml::events::{BytesStart, Event};
use quick_xml::name::{NamespaceError, ResolveResult};
use quick_xml::{NsReader, XmlVersion};

use crate::report::{Error, safe};

const MAX_DEPTH: usize = 256;
const MAX_XMLNS: usize = 64;
const MAX_VALUE: usize = 32 << 20;

/// The UTF-16 text of a part after any byte-order mark, and whether it is big-endian: known
/// from the mark, or from `<?` without one.
pub fn utf16(bytes: &[u8]) -> Option<(&[u8], bool)> {
    match bytes {
        [0xFF, 0xFE, rest @ ..] => Some((rest, false)),
        [0xFE, 0xFF, rest @ ..] => Some((rest, true)),
        [b'<', 0, b'?', 0, ..] => Some((bytes, false)),
        [0, b'<', 0, b'?', ..] => Some((bytes, true)),
        _ => None,
    }
}

/// A part's bytes as text: a UTF-8 or UTF-16 byte-order mark is honoured, UTF-16 without one
/// is recognised from `<?`, and anything that isn't valid UTF-8 or UTF-16 is damage.
pub fn decode<'b>(bytes: &'b [u8], part: &str) -> Result<Cow<'b, str>, Error> {
    if let Some((text, big)) = utf16(bytes) {
        if !text.len().is_multiple_of(2) {
            return Err(Error::bad_xml(part));
        }
        let units = text.as_chunks::<2>().0.iter().map(|&pair| {
            if big {
                u16::from_be_bytes(pair)
            } else {
                u16::from_le_bytes(pair)
            }
        });
        return char::decode_utf16(units)
            .collect::<Result<String, _>>()
            .map(Cow::Owned)
            .map_err(|_| Error::bad_xml(part));
    }
    let body = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    std::str::from_utf8(body)
        .map(Cow::Borrowed)
        .map_err(|_| Error::bad_xml(part))
}

/// Events of one part, each checked against the limits before it is handed out.
pub struct Reader<'a> {
    inner: NsReader<&'a [u8]>,
    part: &'a str,
    depth: usize,
    root: bool,
    /// The namespace URIs met so far, which `stack` refers to by index.
    uris: Vec<String>,
    /// The open elements, outermost first: namespace index and the span of the local name
    /// in `names`. During a `Start` the last one is that element itself.
    stack: Vec<(usize, usize, usize)>,
    names: String,
    /// Whether the last event was a `Start`, so the top of `stack` isn't around it.
    opened: bool,
    /// The namespace index and local name of the element the last `End` closed.
    closed: Option<usize>,
    closed_name: String,
}

impl<'a> Reader<'a> {
    pub fn new(text: &'a str, part: &'a str) -> Self {
        let mut inner = NsReader::from_str(text);
        inner
            .resolver_mut()
            .set_max_namespace_bindings(MAX_DEPTH * MAX_XMLNS);
        Self {
            inner,
            part,
            depth: 0,
            root: false,
            uris: vec![],
            stack: vec![],
            names: String::new(),
            opened: false,
            closed: None,
            closed_name: String::new(),
        }
    }

    fn too_large(&self, what: &str, limit: &str) -> Error {
        Error::too_large(&format!("{what} in part {}", safe(self.part)), limit)
    }

    /// The next event; `Eof` once the root element has closed.
    pub fn event(&mut self) -> Result<Event<'a>, Error> {
        self.opened = false;
        self.closed = None;
        let event = match self.inner.read_event() {
            Ok(e) => e,
            Err(XmlError::Namespace(NamespaceError::TooManyBindings(_))) => {
                return Err(self.too_large("the namespace declaration count of one element", "64"));
            }
            Err(_) => return Err(Error::bad_xml(self.part)),
        };
        match &event {
            Event::DocType(_) => {
                return Err(Error::damaged(
                    "it contains a DOCTYPE declaration, which Office files never use",
                ));
            }
            Event::Start(e) | Event::Empty(e) => {
                if self.depth == 0 && self.root {
                    return Err(Error::bad_xml(self.part));
                }
                self.root = true;
                if self.depth + 1 > MAX_DEPTH {
                    return Err(self.too_large("element nesting", "256 levels"));
                }
                self.attributes(e)?;
                if matches!(event, Event::Start(_)) {
                    self.depth += 1;
                    let uri = self.ns(e);
                    let ns = match self.uris.iter().position(|u| u == uri) {
                        Some(i) => i,
                        None => {
                            self.uris.push(uri.into());
                            self.uris.len() - 1
                        }
                    };
                    let start = self.names.len();
                    self.names.push_str(e.local_name().as_ref());
                    self.stack.push((ns, start, self.names.len()));
                    self.opened = true;
                }
            }
            Event::End(_) => {
                self.depth -= 1;
                if let Some((ns, start, _)) = self.stack.pop() {
                    self.closed_name.clear();
                    self.closed_name.push_str(&self.names[start..]);
                    self.names.truncate(start);
                    self.closed = Some(ns);
                }
            }
            Event::Text(t) if t.len() > MAX_VALUE => {
                return Err(self.too_large("a text value", "32 MiB"));
            }
            Event::CData(t) if t.len() > MAX_VALUE => {
                return Err(self.too_large("a text value", "32 MiB"));
            }
            Event::GeneralRef(r) => {
                let known = ["lt", "gt", "amp", "apos", "quot"].contains(&&**r);
                // XML 1.0 characters: no controls but tab, newline and return.
                let allowed = |c: char| {
                    (c >= ' ' && !matches!(c, '\u{FFFE}' | '\u{FFFF}')) || "\t\n\r".contains(c)
                };
                if !known && !matches!(r.resolve_char_ref(), Ok(Some(c)) if allowed(c)) {
                    return Err(Error::bad_xml(self.part));
                }
            }
            Event::Decl(d) => {
                let encoding = d
                    .encoding()
                    .transpose()
                    .map_err(|_| Error::bad_xml(self.part))?;
                let known = encoding.is_none_or(|e| {
                    ["utf-8", "utf8", "utf-16", "utf16"].contains(&e.to_ascii_lowercase().as_str())
                });
                if !known {
                    return Err(Error::bad_xml(self.part));
                }
            }
            Event::Eof if self.depth > 0 || !self.root => return Err(Error::bad_xml(self.part)),
            _ => {}
        }
        Ok(event)
    }

    /// Checks an element's namespace declarations and attribute values against the limits.
    fn attributes(&self, e: &BytesStart) -> Result<(), Error> {
        let raw = e.attributes_raw();
        if raw.contains("xmlns") {
            let xmlns = e
                .attributes()
                .with_checks(false)
                .flatten()
                .filter(|a| a.key.as_namespace_binding().is_some())
                .count();
            if xmlns > MAX_XMLNS {
                return Err(self.too_large("the namespace declaration count of one element", "64"));
            }
        }
        if raw.contains('&') {
            for a in e.attributes().with_checks(false) {
                let a = a.map_err(|_| Error::bad_xml(self.part))?;
                a.normalized_value(XmlVersion::Implicit1_0)
                    .map_err(|_| Error::bad_xml(self.part))?;
            }
        }
        if raw.len() > MAX_VALUE
            && e.attributes()
                .with_checks(false)
                .flatten()
                .any(|a| a.value.len() > MAX_VALUE)
        {
            return Err(self.too_large("an attribute value", "32 MiB"));
        }
        Ok(())
    }

    /// The namespace URI of an element (empty when it has none).
    pub fn ns(&self, e: &BytesStart) -> &str {
        match self.inner.resolver().resolve_element(e.name()).0 {
            ResolveResult::Bound(ns) => ns.0,
            _ => "",
        }
    }

    /// The elements around the current event, as (namespace, local name), outermost first.
    fn around(&self) -> impl DoubleEndedIterator<Item = (&str, &str)> {
        let n = self.stack.len() - usize::from(self.opened);
        self.stack[..n]
            .iter()
            .map(|&(ns, start, end)| (self.uri(ns), &self.names[start..end]))
    }

    fn uri(&self, i: usize) -> &str {
        &self.uris[i]
    }

    /// How many elements are around the current event: 0 for the root.
    pub fn level(&self) -> usize {
        self.stack.len() - usize::from(self.opened)
    }

    /// Whether an element around the current event is `local` in namespace `ns`.
    pub fn within(&self, ns: &str, local: &str) -> bool {
        self.around().any(|e| e == (ns, local))
    }

    /// The element around the current event, as (namespace, local name).
    pub fn parent(&self) -> Option<(&str, &str)> {
        self.around().next_back()
    }

    /// Whether the current `End` closes `local` in namespace `ns`.
    pub fn closes(&self, ns: &str, local: &str) -> bool {
        self.closed
            .is_some_and(|i| self.uri(i) == ns && self.closed_name == local)
    }

    /// Whether `e` is the element `local` in namespace `ns`.
    pub fn is(&self, e: &BytesStart, ns: &str, local: &str) -> bool {
        e.local_name().as_ref() == local && self.ns(e) == ns
    }

    /// The value of an unprefixed attribute, with references replaced.
    pub fn attr<'e>(&self, e: &'e BytesStart, name: &str) -> Result<Option<Cow<'e, str>>, Error> {
        for a in e.attributes().with_checks(false) {
            let a = a.map_err(|_| Error::bad_xml(self.part))?;
            if a.key.as_ref() == name {
                let value = a.normalized_value(XmlVersion::Implicit1_0);
                return value.map(Some).map_err(|_| Error::bad_xml(self.part));
            }
        }
        Ok(None)
    }

    /// The value of the attribute `local` in namespace `ns`, with references replaced.
    pub fn attr_ns<'e>(
        &self,
        e: &'e BytesStart,
        ns: &str,
        local: &str,
    ) -> Result<Option<Cow<'e, str>>, Error> {
        let mut attrs = self.attrs(e)?.into_iter();
        Ok(attrs.find_map(|(n, l, v)| (n == ns && l == local).then_some(v)))
    }

    /// Every attribute of `e` but namespace declarations, as (namespace, local name, value).
    pub fn attrs<'e>(&self, e: &'e BytesStart) -> Result<Vec<Attr<'e>>, Error> {
        let mut out = vec![];
        for a in e.attributes().with_checks(false) {
            let a = a.map_err(|_| Error::bad_xml(self.part))?;
            if a.key.as_namespace_binding().is_some() {
                continue;
            }
            let (ns, name) = self.inner.resolver().resolve_attribute(a.key);
            let ns = match ns {
                ResolveResult::Bound(ns) => ns.0.to_string(),
                _ => String::new(),
            };
            let value = a
                .normalized_value(XmlVersion::Implicit1_0)
                .map_err(|_| Error::bad_xml(self.part))?;
            out.push((ns, name.as_ref().to_string(), value));
        }
        Ok(out)
    }

    /// Every attribute of `e` as written, namespace declarations included, as (namespace,
    /// qualified name, value with references replaced); a declaration has no namespace.
    pub fn raw_attrs(&self, e: &BytesStart) -> Result<Vec<(String, String, String)>, Error> {
        let mut out = vec![];
        for a in e.attributes().with_checks(false) {
            let a = a.map_err(|_| Error::bad_xml(self.part))?;
            let name = a.key.as_ref().to_string();
            let ns = match a.key.as_namespace_binding() {
                Some(_) => String::new(),
                None => match self.inner.resolver().resolve_attribute(a.key).0 {
                    ResolveResult::Bound(ns) => ns.0.to_string(),
                    _ => String::new(),
                },
            };
            let value = a
                .normalized_value(XmlVersion::Implicit1_0)
                .map_err(|_| Error::bad_xml(self.part))?;
            out.push((ns, name, value.into_owned()));
        }
        Ok(out)
    }

    /// Reads to the end, checking every event.
    pub fn validate(mut self) -> Result<(), Error> {
        while !matches!(self.event()?, Event::Eof) {}
        Ok(())
    }
}

/// Whether an on/off attribute is on.
pub fn yes(v: Option<Cow<str>>) -> bool {
    matches!(v.as_deref(), Some("1" | "true"))
}

/// An attribute as (namespace, local name, value).
pub type Attr<'e> = (String, String, Cow<'e, str>);

/// An element's local name.
pub fn local(e: &BytesStart) -> String {
    e.local_name().as_ref().to_string()
}

/// The text an event stands for: text and CDATA as written, a reference as its character.
pub fn text<'a>(event: &Event<'a>) -> Option<Cow<'a, str>> {
    match event {
        Event::Text(t) => Some(t.xml10_content()),
        Event::CData(t) => Some(t.xml10_content()),
        Event::GeneralRef(r) => {
            let c = match &**r {
                "lt" => '<',
                "gt" => '>',
                "amp" => '&',
                "apos" => '\'',
                "quot" => '"',
                _ => r.resolve_char_ref().ok()??,
            };
            Some(Cow::Owned(c.into()))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(xml: &str) -> Result<(), Error> {
        Reader::new(xml, "word/document.xml").validate()
    }

    fn code(xml: &str) -> &'static str {
        check(xml).map_or_else(|e| e.code, |()| "ok")
    }

    #[test]
    fn well_formed_parts_pass() {
        assert_eq!(
            code(
                "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<a x=\"1 &amp; 2\"><b>&lt;&#169;&#x20AC;</b><c/></a>"
            ),
            "ok"
        );
        assert_eq!(code("<a><![CDATA[x]]><!-- c --><?pi x?></a>"), "ok");
    }

    #[test]
    fn broken_xml_is_damage() {
        for xml in [
            "",
            "<a>",
            "<a></b>",
            "<a/><b/>",
            "<a>&lol;</a>",
            "<a x='&lol;'/>",
            "text",
            "<?xml version=\"1.0\" encoding=\"ISO-8859-1\"?><a/>",
        ] {
            let e = check(xml).unwrap_err();
            assert_eq!(
                e.message, "{name} is damaged: part word/document.xml isn't valid XML.",
                "{xml}"
            );
        }
        let e = check("<a>&#0;</a>").unwrap_err();
        assert_eq!(e.code, "damaged");
    }

    #[test]
    fn a_doctype_is_damage_and_nothing_is_fetched() {
        let e = check("<?xml version=\"1.0\"?><!DOCTYPE a SYSTEM \"file:///etc/passwd\"><a/>")
            .unwrap_err();
        assert_eq!(
            e.message,
            "{name} is damaged: it contains a DOCTYPE declaration, which Office files never use."
        );
        let laughs = "<!DOCTYPE l [<!ENTITY a \"aaaa\"><!ENTITY b \"&a;&a;&a;\">]><l>&b;</l>";
        assert_eq!(code(laughs), "damaged");
    }

    #[test]
    fn limits_are_too_large() {
        let deep = "<a>".repeat(257) + &"</a>".repeat(257);
        let e = check(&deep).unwrap_err();
        assert_eq!(
            e.message,
            "{name} is too large to check (element nesting in part word/document.xml is over 256 levels)."
        );
        let fits = "<a>".repeat(256) + &"</a>".repeat(256);
        assert_eq!(code(&fits), "ok");
        let empty_deep = "<a>".repeat(256) + "<b/>" + &"</a>".repeat(256);
        assert_eq!(code(&empty_deep), "too_large");

        let ns: String = (0..65)
            .map(|i| format!(" xmlns:p{i}=\"urn:{i}\""))
            .collect();
        let e = check(&format!("<a{ns}/>")).unwrap_err();
        assert!(e.message.ends_with("(the namespace declaration count of one element in part word/document.xml is over 64)."), "{}", e.message);
        let ns: String = (0..64)
            .map(|i| format!(" xmlns:p{i}=\"urn:{i}\""))
            .collect();
        assert_eq!(code(&format!("<a{ns}/>")), "ok");

        let big = "x".repeat(MAX_VALUE + 1);
        assert_eq!(code(&format!("<a v=\"{big}\"/>")), "too_large");
        assert_eq!(code(&format!("<a>{big}</a>")), "too_large");
        assert_eq!(code(&format!("<a v=\"{}\"/>", &big[1..])), "ok");
    }

    #[test]
    fn names_resolve_by_namespace_never_by_prefix() {
        let xml =
            "<x:document xmlns:x=\"urn:w\"><t xmlns=\"urn:w\" Id=\"a&amp;b\"/><x:t/></x:document>";
        let mut r = Reader::new(xml, "p");
        let mut seen = vec![];
        loop {
            match r.event().unwrap() {
                Event::Start(e) | Event::Empty(e) => seen.push((
                    r.is(&e, "urn:w", "t"),
                    r.attr(&e, "Id").unwrap().map(Cow::into_owned),
                )),
                Event::Eof => break,
                _ => {}
            }
        }
        assert_eq!(
            seen,
            [(false, None), (true, Some("a&b".into())), (true, None)]
        );
    }

    #[test]
    fn utf16_and_byte_order_marks_decode() {
        let text = "<?xml version=\"1.0\" encoding=\"UTF-16\"?><a>é</a>";
        let le: Vec<u8> = [0xFF, 0xFE]
            .into_iter()
            .chain(text.encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        let be: Vec<u8> = [0xFE, 0xFF]
            .into_iter()
            .chain(text.encode_utf16().flat_map(u16::to_be_bytes))
            .collect();
        let bare: Vec<u8> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
        for bytes in [le, be, bare] {
            let decoded = decode(&bytes, "p").unwrap();
            assert_eq!(decoded, text);
            assert_eq!(Reader::new(&decoded, "p").validate(), Ok(()));
        }
        assert_eq!(decode(b"\xEF\xBB\xBF<a/>", "p").unwrap(), "<a/>");
        assert_eq!(decode(b"<a>\xFF</a>", "p").unwrap_err().code, "damaged");
        assert_eq!(
            decode(&[0xFF, 0xFE, b'<'], "p").unwrap_err().code,
            "damaged"
        );
    }
}
