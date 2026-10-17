# Bang interface

The accepted references live in `assets/design/`: `inbox-light.png`,
`inbox-dark.png`, `compose.png` and `settings-accounts.png`. They were developed
with tasteskill v2 and Frontend App Builder. The explicit Mail 26 reference takes
precedence over generic taste rules for the blue message selection and grouped
toolbar. All surfaces remain opaque; Linux supplies its own window decorations.

The primary screen uses a 260px account sidebar, a 384px message list and the
remaining width for reading. Drags constrain sidebar width to 180–300px and list
width to 280–480px. Below 1120px, navigation becomes an overlay. Below 900px,
list and detail alternate without rebuilding the list or losing its scroll.

The shared Slint theme owns neutral surfaces, semantic text, borders, selection,
spacing and typography. Noto Sans and one original outline SVG family are used
throughout. The web composer carries the same palette and bundled font. Rich
message typography is separate from interface typography.

The composer uses a compact header, optional Cc/Bcc, sender selection and a
collapsible formatting row. Attachments appear immediately below that row,
before the editor, with native selection, name, size and a remove control.
Text selections survive toolbar use. Plain text is exported beside HTML for
mail clients without rich rendering. A failed send keeps the draft and files.

First run, empty folders, empty search, loading and errors have distinct text.
Settings change immediately, persist, and update the open composer. Removing an
account and abandoning an unsent message require explicit confirmation.

This version does not add reply/forward/archive commands, cloud draft storage,
AI, calendar features or a full HTML renderer for received mail.
