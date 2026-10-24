# retifs Contacts

An address book on [retifs](../retifs): the `type=contact` records of ADR-0024 in your own library `me` (every device of yours has them) and in shared libraries (a family's address book is a club's library).

- A list, searchable, of the contacts in all your libraries or one.
- A card to read and edit one: names, organisation, birthday, labelled emails, phone numbers and addresses, a note.
- **Related:** the photos a person is in (`person:<contact id>`) and the events they're at (`attendee:<contact id>`), in any of your libraries. Any of those photos can be their profile picture (`photo = <photo record id>`; nothing is copied).
- Updates as the libraries change, here or on another device.

`core/` talks to `retifsd` through libretifs; `gtk/` is the Linux front end (GTK 4, libadwaita). Others (Windows, Haiku, Android) come later over the same core.

Build the GTK app on Linux (or WSL): `cargo build -p retifs-contacts-gtk`. For demos, `RETIFS_CONTACTS_OPEN=<name>` opens a contact and `RETIFS_CONTACTS_SNAPSHOT=<file.png>` draws the window to a file every 2 s.
