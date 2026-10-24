//! Contacts on retifs: the `type=contact` records of ADR-0024, in the user's
//! own namespace `me` (their address book, the same records ADR-0020 uses to
//! share with people) and in any library holding contacts (a club's shared
//! address book). Front ends (GTK now, others later) share this.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use libretifs::{AttrValue, Client, RecordView, Request, Response};

/// The user's own namespace: their address book.
pub const ME: &str = "me";
pub const TYPE: &str = "contact";

/// The labelled kinds of value a contact has several of (`email:work`).
pub const MULTI: [&str; 3] = ["email", "phone", "address"];
/// The single values, in the order a card shows them.
pub const FIELDS: [(&str, &str); 8] = [
    ("name", "Name"),
    ("given", "First name"),
    ("family", "Last name"),
    ("org", "Organisation"),
    ("role", "Role"),
    ("birthday", "Birthday"),
    ("url", "Website"),
    ("note", "Note"),
];

/// A contact: where it lives, its record (none until saved), its values.
/// Keys other than the ones this app edits (`retifs:node`, `lxmf`, an app's
/// own) are kept as they are.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Contact {
    pub ns: String,
    pub id: Option<String>,
    pub values: BTreeMap<String, String>,
}

impl Contact {
    pub fn new(ns: &str) -> Contact {
        Contact { ns: ns.into(), ..Default::default() }
    }

    fn from_record(ns: &str, r: &RecordView) -> Contact {
        let values = r
            .attrs
            .iter()
            .filter_map(|(k, v)| match v {
                AttrValue::Str(s) => Some((k.clone(), s.clone())),
                _ => None,
            })
            .collect();
        Contact { ns: ns.into(), id: Some(r.id.clone()), values }
    }

    pub fn get(&self, key: &str) -> &str {
        self.values.get(key).map(String::as_str).unwrap_or("")
    }

    /// The name to show: the petname, else first and last name, else an
    /// address or number, else "No name".
    pub fn display_name(&self) -> String {
        let full = [self.get("given"), self.get("family")].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join(" ");
        [self.get("name").to_string(), full, self.get("email").to_string(), self.get("phone").to_string()]
            .into_iter()
            .find(|s| !s.is_empty())
            .unwrap_or_else(|| "No name".into())
    }

    /// A line under the name: organisation, else the first address or number.
    pub fn summary(&self) -> String {
        [self.get("org"), self.get("email"), self.get("phone")].into_iter().find(|s| !s.is_empty()).unwrap_or("").to_string()
    }

    /// Values of a labelled kind: (label, value), the unlabelled one first
    /// (label "").
    pub fn multi(&self, kind: &str) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = self
            .values
            .iter()
            .filter_map(|(k, v)| {
                if k == kind {
                    Some((String::new(), v.clone()))
                } else {
                    k.strip_prefix(kind).and_then(|r| r.strip_prefix(':')).map(|l| (l.to_string(), v.clone()))
                }
            })
            .collect();
        out.sort_by(|a, b| (!a.0.is_empty(), &a.0).cmp(&(!b.0.is_empty(), &b.0)));
        out
    }

    /// Replaces the values of a labelled kind.
    pub fn set_multi(&mut self, kind: &str, items: &[(String, String)]) {
        self.values.retain(|k, _| !(k == kind || k.starts_with(&format!("{kind}:"))));
        let mut used: Vec<String> = Vec::new();
        for (label, value) in items {
            let value = value.trim();
            if value.is_empty() {
                continue;
            }
            let label = label.trim().to_lowercase().replace([' ', ':'], "-");
            let mut key = if label.is_empty() { kind.to_string() } else { format!("{kind}:{label}") };
            // Two of one label: the second gets a number.
            let mut n = 2;
            while used.contains(&key) {
                key = format!("{kind}:{}{n}", if label.is_empty() { "other" } else { &label });
                n += 1;
            }
            used.push(key.clone());
            self.values.insert(key, value.to_string());
        }
    }

    pub fn set(&mut self, key: &str, value: &str) {
        let value = value.trim();
        if value.is_empty() {
            self.values.remove(key);
        } else {
            self.values.insert(key.into(), value.into());
        }
    }

    /// Whether a search matches it: every word somewhere in its values.
    pub fn matches(&self, search: &str) -> bool {
        let text = self.values.values().map(|v| v.to_lowercase()).collect::<Vec<_>>().join(" ");
        search.split_whitespace().all(|w| text.contains(&w.to_lowercase()))
    }

    /// Initials for an avatar.
    pub fn initials(&self) -> String {
        self.display_name().split_whitespace().filter_map(|w| w.chars().next()).take(2).collect::<String>().to_uppercase()
    }
}

/// A record that points at a contact: a photo it's in, an event it's at.
#[derive(Debug, Clone, PartialEq)]
pub struct Related {
    pub ns: String,
    pub id: String,
    /// `image`, `event`, or the record's `type`.
    pub kind: String,
    pub title: String,
    /// When: an event's start, a photo's `taken`.
    pub when: String,
}

impl Related {
    fn from_record(ns: &str, r: &RecordView) -> Related {
        let get = |k: &str| {
            r.attrs.iter().find(|(key, _)| key == k).and_then(|(_, v)| match v {
                AttrValue::Str(s) => Some(s.clone()),
                _ => None,
            })
        };
        // Its title, else the name it has in the library's tree, else the file's own name.
        let title = get("title").or_else(|| get("path").map(|p| p.rsplit('/').next().unwrap_or(&p).to_string())).or_else(|| get("name")).unwrap_or_else(|| r.id.clone());
        Related { ns: ns.into(), id: r.id.clone(), kind: get("type").unwrap_or_default(), title, when: get("start").or_else(|| get("taken")).unwrap_or_default() }
    }

    pub fn is_photo(&self) -> bool {
        self.kind == "image" || self.kind == "photo"
    }
}

/// The connection to `retifsd`: a small runtime for libretifs' async client.
pub struct Book {
    rt: tokio::runtime::Runtime,
    store: PathBuf,
}

impl Default for Book {
    fn default() -> Self {
        Self::new()
    }
}

impl Book {
    /// The daemon of the default store (`$RETIFS_STORE`, else the platform default).
    pub fn new() -> Self {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("a tokio runtime");
        Book { rt, store: libretifs::default_store_dir() }
    }

    fn client(&self) -> Result<Client, String> {
        self.rt.block_on(Client::connect(&self.store)).map_err(|e| e.to_string())
    }

    fn call(&self, c: &mut Client, req: Request) -> Result<Response, String> {
        match self.rt.block_on(c.call(req)).map_err(|e| e.to_string())? {
            Response::Error { message } => Err(message),
            r => Ok(r),
        }
    }

    /// The libraries this device has: `me` first, then the rest by name.
    pub fn libraries(&self) -> Result<Vec<String>, String> {
        let mut c = self.client()?;
        let mut names = match self.call(&mut c, Request::Namespaces)? {
            Response::Names { names } => names,
            _ => Vec::new(),
        };
        names.retain(|n| n != ME && !n.starts_with("ch-"));
        names.sort_by_key(|n| n.to_lowercase());
        names.insert(0, ME.into());
        Ok(names)
    }

    /// The contacts in `libraries`, by name.
    pub fn contacts(&self, libraries: &[String]) -> Result<Vec<Contact>, String> {
        let mut c = self.client()?;
        let mut out = Vec::new();
        for ns in libraries {
            let req = Request::Query { ns: ns.clone(), terms: vec![format!("type={TYPE}")], at: None };
            match self.call(&mut c, req) {
                Ok(Response::Records { records }) => out.extend(records.iter().map(|r| Contact::from_record(ns, r))),
                Ok(_) => {}
                // A library not here yet (`me` before anything was saved): none.
                Err(e) if e.contains("no namespace") => {}
                Err(e) => return Err(format!("{ns}: {e}")),
            }
        }
        out.sort_by_cached_key(|c| c.display_name().to_lowercase());
        Ok(out)
    }

    /// Saves a contact: a new record, or what changed in its record (keys it
    /// no longer has are unset). Returns it as saved.
    pub fn save(&self, contact: &Contact, before: Option<&Contact>) -> Result<Contact, String> {
        let mut c = self.client()?;
        let attrs: Vec<(String, AttrValue)> = contact
            .values
            .iter()
            .filter(|(k, v)| before.is_none_or(|b| b.values.get(*k) != Some(*v)))
            .map(|(k, v)| (k.clone(), AttrValue::Str(v.clone())))
            .collect();
        let req = match &contact.id {
            Some(id) => {
                let unset = before.map(|b| b.values.keys().filter(|k| !contact.values.contains_key(*k)).cloned().collect()).unwrap_or_default();
                Request::RecordSet { ns: contact.ns.clone(), id: id.clone(), attrs, unset, data: None }
            }
            None => {
                let mut attrs = attrs;
                attrs.push(("type".into(), AttrValue::Str(TYPE.into())));
                Request::RecordCreate { ns: contact.ns.clone(), attrs, data: None }
            }
        };
        match self.call(&mut c, req)? {
            Response::Records { records } => records.first().map(|r| Contact::from_record(&contact.ns, r)).ok_or_else(|| "nothing saved".into()),
            other => Err(format!("unexpected answer: {other:?}")),
        }
    }

    pub fn delete(&self, contact: &Contact) -> Result<(), String> {
        let Some(id) = &contact.id else { return Ok(()) };
        let mut c = self.client()?;
        self.call(&mut c, Request::RecordDelete { ns: contact.ns.clone(), id: id.clone() }).map(|_| ())
    }

    /// Records elsewhere that point at this contact (ADR-0024 references):
    /// photos it's in (`person:<id>`) and events it's at (`attendee:<id>`),
    /// in any library. Newest first by date.
    pub fn related(&self, contact_id: &str, libraries: &[String]) -> Vec<Related> {
        let Ok(mut c) = self.client() else { return Vec::new() };
        let mut out = Vec::new();
        for ns in libraries {
            for key in ["person", "attendee"] {
                let req = Request::Query { ns: ns.clone(), terms: vec![format!("has:{key}:{contact_id}")], at: None };
                if let Ok(Response::Records { records }) = self.call(&mut c, req) {
                    out.extend(records.iter().map(|r| Related::from_record(ns, r)));
                }
            }
        }
        out.sort_by(|a, b| b.when.cmp(&a.when));
        out.dedup_by(|a, b| a.id == b.id);
        out
    }

    /// A record's content, whole (a profile photo), wherever among
    /// `libraries` the record is. At most `max` bytes.
    pub fn content_of(&self, record_id: &str, libraries: &[String], max: u64) -> Option<Vec<u8>> {
        let mut c = self.client().ok()?;
        for ns in libraries {
            let Ok(Response::Records { records }) = self.call(&mut c, Request::RecordGet { ns: ns.clone(), id: record_id.into() }) else { continue };
            let Some(cap) = records.first().and_then(|r| r.blocks.first()).cloned() else { continue };
            let size: u64 = cap.rsplit(':').next().and_then(|s| s.parse().ok()).unwrap_or(max);
            if size > max {
                return None;
            }
            let req = Request::ReadRange { cap, ns: Some(ns.clone()), offset: 0, len: size, wait_ms: 30_000 };
            if let Ok(Response::Range { data }) = self.call(&mut c, req) {
                return Some(data);
            }
        }
        None
    }

    /// Each library's change count: a front end reloads when these differ.
    pub fn change_counts(&self, libraries: &[String]) -> HashMap<String, u64> {
        let Ok(mut c) = self.client() else { return HashMap::new() };
        libraries
            .iter()
            .filter_map(|ns| match self.rt.block_on(c.call(Request::Watch { ns: ns.clone(), since: 0, timeout_ms: 0 })) {
                Ok(Response::Changed { count }) => Some((ns.clone(), count)),
                _ => None,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labelled_values() {
        let mut c = Contact::new(ME);
        c.set_multi("email", &[("".into(), "a@x.ee".into()), ("Work".into(), "a@work.ee".into()), ("work".into(), "b@work.ee".into()), ("home".into(), " ".into())]);
        assert_eq!(c.get("email"), "a@x.ee");
        assert_eq!(c.get("email:work"), "a@work.ee");
        assert_eq!(c.get("email:work2"), "b@work.ee");
        assert_eq!(c.multi("email").len(), 3);
        assert_eq!(c.multi("email")[0], ("".to_string(), "a@x.ee".to_string()));
        c.set_multi("email", &[("home".into(), "h@x.ee".into())]);
        assert_eq!(c.multi("email"), [("home".to_string(), "h@x.ee".to_string())]);
    }

    #[test]
    fn names_and_search() {
        let mut c = Contact::new(ME);
        assert_eq!(c.display_name(), "No name");
        c.set("given", "Mari");
        c.set("family", "Maasikas");
        assert_eq!(c.display_name(), "Mari Maasikas");
        assert_eq!(c.initials(), "MM");
        c.set("name", "Ema");
        assert_eq!(c.display_name(), "Ema");
        c.set("org", "Prangli kool");
        assert!(c.matches("prangli ema") && !c.matches("tallinn"));
        c.set("org", "  ");
        assert_eq!(c.get("org"), "");
    }
}
