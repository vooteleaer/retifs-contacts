//! retifs Contacts for Linux: native GTK 4 / libadwaita over the shared core.
//! A list of the contacts in the user's address book (`me`) and their other
//! libraries, searchable; a card to read and edit one (ADR-0024 keys). It
//! updates as the libraries change, from this device or another.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::{gio, glib};
use retifs_contacts_core::{Book, Contact, FIELDS, ME, MULTI};

fn main() -> glib::ExitCode {
    let app = adw::Application::builder().application_id("org.retifs.Contacts").build();
    app.connect_activate(build);
    app.run()
}

/// Runs `work` on a worker thread (daemon calls block), then `then` with its
/// result on the UI thread.
fn bg<T: Send + 'static>(book: &Arc<Book>, work: impl FnOnce(&Book) -> T + Send + 'static, then: impl FnOnce(T) + 'static) {
    let book = book.clone();
    glib::spawn_future_local(async move {
        if let Ok(r) = gio::spawn_blocking(move || work(&book)).await {
            then(r);
        }
    });
}

/// Labels offered for addresses and numbers.
const LABELS: [&str; 5] = ["", "home", "work", "mobile", "other"];

/// The card being edited: the contact as loaded, and its editors.
struct Card {
    before: Option<Contact>,
    ns: String,
    fields: Vec<(&'static str, adw::EntryRow)>,
    multi: Vec<(&'static str, Rc<RefCell<Vec<(gtk::DropDown, adw::EntryRow)>>>)>,
    library: adw::ComboRow,
}

struct App {
    book: Arc<Book>,
    window: adw::ApplicationWindow,
    list: gtk::ListBox,
    search: gtk::SearchEntry,
    scope: gtk::DropDown,
    libraries: RefCell<Vec<String>>,
    contacts: RefCell<Vec<Contact>>,
    card_bin: adw::Bin,
    title: adw::WindowTitle,
    save: gtk::Button,
    delete: gtk::Button,
    card: RefCell<Option<Card>>,
    counts: RefCell<HashMap<String, u64>>,
    status: gtk::Label,
}

fn build(gapp: &adw::Application) {
    let book = Arc::new(Book::new());

    // Sidebar: library, search, list.
    let scope = gtk::DropDown::from_strings(&["All libraries"]);
    scope.set_hexpand(true);
    let search = gtk::SearchEntry::builder().placeholder_text("Search contacts").build();
    let list = gtk::ListBox::new();
    list.add_css_class("navigation-sidebar");
    let status = gtk::Label::builder().label("").xalign(0.0).margin_start(12).margin_bottom(6).build();
    status.add_css_class("dim-label");
    status.add_css_class("caption");
    let side_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let controls = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(6).margin_start(12).margin_end(12).margin_top(6).build();
    controls.append(&scope);
    controls.append(&search);
    side_box.append(&controls);
    side_box.append(&gtk::ScrolledWindow::builder().child(&list).hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).build());
    side_box.append(&status);
    let add = gtk::Button::from_icon_name("list-add-symbolic");
    add.set_tooltip_text(Some("New contact"));
    let side_header = adw::HeaderBar::new();
    side_header.pack_start(&add);
    let side_view = adw::ToolbarView::new();
    side_view.add_top_bar(&side_header);
    side_view.set_content(Some(&side_box));
    let side_page = adw::NavigationPage::builder().title("Contacts").child(&side_view).build();

    // Content: the card.
    let title = adw::WindowTitle::new("Contacts", "");
    let save = gtk::Button::builder().label("Save").sensitive(false).build();
    save.add_css_class("suggested-action");
    let delete = gtk::Button::from_icon_name("user-trash-symbolic");
    delete.set_tooltip_text(Some("Delete this contact"));
    delete.set_sensitive(false);
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&title));
    header.pack_end(&save);
    header.pack_end(&delete);
    let card_bin = adw::Bin::new();
    card_bin.set_child(Some(&empty_page()));
    let content = adw::ToolbarView::new();
    content.add_top_bar(&header);
    content.set_content(Some(&card_bin));
    let content_page = adw::NavigationPage::builder().title("Contact").child(&content).build();

    let split = adw::NavigationSplitView::builder().sidebar(&side_page).content(&content_page).build();
    let window = adw::ApplicationWindow::builder().application(gapp).title("Contacts").default_width(900).default_height(640).content(&split).build();

    let me = Rc::new(App {
        book,
        window: window.clone(),
        list: list.clone(),
        search: search.clone(),
        scope: scope.clone(),
        libraries: RefCell::new(vec![ME.into()]),
        contacts: RefCell::new(Vec::new()),
        card_bin,
        title,
        save: save.clone(),
        delete: delete.clone(),
        card: RefCell::new(None),
        counts: RefCell::new(HashMap::new()),
        status,
    });

    {
        let me = me.clone();
        search.connect_search_changed(move |_| show_list(&me));
    }
    {
        let me = me.clone();
        scope.connect_selected_notify(move |_| reload(&me));
    }
    {
        let me = me.clone();
        list.connect_row_activated(move |_, row| {
            let i = row.index() as usize;
            let c = me.visible().get(i).cloned();
            if let Some(c) = c {
                open_card(&me, Some(c));
            }
        });
    }
    {
        let me = me.clone();
        add.connect_clicked(move |_| open_card(&me, None));
    }
    {
        let me = me.clone();
        save.connect_clicked(move |_| save_card(&me));
    }
    {
        let me = me.clone();
        delete.connect_clicked(move |_| delete_card(&me));
    }

    // Libraries, then contacts; again when a library shown changes.
    let me2 = me.clone();
    bg(&me.book, |b| b.libraries(), move |r| {
        match r {
            Ok(libs) => set_libraries(&me2, libs),
            Err(e) => me2.status.set_text(&format!("retifsd: {e}")),
        }
        reload(&me2);
        open_on_start(&me2);
    });
    watch_changes(&me, Arc::new(Book::new()), HashMap::new());
    snapshot_on_request(&window);
    window.present();
}

impl App {
    /// The libraries the list shows: the chosen one, or all.
    fn scope(&self) -> Vec<String> {
        let libs = self.libraries.borrow();
        match self.scope.selected() {
            0 => libs.clone(),
            i => libs.get(i as usize - 1).cloned().into_iter().collect(),
        }
    }

    /// The contacts the search lets through, in list order.
    fn visible(&self) -> Vec<Contact> {
        let q = self.search.text().to_string();
        self.contacts.borrow().iter().filter(|c| c.matches(&q)).cloned().collect()
    }
}

fn set_libraries(me: &Rc<App>, libs: Vec<String>) {
    let mut items = vec!["All libraries".to_string()];
    items.extend(libs.iter().map(|l| if l == ME { "Mine (me)".into() } else { l.clone() }));
    let model = gtk::StringList::new(&items.iter().map(String::as_str).collect::<Vec<_>>());
    *me.libraries.borrow_mut() = libs;
    me.scope.set_model(Some(&model));
    me.scope.set_selected(0);
}

fn reload(me: &Rc<App>) {
    let scope = me.scope();
    let me2 = me.clone();
    bg(&me.book, move |b| (b.contacts(&scope), b.change_counts(&scope)), move |(r, counts)| {
        *me2.counts.borrow_mut() = counts;
        match r {
            Ok(list) => *me2.contacts.borrow_mut() = list,
            Err(e) => me2.status.set_text(&e),
        }
        show_list(&me2);
    });
}

/// Reloads when a library shown changes: retifsd says so the moment it
/// does (`Book::wait_change`), waited for on a `Book` of its own so
/// nothing else is held up meanwhile.
fn watch_changes(me: &Rc<App>, watcher: Arc<Book>, mut known: HashMap<String, u64>) {
    // The libraries shown now, a newly shown one from the count of the last load.
    let scope = me.scope();
    known.retain(|ns, _| scope.contains(ns));
    for (ns, n) in me.counts.borrow().iter() {
        if scope.contains(ns) {
            known.entry(ns.clone()).or_insert(*n);
        }
    }
    let (me2, w) = (me.clone(), watcher.clone());
    bg(&watcher, move |b| {
        let changed = b.wait_change(&mut known, std::time::Duration::from_secs(60));
        (changed, known)
    }, move |(changed, known)| {
        if changed {
            reload(&me2);
        }
        watch_changes(&me2, w, known);
    });
}

fn show_list(me: &Rc<App>) {
    me.list.remove_all();
    let visible = me.visible();
    let shown_id = me.card.borrow().as_ref().and_then(|c| c.before.as_ref().and_then(|b| b.id.clone()));
    for c in &visible {
        let row = adw::ActionRow::builder().title(glib::markup_escape_text(&c.display_name())).subtitle(glib::markup_escape_text(&subtitle(c))).activatable(true).build();
        let avatar = adw::Avatar::new(32, Some(&c.display_name()), true);
        row.add_prefix(&avatar);
        me.list.append(&row);
        if c.id.is_some() && c.id == shown_id {
            me.list.select_row(Some(&row));
        }
    }
    let total = me.contacts.borrow().len();
    me.status.set_text(&if visible.len() == total { format!("{total} contact(s)") } else { format!("{} of {total} contact(s)", visible.len()) });
}

fn subtitle(c: &Contact) -> String {
    let s = c.summary();
    if c.ns == ME { s } else if s.is_empty() { c.ns.clone() } else { format!("{s} · {}", c.ns) }
}

fn empty_page() -> adw::StatusPage {
    adw::StatusPage::builder().icon_name("avatar-default-symbolic").title("No contact selected").description("Pick one, or add one with +. Your address book is your own library me: every device of yours has it.").build()
}

fn show_related(me: &Rc<App>, group: &adw::PreferencesGroup, c: &Contact, items: Vec<retifs_contacts_core::Related>) {
    group.set_visible(!items.is_empty());
    for r in items {
        let when = r.when.replace('T', " ");
        let sub = [when.as_str(), if r.ns == ME { "" } else { r.ns.as_str() }].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join(" · ");
        let row = adw::ActionRow::builder().title(glib::markup_escape_text(&r.title)).subtitle(glib::markup_escape_text(&sub)).build();
        row.add_prefix(&gtk::Image::from_icon_name(if r.is_photo() { "image-x-generic-symbolic" } else if r.kind == "event" { "x-office-calendar-symbolic" } else { "text-x-generic-symbolic" }));
        if r.is_photo() {
            let current = c.get("photo") == r.id;
            let use_it = gtk::Button::builder().label(if current { "Profile photo" } else { "Use as profile photo" }).valign(gtk::Align::Center).sensitive(!current).build();
            use_it.add_css_class("flat");
            let (me2, c2, photo_id) = (me.clone(), c.clone(), r.id.clone());
            use_it.connect_clicked(move |_| {
                let mut updated = c2.clone();
                updated.set("photo", &photo_id);
                let (me3, before) = (me2.clone(), c2.clone());
                bg(&me2.book, move |b| b.save(&updated, Some(&before)), move |r| match r {
                    Ok(saved) => open_card(&me3, Some(saved)),
                    Err(e) => me3.status.set_text(&format!("Not saved: {e}")),
                });
            });
            row.add_suffix(&use_it);
        }
        group.add(&row);
    }
}

/// Opens a contact's card (None: a new one).
fn open_card(me: &Rc<App>, contact: Option<Contact>) {
    let ns = contact.as_ref().map(|c| c.ns.clone()).unwrap_or_else(|| me.scope().first().cloned().filter(|_| me.scope.selected() > 0).unwrap_or_else(|| ME.into()));
    let c = contact.clone().unwrap_or_else(|| Contact::new(&ns));
    let page = adw::PreferencesPage::new();

    let head = adw::PreferencesGroup::new();
    let avatar = adw::Avatar::new(96, Some(&c.display_name()), true);
    let name_label = gtk::Label::builder().label(c.display_name()).css_classes(["title-1"]).margin_top(6).build();
    let head_box = gtk::Box::builder().orientation(gtk::Orientation::Vertical).halign(gtk::Align::Center).margin_bottom(12).build();
    head_box.append(&avatar);
    head_box.append(&name_label);
    head.add(&head_box);
    page.add(&head);

    // Records elsewhere that point at this person (ADR-0024): photos they're
    // in, events they're at; a photo can be their profile picture.
    let related = adw::PreferencesGroup::builder().title("Related").description("Photos and events that name this person, in any of your libraries.").visible(false).build();
    page.add(&related);
    if let Some(id) = c.id.clone() {
        let (me2, c2, related2, avatar2) = (me.clone(), c.clone(), related.clone(), avatar.clone());
        let photo_id = c.get("photo").to_string();
        bg(&me.book, move |b| {
            // Every library: photos and calendars point at contacts too.
            let libs = b.all_libraries().unwrap_or_default();
            let items = b.related(&id, &libs);
            let photo = (!photo_id.is_empty()).then(|| b.content_of(&photo_id, &libs, 20 << 20)).flatten();
            (items, photo)
        }, move |(items, photo)| {
            if let Some(texture) = photo.and_then(|bytes| gtk::gdk::Texture::from_bytes(&glib::Bytes::from_owned(bytes)).ok()) {
                avatar2.set_custom_image(Some(&texture));
            }
            show_related(&me2, &related2, &c2, items);
        });
    }


    let details = adw::PreferencesGroup::builder().title("Details").build();
    let mut fields = Vec::new();
    for (key, label) in FIELDS {
        let row = adw::EntryRow::builder().title(label).text(c.get(key)).build();
        if key == "birthday" {
            row.set_tooltip_text(Some("YYYY-MM-DD, or --MM-DD without the year"));
        }
        details.add(&row);
        fields.push((key, row));
    }
    page.add(&details);

    let mut multi = Vec::new();
    for kind in MULTI {
        let (title, add_label) = match kind {
            "email" => ("Email", "Add email"),
            "phone" => ("Phone", "Add phone number"),
            _ => ("Address", "Add address"),
        };
        let group = adw::PreferencesGroup::builder().title(title).build();
        let rows: Rc<RefCell<Vec<(gtk::DropDown, adw::EntryRow)>>> = Rc::new(RefCell::new(Vec::new()));
        let mut items = c.multi(kind);
        if items.is_empty() {
            items.push((String::new(), String::new()));
        }
        for (label, value) in items {
            add_multi_row(&group, &rows, &label, &value);
        }
        let add = gtk::Button::builder().label(add_label).css_classes(["flat"]).halign(gtk::Align::Start).build();
        {
            let (group, rows) = (group.clone(), rows.clone());
            add.connect_clicked(move |_| add_multi_row(&group, &rows, "", ""));
        }
        group.set_header_suffix(Some(&add));
        page.add(&group);
        multi.push((kind, rows));
    }

    let where_group = adw::PreferencesGroup::builder().title("Library").description("Your own address book is me; a club's library is shared with its members.").build();
    let libs = me.libraries.borrow().clone();
    let library = adw::ComboRow::builder().title("Kept in").model(&gtk::StringList::new(&libs.iter().map(String::as_str).collect::<Vec<_>>())).build();
    library.set_selected(libs.iter().position(|l| *l == c.ns).unwrap_or(0) as u32);
    // Moving a contact to another library isn't done yet: new ones choose.
    library.set_sensitive(contact.is_none());
    where_group.add(&library);
    page.add(&where_group);

    // Unsaved changes enable Save.
    let save = me.save.clone();
    let dirty = move || save.set_sensitive(true);
    for (_, row) in &fields {
        let d = dirty.clone();
        row.connect_changed(move |_| d());
    }
    me.save.set_sensitive(contact.is_none());
    me.delete.set_sensitive(contact.is_some());
    me.title.set_title(&if contact.is_some() { c.display_name() } else { "New contact".into() });
    me.title.set_subtitle(&if c.ns == ME { "your address book".into() } else { format!("in {}", c.ns) });
    me.card_bin.set_child(Some(&page));
    *me.card.borrow_mut() = Some(Card { before: contact, ns, fields, multi, library });
}

fn add_multi_row(group: &adw::PreferencesGroup, rows: &Rc<RefCell<Vec<(gtk::DropDown, adw::EntryRow)>>>, label: &str, value: &str) {
    let mut labels: Vec<String> = LABELS.iter().map(|l| l.to_string()).collect();
    if !labels.iter().any(|l| l == label) {
        labels.push(label.to_string());
    }
    let shown: Vec<&str> = labels.iter().map(|l| if l.is_empty() { "main" } else { l.as_str() }).collect();
    let dd = gtk::DropDown::from_strings(&shown);
    dd.set_valign(gtk::Align::Center);
    dd.set_selected(labels.iter().position(|l| l == label).unwrap_or(0) as u32);
    // The labels as stored (index 0: none).
    unsafe { dd.set_data("labels", labels.clone()) };
    let row = adw::EntryRow::builder().title("").text(value).build();
    row.add_prefix(&dd);
    group.add(&row);
    rows.borrow_mut().push((dd, row));
}

fn label_of(dd: &gtk::DropDown) -> String {
    let labels: Option<&Vec<String>> = unsafe { dd.data::<Vec<String>>("labels").map(|p| p.as_ref()) };
    labels.and_then(|l| l.get(dd.selected() as usize).cloned()).unwrap_or_default()
}

fn save_card(me: &Rc<App>) {
    let card = me.card.borrow();
    let Some(card) = card.as_ref() else { return };
    let mut c = card.before.clone().unwrap_or_else(|| Contact::new(&card.ns));
    if card.before.is_none() {
        let libs = me.libraries.borrow();
        c.ns = libs.get(card.library.selected() as usize).cloned().unwrap_or_else(|| ME.into());
    }
    for (key, row) in &card.fields {
        c.set(key, &row.text());
    }
    for (kind, rows) in &card.multi {
        let items: Vec<(String, String)> = rows.borrow().iter().map(|(dd, row)| (label_of(dd), row.text().to_string())).collect();
        c.set_multi(kind, &items);
    }
    let before = card.before.clone();
    let me2 = me.clone();
    me.save.set_sensitive(false);
    bg(&me.book, move |b| b.save(&c, before.as_ref()), move |r| match r {
        Ok(saved) => {
            me2.status.set_text(&format!("Saved {}", saved.display_name()));
            open_card(&me2, Some(saved));
            reload(&me2);
        }
        Err(e) => {
            me2.save.set_sensitive(true);
            me2.status.set_text(&format!("Not saved: {e}"));
        }
    });
}

fn delete_card(me: &Rc<App>) {
    let Some(c) = me.card.borrow().as_ref().and_then(|c| c.before.clone()) else { return };
    let dialog = adw::MessageDialog::builder()
        .heading(format!("Delete {}?", c.display_name()))
        .body("Its history stays in the library: an earlier version can be brought back.")
        .default_response("cancel")
        .close_response("cancel")
        .transient_for(&me.window)
        .build();
    dialog.add_responses(&[("cancel", "Cancel"), ("delete", "Delete")]);
    dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
    let me2 = me.clone();
    dialog.connect_response(None, move |_, r| {
        if r != "delete" {
            return;
        }
        let (c, me3) = (c.clone(), me2.clone());
        bg(&me2.book, move |b| b.delete(&c), move |r| {
            match r {
                Ok(()) => {
                    *me3.card.borrow_mut() = None;
                    me3.card_bin.set_child(Some(&empty_page()));
                    me3.title.set_title("Contacts");
                    me3.title.set_subtitle("");
                    me3.delete.set_sensitive(false);
                    me3.save.set_sensitive(false);
                }
                Err(e) => me3.status.set_text(&format!("Not deleted: {e}")),
            }
            reload(&me3);
        });
    });
    dialog.present();
}

/// RETIFS_CONTACTS_OPEN=<name>: that contact's card on start (demos).
fn open_on_start(me: &Rc<App>) {
    let Ok(name) = std::env::var("RETIFS_CONTACTS_OPEN") else { return };
    let me2 = me.clone();
    glib::timeout_add_seconds_local_once(2, move || {
        let c = me2.contacts.borrow().iter().find(|c| c.display_name() == name).cloned();
        if let Some(c) = c {
            open_card(&me2, Some(c));
            show_list(&me2);
        }
    });
}

/// RETIFS_CONTACTS_SNAPSHOT=<file.png>: the window drawn to a file every 2 s
/// (demos and screenshots without a screen grab).
fn snapshot_on_request(window: &adw::ApplicationWindow) {
    let Ok(path) = std::env::var("RETIFS_CONTACTS_SNAPSHOT") else { return };
    let window = window.clone();
    glib::timeout_add_seconds_local(2, move || {
        let paintable = gtk::WidgetPaintable::new(Some(&window));
        let snapshot = gtk::Snapshot::new();
        paintable.snapshot(&snapshot, window.width() as f64, window.height() as f64);
        if let (Some(node), Some(renderer)) = (snapshot.to_node(), window.renderer()) {
            let _ = renderer.render_texture(&node, None).save_to_png(&path);
        }
        glib::ControlFlow::Continue
    });
}
