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


Sender avatars use 32px circles in 90px list rows and 40px circles in the reader.
Colored initials appear immediately; downloaded photos/logos use the same circular
shape. Inbox category tabs sit above the message list, with unread counts and
compact labels in narrow columns. Custom labels appear in the sidebar; the reader's
Classify action opens category corrections and multiple label assignments. Label
management supplies a name and one of six accessible colors. These dialogs share
the existing opaque surfaces, modal input guard, translations and native-reader
visibility behavior.

First-launch setup uses three screens: Welcome, Preferences, and Connect account.
A native vector envelope unfolds into a miniature inbox once over 1.1 seconds;
step changes use a 240ms fade and short translation. GTK's animation preference
is followed live, and reduced motion shows the finished illustration. The welcome
screen owns focus, blocks inbox interaction, and hides the native mail reader.

Only a missing configuration with no accounts starts setup automatically. The
optional `welcome_completed` preference is false while setup is pending and true
after connection or Set up later; legacy configurations and existing accounts
bypass it. Interrupted setup restarts at Welcome with saved preferences retained.
A failed completion save keeps the final screen open with a retry action and
preserves any connected account.
