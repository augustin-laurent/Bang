import { Editor } from "@tiptap/core";
import StarterKit from "@tiptap/starter-kit";
import { TextStyleKit } from "@tiptap/extension-text-style";
import Highlight from "@tiptap/extension-highlight";
import TextAlign from "@tiptap/extension-text-align";
import { parseAddresses, safeLink, fileSize } from "./logic.mjs";
import "./style.css";

type Account = { id: string; email: string };
type FileInfo = { id: number; name: string; size: number };
type Host = {
  type: string;
  accounts?: Account[];
  language?: string;
  dark?: boolean;
  translations?: Record<string, string>;
  files?: FileInfo[];
  ok?: boolean;
  error?: string;
  message?: string;
  to?: string[];
  subject?: string;
  body_html?: string;
};
declare global {
  interface Window {
    ipc?: { postMessage: (message: string) => void };
    bangReceive: (message: Host) => void;
  }
}
let language = "en",
  translations: Record<string, string> = {},
  accounts: Account[] = [],
  files: FileInfo[] = [],
  pending = false,
  dirty = false,
  formatVisible = true;
const preview = new URLSearchParams(location.search).has("preview");
const app = document.querySelector<HTMLElement>("#app")!;
const paths: Record<string, string> = {
  send: '<path d="m22 2-7 20-4-9L2 9 22 2z M11 13 22 2"/>',
  clip: '<path d="m8 13 7-7a3 3 0 0 1 4 4l-9 9a5 5 0 0 1-7-7L13 2"/>',
  compose: '<path d="M14 4H4v16h16V10 M12 12l1-4 7-7 3 3-7 7-4 1z"/>',
  file: '<path d="M5 2h9l5 5v15H5V2z M14 2v6h5"/>',
  folder: '<path d="M3 5h6l2 2h10v12H3V5z"/>',
  link: '<path d="m10 14 4-4 M8 16l-2 2a4 4 0 0 1-6-6l5-5a4 4 0 0 1 6 0 M16 8l2-2a4 4 0 0 1 6 6l-5 5a4 4 0 0 1-6 0"/>',
  highlight: '<path d="m14 3 7 7-10 10H4v-7L14 3z M10 7l7 7 M3 23h18"/>',
  bullet:
    '<path d="M9 5h13 M9 12h13 M9 19h13"/><circle cx="3" cy="5" r="1"/><circle cx="3" cy="12" r="1"/><circle cx="3" cy="19" r="1"/>',
  ordered:
    '<path d="M9 5h13 M9 12h13 M9 19h13 M2 2h2v5 M2 11c4-3 4 1 0 4h4 M2 18h3l-2 2c4 0 2 4-1 2"/>',
  close: '<path d="m5 5 14 14 M19 5 5 19"/>',
};
const icon = (name: string) =>
  `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${paths[name] || ""}</svg>`;
const t = (key: string) => (language === "fr" ? translations[key] || key : key);
const post = (msg: unknown) => {
  if (window.ipc) window.ipc.postMessage(JSON.stringify(msg));
  else if (preview)
    document.dispatchEvent(new CustomEvent("bang:ipc", { detail: msg }));
};
const button = (id: string, key: string, content: string, extra = "") =>
  `<button type="button" id="${id}" data-label="${key}" ${extra}>${content}</button>`;
app.innerHTML = `
<header class="toolbar"><div class="title">${icon("compose")}<span data-i18n="New message"></span></div><div class="toolbar-actions">${button("attach", "Attach a file", icon("clip"))}${button("format", "Formatting", "Aa", 'aria-pressed="true"')}${button("save-draft", "Save draft", icon("file"))}${button("save-template", "Save as template", icon("folder"))}${button("send", "Send", icon("send"), 'class="primary"')}</div></header>
<section class="addresses" data-label="Message headers">
<div class="field"><label for="to" data-i18n="To:"></label><input id="to" type="text" autocomplete="off" spellcheck="false"><button id="cc-toggle" type="button" aria-expanded="false">Cc / Cci ▾</button></div>
<div class="field optional" hidden><label for="cc">Cc :</label><input id="cc" type="text" spellcheck="false"></div>
<div class="field optional" hidden><label for="bcc">Cci :</label><input id="bcc" type="text" spellcheck="false"></div>
<div class="field"><label for="subject" data-i18n="Subject:"></label><input id="subject" type="text"></div>
<div class="field"><label for="from" data-i18n="From:"></label><select id="from"></select></div>
</section>
<section id="formatting" class="formatting" data-label="Formatting">
<select id="font" data-label="Font"><option value="sans-serif" data-i18n="Sans-serif"></option><option value="serif" data-i18n="Serif"></option><option value="monospace" data-i18n="Monospace"></option></select>
<select id="size" data-label="Size">${[9, 10, 11, 12, 14, 16, 18, 24, 36].map((n) => `<option value="${n}" ${n === 12 ? "selected" : ""}>${n}</option>`).join("")}</select><i class="separator"></i>
${button("bold", "Bold", "<b>B</b>")}${button("italic", "Italic", "<em>I</em>")}${button("underline", "Underline", "<u>U</u>")}<i class="separator"></i>
<label class="color-control" data-label="Text color"><span class="color-mark">A</span><input id="color" type="color" value="#202124" data-label="Text color"></label>
<span class="highlight-group">${button("highlight-toggle", "Highlight", icon("highlight"))}<input id="highlight" type="color" value="#ffeb8a" data-label="Highlight color"></span><i class="separator"></i>
<select id="align" data-label="Alignment">${["Left", "Center", "Right", "Justify"].map((s) => `<option value="${s.toLowerCase()}" data-i18n="${s}"></option>`).join("")}</select>
${button("bullet", "Bullet list", icon("bullet"))}${button("ordered", "Numbered list", icon("ordered"))}<i class="separator"></i>${button("link", "Link", icon("link"))}
</section>
<section id="attachments" class="attachments" hidden data-label="Attachments"></section>
<div id="feedback" role="status" aria-live="polite" hidden></div>
<div id="editor-wrap"><div id="editor"></div></div>
<dialog id="discard-dialog"><h2 data-i18n="Discard message?"></h2><p data-i18n="Your unsent changes will be lost."></p><div class="dialog-actions"><button id="keep" data-i18n="Keep editing"></button><button id="discard" class="danger" data-i18n="Discard"></button></div></dialog>
<dialog id="link-dialog"><form method="dialog"><h2 data-i18n="Link address"></h2><label class="sr-only" for="link-url" data-i18n="Link address"></label><input id="link-url" type="url" placeholder="https://"><div class="dialog-actions"><button value="cancel" data-i18n="Cancel"></button><button id="unlink" type="button" data-i18n="Remove link"></button><button id="apply-link" type="button" class="primary" data-i18n="Apply"></button></div></form></dialog>
<dialog id="template-dialog"><h2 data-i18n="Template name"></h2><label class="sr-only" for="template-name" data-i18n="Template name"></label><input id="template-name" type="text" autocomplete="off"><div class="dialog-actions"><button id="cancel-template" type="button" data-i18n="Cancel"></button><button id="apply-template" type="button" class="primary" data-i18n="Save"></button></div></dialog>`;
const el = <T extends HTMLElement = HTMLElement>(id: string) =>
  document.getElementById(id) as T;
const input = (id: string) => el<HTMLInputElement>(id);
const editor = new Editor({
  element: el("editor"),
  extensions: [
    StarterKit.configure({
      heading: false,
      codeBlock: false,
      horizontalRule: false,
      link: {
        openOnClick: false,
        protocols: ["http", "https", "mailto"],
        isAllowedUri: (url) => safeLink(url),
      },
    }),
    TextStyleKit,
    Highlight.configure({ multicolor: true }),
    TextAlign.configure({ types: ["paragraph", "heading"] }),
  ],
  editorProps: {
    attributes: {
      role: "textbox",
      "aria-multiline": "true",
      "aria-label": "Message",
      spellcheck: "true",
    },
    handleClickOn: (_view, _pos, node) => node.type.name === "link",
  },
  onUpdate: () => setDirty(),
  onSelectionUpdate: () => updateToolbar(),
  onTransaction: () => updateToolbar(),
});
function setDirty() {
  dirty = true;
  post({ type: "dirty", dirty: true });
}
function feedback(message: string) {
  el("feedback").textContent = message;
  el("feedback").hidden = !message;
}
function localize() {
  document.documentElement.lang = language;
  document.title = `Bang — ${t("New message")}`;
  document
    .querySelectorAll<HTMLElement>("[data-i18n]")
    .forEach((node) => (node.textContent = t(node.dataset.i18n!)));
  document.querySelectorAll<HTMLElement>("[data-label]").forEach((node) => {
    const text = t(node.dataset.label!);
    node.title = text;
    node.setAttribute("aria-label", text);
  });
  input("to").placeholder = "camille@atelier.example";
  el("cc-toggle").textContent = language === "fr" ? "Cc / Cci ▾" : "Cc / Bcc ▾";
  const bcc = document.querySelector<HTMLLabelElement>('label[for="bcc"]');
  if (bcc) bcc.textContent = language === "fr" ? "Cci :" : "Bcc :";
  editor.view.dom.setAttribute(
    "aria-label",
    language === "fr" ? "Corps du message" : "Message body",
  );
  renderFiles();
}
function renderFiles() {
  const shelf = el("attachments");
  shelf.replaceChildren();
  shelf.hidden = files.length === 0;
  for (const file of files) {
    const row = document.createElement("div");
    row.className = "attachment";
    const symbol = document.createElement("span");
    symbol.innerHTML = icon("file");
    row.append(symbol);
    const meta = document.createElement("span");
    const name = document.createElement("strong");
    name.textContent = file.name;
    const size = document.createElement("small");
    size.textContent = fileSize(file.size, language);
    meta.append(name, size);
    row.append(meta);
    const remove = document.createElement("button");
    remove.type = "button";
    remove.innerHTML = icon("close");
    remove.title = t("Remove");
    remove.setAttribute("aria-label", `${t("Remove")} ${file.name}`);
    remove.disabled = pending;
    remove.onclick = () => {
      post({ type: "remove_attachment", id: file.id });
      if (preview) {
        files = files.filter((f) => f.id !== file.id);
        renderFiles();
      }
      setDirty();
    };
    row.append(remove);
    shelf.append(row);
  }
}
function updateToolbar() {
  for (const [id, mark] of [
    ["bold", "bold"],
    ["italic", "italic"],
    ["underline", "underline"],
    ["bullet", "bulletList"],
    ["ordered", "orderedList"],
    ["link", "link"],
    ["highlight-toggle", "highlight"],
  ])
    el(id).setAttribute("aria-pressed", String(editor.isActive(mark)));
  const attrs = editor.getAttributes("textStyle");
  el<HTMLSelectElement>("font").value = attrs.fontFamily || "sans-serif";
  el<HTMLSelectElement>("size").value = String(
    parseFloat(attrs.fontSize || "12"),
  );
  const color = attrs.color;
  document.querySelector<HTMLElement>(".color-mark")!.style.borderBottomColor =
    color || "";
  if (color?.startsWith("#")) input("color").value = color;
  const highlight = editor.getAttributes("highlight").color;
  if (highlight?.startsWith("#")) input("highlight").value = highlight;
  el<HTMLSelectElement>("align").value =
    editor.getAttributes("paragraph").textAlign || "left";
}
function sending(value: boolean) {
  pending = value;
  editor.setEditable(!value);
  document
    .querySelectorAll<HTMLInputElement | HTMLButtonElement | HTMLSelectElement>(
      "button,input,select",
    )
    .forEach((n) => (n.disabled = value));
  renderFiles();
}
function requestClose() {
  if (pending) {
    feedback(t("Keep open until sending completes."));
    post({ type: "close_cancelled" });
    return;
  }
  if (dirty) el<HTMLDialogElement>("discard-dialog").showModal();
  else post({ type: "close" });
}
window.bangReceive = (msg: Host) => {
  if (msg.type === "init") {
    accounts = msg.accounts || [];
    translations = msg.translations || {};
    language = msg.language || "en";
    document.documentElement.dataset.theme = msg.dark ? "dark" : "light";
    const select = el<HTMLSelectElement>("from");
    const previous = select.value;
    select.replaceChildren();
    for (const acc of accounts) {
      const option = document.createElement("option");
      option.value = acc.id;
      option.textContent = acc.email;
      select.append(option);
    }
    if (accounts.some((a) => a.id === previous)) select.value = previous;
    localize();
    input("to").focus();
  } else if (msg.type === "preferences") {
    language = msg.language || "en";
    document.documentElement.dataset.theme = msg.dark ? "dark" : "light";
    localize();
  } else if (msg.type === "prefill") {
    input("to").value = (msg.to || []).join(", ");
    input("subject").value = msg.subject || "";
    editor.commands.setContent(msg.body_html || "", { emitUpdate: false });
    setDirty();
    input("to").focus();
  } else if (msg.type === "attachments") {
    files = msg.files || [];
    renderFiles();
    setDirty();
  } else if (msg.type === "send_result") {
    sending(false);
    if (msg.ok) {
      dirty = false;
      post({ type: "dirty", dirty: false });
      post({ type: "close" });
    } else feedback(`${t("Send failed:")} ${msg.error || ""}`);
  } else if (msg.type === "error") feedback(msg.message || "");
  else if (msg.type === "request_close") requestClose();
};
el("attach").onclick = () => post({ type: "attach" });
el("cc-toggle").onclick = () => {
  const expanded = el("cc-toggle").getAttribute("aria-expanded") !== "true";
  el("cc-toggle").setAttribute("aria-expanded", String(expanded));
  document
    .querySelectorAll<HTMLElement>(".optional")
    .forEach((n) => (n.hidden = !expanded));
};
el("format").onclick = () => {
  formatVisible = !formatVisible;
  el("formatting").hidden = !formatVisible;
  el("format").setAttribute("aria-pressed", String(formatVisible));
};
for (const id of ["bold", "italic", "underline", "bullet", "ordered"]) {
  el(id).onmousedown = (e) => e.preventDefault();
  el(id).onclick = () => {
    const chain = editor.chain().focus();
    if (id === "bold") chain.toggleBold().run();
    if (id === "italic") chain.toggleItalic().run();
    if (id === "underline") chain.toggleUnderline().run();
    if (id === "bullet") chain.toggleBulletList().run();
    if (id === "ordered") chain.toggleOrderedList().run();
    updateToolbar();
  };
}
el<HTMLSelectElement>("font").onchange = (e) =>
  editor
    .chain()
    .focus()
    .setFontFamily((e.target as HTMLSelectElement).value)
    .run();
el<HTMLSelectElement>("size").onchange = (e) =>
  editor
    .chain()
    .focus()
    .setFontSize((e.target as HTMLSelectElement).value + "pt")
    .run();
input("color").oninput = (e) =>
  editor
    .chain()
    .focus()
    .setColor((e.target as HTMLInputElement).value)
    .run();
input("highlight").oninput = (e) =>
  editor
    .chain()
    .focus()
    .setHighlight({ color: (e.target as HTMLInputElement).value })
    .run();
el<HTMLSelectElement>("align").onchange = (e) =>
  editor
    .chain()
    .focus()
    .setTextAlign((e.target as HTMLSelectElement).value)
    .run();
el("highlight-toggle").onmousedown = (e) => e.preventDefault();
el("highlight-toggle").onclick = () =>
  editor
    .chain()
    .focus()
    .toggleHighlight({ color: input("highlight").value })
    .run();
el("link").onmousedown = (e) => e.preventDefault();
el("link").onclick = () => {
  input("link-url").value = editor.getAttributes("link").href || "";
  el<HTMLDialogElement>("link-dialog").showModal();
  input("link-url").focus();
};
el("apply-link").onclick = () => {
  const url = input("link-url").value.trim();
  if (!safeLink(url)) {
    input("link-url").setCustomValidity(t("Link address"));
    input("link-url").reportValidity();
    return;
  }
  input("link-url").setCustomValidity("");
  editor.chain().focus().extendMarkRange("link").setLink({ href: url }).run();
  el<HTMLDialogElement>("link-dialog").close();
};
input("link-url").oninput = () => input("link-url").setCustomValidity("");
el<HTMLFormElement>("link-dialog").querySelector("form")!.onsubmit = (e) => {
  e.preventDefault();
  if ((e.submitter as HTMLButtonElement | null)?.value === "cancel") {
    el<HTMLDialogElement>("link-dialog").close();
    return;
  }
  el("apply-link").click();
};
el("unlink").onclick = () => {
  editor.chain().focus().extendMarkRange("link").unsetLink().run();
  el<HTMLDialogElement>("link-dialog").close();
};
el("keep").onclick = () => {
  el<HTMLDialogElement>("discard-dialog").close();
  post({ type: "close_cancelled" });
};
el("discard-dialog").addEventListener("cancel", () =>
  post({ type: "close_cancelled" }),
);
el("discard").onclick = () => {
  dirty = false;
  post({ type: "close" });
};
function collectDraft() {
  let to: string[], cc: string[], bcc: string[];
  try {
    to = parseAddresses(input("to").value);
    cc = parseAddresses(input("cc").value);
    bcc = parseAddresses(input("bcc").value);
  } catch {
    feedback(t("Check the recipient addresses."));
    return null;
  }
  if (to.length + cc.length + bcc.length === 0) {
    feedback(t("Enter at least one valid recipient."));
    input("to").focus();
    return null;
  }
  const account = el<HTMLSelectElement>("from").value;
  if (!accounts.some((a) => a.id === account)) {
    feedback(t("No account available"));
    return null;
  }
  return {
    account,
    to,
    cc,
    bcc,
    subject: input("subject").value,
    body_text: editor.getText({ blockSeparator: "\n\n" }),
    body_html: editor.getHTML(),
  };
}
el("send").onclick = () => {
  if (pending) return;
  const draft = collectDraft();
  if (!draft) return;
  sending(true);
  feedback(t("Sending…"));
  post({ type: "send", ...draft });
};
el("save-draft").onclick = () => {
  if (pending) return;
  const draft = collectDraft();
  if (!draft) return;
  post({ type: "save_draft", ...draft });
  feedback(t("Draft saved"));
};
let pendingTemplate: ReturnType<typeof collectDraft> = null;
el("save-template").onclick = () => {
  if (pending) return;
  pendingTemplate = collectDraft();
  if (!pendingTemplate) return;
  input("template-name").value = "";
  el<HTMLDialogElement>("template-dialog").showModal();
  input("template-name").focus();
};
el("cancel-template").onclick = () =>
  el<HTMLDialogElement>("template-dialog").close();
el("apply-template").onclick = () => {
  const name = input("template-name").value.trim();
  if (!name) {
    input("template-name").setCustomValidity(t("Template name"));
    input("template-name").reportValidity();
    return;
  }
  post({ type: "save_template", name, content: pendingTemplate });
  el<HTMLDialogElement>("template-dialog").close();
  feedback(t("Template saved"));
};
input("template-name").oninput = () =>
  input("template-name").setCustomValidity("");
document
  .querySelectorAll(".addresses input,.addresses select")
  .forEach((n) => n.addEventListener("input", setDirty));
document.addEventListener("keydown", (event) => {
  if (event.key === "Escape" && !document.querySelector("dialog[open]")) {
    event.preventDefault();
    requestClose();
  }
  if ((event.ctrlKey || event.metaKey) && event.key === "Enter") {
    event.preventDefault();
    el("send").click();
  }
});
// Keep linked mail content inside the editor; the host also rejects remote navigation.
editor.view.dom.addEventListener("click", (e) => {
  if ((e.target as HTMLElement).closest("a")) e.preventDefault();
});
localize();
post({ type: "ready" });
if (preview) {
  document.addEventListener("bang:ipc", ((event: CustomEvent) => {
    if (event.detail.type === "send") {
      app.dataset.previewSend = JSON.stringify(event.detail);
      queueMicrotask(() =>
        window.bangReceive({
          type: "send_result",
          ok: false,
          error: t("Offline preview: no message was sent."),
        }),
      );
    }
  }) as EventListener);
  const fr = (await import("../../../app/i18n/fr.json")).default;
  window.bangReceive({
    type: "init",
    accounts: [
      { id: "personal", email: "augustin@example.fr" },
      { id: "work", email: "augustin@atelier.example" },
    ],
    language: new URLSearchParams(location.search).get("lang") || "fr",
    dark: new URLSearchParams(location.search).get("theme") === "dark",
    translations: fr,
  });
  input("to").value = "Camille Moreau <camille@atelier.example>";
  input("subject").value = "Les prochaines étapes pour Bang";
  editor.commands.setContent(
    '<p>Bonjour Camille,</p><p>Merci pour ton retour. Voici les trois points pour notre prochain échange :</p><ul><li><p>Valider les <strong>proportions</strong> des trois volets</p></li><li><p>Finaliser la <u>barre d’outils</u></p></li><li><p>Préparer la <mark data-color="#ffeb8a" style="background-color:#ffeb8a">version Linux</mark></p></li></ul><p><a href="https://atelier.example/bang">Voir la maquette.</a></p><p>À bientôt,<br>Augustin</p>',
  );
  files = [{ id: 1, name: "maquette-bang.pdf", size: 253952 }];
  renderFiles();
  dirty = false;
}
