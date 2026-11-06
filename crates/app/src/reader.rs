//! Read-only HTML mail. Each selection gets an ephemeral WebKit context and
//! immutable CID allowlist, so in-flight requests cannot read another message.
use crate::{BangApp, Theme};
use gtk::{gdk, gio, glib, prelude::*};
use slint::{ComponentHandle, Global};
use std::{cell::RefCell, collections::HashMap, rc::Rc, sync::Arc, time::Duration};
use webkit2gtk::{
    CookieManagerExt, NavigationPolicyDecision, NavigationPolicyDecisionExt, NavigationType,
    PolicyDecisionExt, PolicyDecisionType, ResponsePolicyDecision, ResponsePolicyDecisionExt,
    SettingsExt, URIRequestExt, URIResponseExt, URISchemeRequestExt, URISchemeResponseExt,
    WebContext, WebContextExt, WebView, WebViewExt,
};

thread_local! { static READER: RefCell<Option<Rc<RefCell<Reader>>>> = const { RefCell::new(None) }; }
struct Reader {
    fixed: gtk::Fixed,
    view: Option<WebView>,
    message: Option<bang_core::MessageFull>,
    store: Option<Arc<bang_core::Store>>,
    bridge: Option<crate::bridge::Bridge>,
    dark: bool,
    generation: u64,
    geometry: Option<(i32, i32, i32, i32)>,
}

/// Wire the main window's HTML viewport and image-loading action.
pub fn attach(
    ui: &BangApp,
    fixed: &gtk::Fixed,
    store: Option<Arc<bang_core::Store>>,
    bridge: Option<crate::bridge::Bridge>,
) {
    let reader = Rc::new(RefCell::new(Reader {
        fixed: fixed.clone(),
        view: None,
        message: None,
        store,
        bridge,
        dark: Theme::get(ui).get_dark(),
        generation: 0,
        geometry: None,
    }));
    READER.with(|r| *r.borrow_mut() = Some(reader.clone()));
    ui.set_native_reader_available(true);
    let weak = ui.as_weak();
    let r = reader.clone();
    ui.on_reader_load_images(move || {
        if let Some(ui) = weak.upgrade() {
            ui.set_remote_images(true);
            r.borrow_mut().rebuild(&ui);
        }
    });
    let weak = ui.as_weak();
    glib::timeout_add_local(Duration::from_millis(16), move || {
        let Some(ui) = weak.upgrade() else {
            return glib::ControlFlow::Break;
        };
        let mut r = reader.borrow_mut();
        if r.dark != Theme::get(&ui).get_dark() {
            r.dark = Theme::get(&ui).get_dark();
            r.rebuild(&ui);
        }
        r.layout(&ui);
        glib::ControlFlow::Continue
    });
}
/// Discard the old reader immediately when selection changes.
pub fn clear(ui: &BangApp) {
    ui.set_message_html("".into());
    ui.set_remote_images(false);
    ui.set_reader_failed(false);
    READER.with(|r| {
        if let Some(r) = r.borrow().as_ref() {
            let mut r = r.borrow_mut();
            r.remove_view();
            r.message = None;
        }
    });
}
/// Display an already selection-checked message. The Slint text stays available as fallback.
pub fn load(ui: &BangApp, message: &bang_core::MessageFull) {
    clear(ui);
    ui.set_message_html(message.body_html.clone().into());
    READER.with(|r| {
        if let Some(r) = r.borrow().as_ref() {
            let mut r = r.borrow_mut();
            r.message = Some(message.clone());
            r.rebuild(ui);
        }
    });
}
impl Reader {
    fn remove_view(&mut self) {
        if let Some(view) = self.view.take() {
            view.stop_loading();
            self.fixed.remove(&view);
        }
        self.geometry = None;
    }
    fn rebuild(&mut self, ui: &BangApp) {
        self.remove_view();
        let Some(message) = &self.message else { return };
        if message.body_html.is_empty() {
            return;
        }
        self.generation += 1;
        let generation = self.generation;
        let id = message.summary.id;
        let document_url = format!("bangmail://reader/{id}/{generation}");
        let html = prepare_document(&message.body_html, self.dark);
        let remote = ui.get_remote_images();
        let csp = content_policy(remote);
        let context = WebContext::new_ephemeral();
        context.set_cache_model(webkit2gtk::CacheModel::DocumentViewer);
        if let Some(cookies) = context.cookie_manager() {
            cookies.set_accept_policy(webkit2gtk::CookieAcceptPolicy::Never);
        }
        let url = document_url.clone();
        context.register_uri_scheme("bangmail", move |request| {
            if request.uri().as_deref() == Some(url.as_str()) {
                respond(
                    request,
                    html.as_bytes(),
                    "text/html; charset=utf-8",
                    &csp,
                    200,
                );
            } else {
                respond(request, b"", "text/plain", &csp, 404);
            }
        });
        let resources: HashMap<_, _> = message
            .attachments
            .iter()
            .filter(|a| !a.content_id.is_empty() && a.content_type.starts_with("image/"))
            .filter_map(|a| {
                self.store
                    .as_ref()?
                    .read_attachment(&a.path)
                    .ok()
                    .map(|bytes| (a.content_id.clone(), (a.content_type.clone(), bytes)))
            })
            .collect();
        context.register_uri_scheme("cid", move |request| {
            let uri = request.uri().unwrap_or_default();
            let key = percent_encoding::percent_decode_str(uri.strip_prefix("cid:").unwrap_or(""))
                .decode_utf8_lossy();
            if let Some((mime, bytes)) = resources.get(key.as_ref()) {
                respond(request, bytes, mime, &content_policy(false), 200);
            } else {
                respond(request, b"", "text/plain", &content_policy(false), 404);
            }
        });
        let view = WebView::with_context(&context);
        let settings = webkit2gtk::Settings::new();
        settings.set_enable_javascript(false);
        settings.set_enable_javascript_markup(false);
        settings.set_enable_html5_database(false);
        settings.set_enable_html5_local_storage(false);
        settings.set_enable_dns_prefetching(false);
        settings.set_allow_file_access_from_file_urls(false);
        settings.set_allow_universal_access_from_file_urls(false);
        view.set_settings(&settings);
        view.set_background_color(&if self.dark {
            gdk::RGBA::new(0.12, 0.12, 0.12, 1.)
        } else {
            gdk::RGBA::new(1., 1., 1., 1.)
        });
        // An allowlist for the exact top-level document. Only real link clicks
        // may leave the reader, and they are handled by the parent application.
        let store = self.store.clone();
        let bridge = self.bridge.clone();
        let weak = ui.as_weak();
        view.connect_decide_policy(move |_, decision, kind| {
            if matches!(
                kind,
                PolicyDecisionType::NavigationAction | PolicyDecisionType::NewWindowAction
            ) {
                if let Some(navigation) = decision.downcast_ref::<NavigationPolicyDecision>() {
                    if let Some(action) = navigation.navigation_action() {
                        let uri = action.request().and_then(|r| r.uri()).unwrap_or_default();
                        if uri.split('#').next() == Some(document_url.as_str())
                            && kind == PolicyDecisionType::NavigationAction
                        {
                            decision.use_();
                            return true;
                        }
                        decision.ignore();
                        if action.navigation_type() == NavigationType::LinkClicked
                            && action.is_user_gesture()
                        {
                            if let Some(ui) = weak.upgrade() {
                                open_link(&ui, &uri, store.clone(), bridge.clone());
                            }
                        }
                        return true;
                    }
                }
                decision.ignore();
                return true;
            }
            // A response WebKit treats as a file download (Content-Disposition
            // or the download attribute) must not start a download: the
            // reader only renders, it never saves files.
            if kind == PolicyDecisionType::Response {
                if let Some(response_decision) = decision.downcast_ref::<ResponsePolicyDecision>() {
                    if response_decision
                        .response()
                        .as_ref()
                        .is_some_and(|r| r.suggested_filename().is_some())
                    {
                        decision.ignore();
                        return true;
                    }
                }
            }
            false
        });
        view.connect_create(|_, _| None);
        // The document URI is generated by the parent, never from the mail.
        let url = format!("bangmail://reader/{id}/{generation}");
        let document_url = url.clone();
        let weak = ui.as_weak();
        view.connect_load_failed(move |_, _, failed_uri, _| {
            // A failed sub-resource (a dead remote image after consent) must
            // not discard the whole document; only a document failure falls
            // back to the plain-text body.
            if failed_uri == document_url {
                fail(&weak, id, generation);
            }
            false
        });
        let weak = ui.as_weak();
        view.connect_web_process_terminated(move |_, _| {
            fail(&weak, id, generation);
        });
        view.connect_context_menu(|_, _, _, _| true);
        self.fixed.put(&view, 0, 0);
        view.set_no_show_all(true);
        view.load_uri(&url);
        self.view = Some(view);
        self.layout(ui);
    }
    fn layout(&mut self, ui: &BangApp) {
        let Some(view) = &self.view else { return };
        let (x, y, w, h) = (
            ui.get_reader_x_px().round() as i32,
            ui.get_reader_y_px().round() as i32,
            ui.get_reader_width_px().round() as i32,
            ui.get_reader_height_px().round() as i32,
        );
        let shown = ui.get_reader_shown() && w > 0 && h > 0;
        if shown {
            let geometry = (x, y, w, h);
            if self.geometry != Some(geometry) {
                self.fixed.move_(view, x, y);
                view.set_size_request(w, h);
                self.geometry = Some(geometry);
            }
            view.show();
        } else {
            view.hide();
        }
    }
}
fn fail(weak: &slint::Weak<BangApp>, id: i64, generation: u64) {
    // Defer: WebKit can report failures while rebuild holds the reader borrow.
    let weak = weak.clone();
    glib::idle_add_local_once(move || {
        if let Some(ui) = weak.upgrade() {
            READER.with(|r| {
                if let Some(r) = r.borrow().as_ref() {
                    let r = r.borrow();
                    if ui.get_selected_message() as i64 == id && r.generation == generation {
                        ui.set_reader_failed(true);
                        ui.set_status_message(
                            crate::i18n::tr("HTML reader unavailable; showing text").into(),
                        );
                    }
                }
            });
        }
    });
}
fn respond(
    request: &webkit2gtk::URISchemeRequest,
    bytes: &[u8],
    mime: &str,
    policy: &str,
    status: u32,
) {
    let stream = gio::MemoryInputStream::from_bytes(&glib::Bytes::from(bytes));
    let response = webkit2gtk::URISchemeResponse::new(&stream, bytes.len() as i64);
    response.set_status(status, None);
    response.set_content_type(mime);
    let headers = soup::MessageHeaders::new(soup::MessageHeadersType::Response);
    headers.append("Content-Security-Policy", policy);
    headers.append("Cache-Control", "no-store");
    headers.append("X-Content-Type-Options", "nosniff");
    response.set_http_headers(headers);
    request.finish_with_response(&response);
}
fn content_policy(remote: bool) -> String {
    format!("default-src 'none'; script-src 'none'; style-src 'unsafe-inline'; img-src cid: data:{}; font-src data:; connect-src 'none'; frame-src 'none'; object-src 'none'; media-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'", if remote { " http: https:" } else { "" })
}
fn prepare_document(html: &str, dark: bool) -> String {
    let doc = dom_query::Document::from(html);
    // Remove active/navigation and speculative-loading elements even when a
    // browser does not enforce the corresponding CSP directive.
    for group in doc.select("script, link, meta, base, iframe, frame, frameset, object, embed, input, button, select, textarea").iter() {
        if let Some(node) = group.get(0) { node.remove_from_parent(); }
    }
    for group in doc.select("*").iter() {
        if let Some(node) = group.get(0) {
            for attr in node.attrs() {
                let name = attr.name.local.to_string();
                if name.starts_with("on")
                    || matches!(name.as_str(), "ping" | "srcdoc" | "autofocus")
                {
                    node.remove_attr(&name);
                }
            }
        }
    }
    if let Some(head) = doc.head() {
        let (fg, bg) = if dark {
            ("#ededed", "#1f1f1f")
        } else {
            ("#202020", "#ffffff")
        };
        head.prepend_html(format!("<meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><style>body{{margin:16px;color:{fg};background-color:{bg};font:15px/1.5 sans-serif;overflow-wrap:anywhere}}img{{max-width:100%;height:auto}}pre{{white-space:pre-wrap}}a{{color:#438be8}}</style>"));
    }
    doc.html().to_string()
}
fn open_link(
    ui: &BangApp,
    uri: &str,
    store: Option<Arc<bang_core::Store>>,
    bridge: Option<crate::bridge::Bridge>,
) {
    let Ok(url) = url::Url::parse(uri) else {
        return;
    };
    match url.scheme() {
        "http" | "https" => {
            let uri = uri.to_string();
            std::thread::spawn(move || {
                let _ = webbrowser::open(&uri);
            });
        }
        "mailto" => {
            let (Some(store), Some(bridge)) = (store, bridge) else {
                return;
            };
            let prefill = mailto_prefill(&url);
            if let Err(e) = crate::composer::open(ui, store, bridge, Some(prefill), None) {
                ui.set_status_message(e.to_string().into());
            }
        }
        _ => {}
    }
}
fn mailto_prefill(url: &url::Url) -> crate::composer::Prefill {
    let mut to: Vec<_> = percent_encoding::percent_decode_str(url.path())
        .decode_utf8_lossy()
        .split(',')
        .filter(|a| !a.trim().is_empty())
        .map(|a| a.trim().to_owned())
        .collect();
    let mut subject = String::new();
    let mut body = String::new();
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "subject" => subject = value.into_owned(),
            "body" => body = value.into_owned(),
            "to" => to.extend(
                value
                    .split(',')
                    .map(|a| a.trim().to_owned())
                    .filter(|a| !a.is_empty()),
            ),
            _ => {}
        }
    }
    crate::composer::Prefill {
        from: None,
        to,
        cc: Vec::new(),
        subject,
        body_html: format!(
            "<p>{}</p>",
            body.replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
                .replace('\n', "<br>")
        ),
        in_reply_to: None,
        references: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pump_for(duration: Duration) {
        let until = std::time::Instant::now() + duration;
        while std::time::Instant::now() < until {
            while gtk::events_pending() && std::time::Instant::now() < until {
                gtk::main_iteration_do(false);
            }
            slint::platform::update_timers_and_animations();
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn current_view() -> WebView {
        READER.with(|r| {
            r.borrow()
                .as_ref()
                .unwrap()
                .borrow()
                .view
                .as_ref()
                .unwrap()
                .clone()
        })
    }
    fn js(script: &str) -> String {
        use javascriptcore::ValueExt;
        let result = Rc::new(RefCell::new(None));
        let out = result.clone();
        let settings = WebViewExt::settings(&current_view()).unwrap();
        assert!(!settings.enables_javascript());
        // Test-only DOM inspection: incoming scripts/handlers have already been
        // removed and the CSP stays in force. Restore before returning.
        settings.set_enable_javascript(true);
        current_view().evaluate_javascript(
            script,
            Some("bang-tests"),
            None,
            None::<&gio::Cancellable>,
            move |r| *out.borrow_mut() = Some(r.map(|v| v.to_str().to_string())),
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while result.borrow().is_none() && std::time::Instant::now() < deadline {
            pump_for(Duration::from_millis(20));
        }
        settings.set_enable_javascript(false);
        let result = result
            .borrow_mut()
            .take()
            .expect("WebKit JavaScript inspection timed out")
            .expect("WebKit inspection failed");
        result
    }
    fn await_loaded() {
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            pump_for(Duration::from_millis(50));
            if !current_view().is_loading() && current_view().estimated_load_progress() == 1. {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "WebKit load timed out"
            );
        }
        pump_for(Duration::from_millis(200));
    }
    #[cfg(feature = "preview-renderer")]
    fn native_click(ui: &BangApp, label: &str) {
        let element = i_slint_backend_testing::ElementHandle::find_by_accessible_label(ui, label)
            .find(|e| e.size().width > 0. && e.size().height > 0.)
            .expect(label);
        let p = element.absolute_position();
        let size = element.size();
        crate::gtk_host::click_native(slint::LogicalPosition::new(
            p.x + size.width / 2.,
            p.y + size.height / 2.,
        ));
    }
    #[cfg(feature = "preview-renderer")]
    fn assert_native_chrome_click(ui: &BangApp) {
        let clicks = Rc::new(std::cell::Cell::new(0));
        let count = clicks.clone();
        ui.on_compose_clicked(move || count.set(count.get() + 1));
        native_click(ui, &crate::i18n::tr("New message"));
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while clicks.get() == 0 {
            pump_for(Duration::from_millis(20));
            assert!(
                std::time::Instant::now() < deadline,
                "native click did not reach the compose button"
            );
        }
        assert_eq!(clicks.get(), 1);
    }
    fn await_visibility(ui: &BangApp, visible: bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        loop {
            pump_for(Duration::from_millis(20));
            if current_view().is_visible() == visible {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "reader visibility did not become {visible}; shown={}, settings={}, preview={}",
                ui.get_reader_shown(),
                ui.get_settings_open(),
                ui.get_preview_open()
            );
        }
    }
    #[test]
    #[ignore = "requires a GTK display, WebKitGTK 4.1 and loopback sockets; run separately under X11 and Wayland"]
    fn native_html_reader() {
        use std::{
            io::{Read, Write},
            net::TcpListener,
            sync::{
                atomic::{AtomicBool, Ordering},
                Mutex,
            },
        };
        crate::gtk_host::init().unwrap();
        let root = std::env::temp_dir().join(format!("bang-reader-test-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let store = Arc::new(bang_core::Store::open(&root.join("mail.db")).unwrap());
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            32,
            32,
            image::Rgba([30, 160, 100, 255]),
        ))
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
        let png = png.into_inner();
        let path = store.save_attachment(1, "logo.png", &png).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let log = requests.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let bytes = png.clone();
        let server = std::thread::spawn(move || {
            while !stopped.load(Ordering::Relaxed) {
                if let Ok((mut stream, _)) = listener.accept() {
                    stream
                        .set_read_timeout(Some(Duration::from_secs(1)))
                        .unwrap();
                    let mut buffer = [0u8; 4096];
                    let count = stream.read(&mut buffer).unwrap_or(0);
                    log.lock().unwrap().push(
                        String::from_utf8_lossy(&buffer[..count])
                            .lines()
                            .next()
                            .unwrap_or("")
                            .to_string(),
                    );
                    let _ = write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",bytes.len());
                    let _ = stream.write_all(&bytes);
                } else {
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        });
        let ui = BangApp::new().unwrap();
        crate::i18n::select("fr");
        Theme::get(&ui).set_dark(std::env::var("BANG_THEME").unwrap_or_default() == "dark");
        Theme::get(&ui).invoke_sync_palette();
        ui.window().set_size(slint::LogicalSize::new(1440., 900.));
        ui.set_account_count(1);
        ui.set_selected_message(1);
        crate::gtk_host::attach(&ui, Some(store.clone()), None);
        let mut message: bang_core::MessageFull = serde_json::from_value(serde_json::json!({
            "summary":{"id":1,"account_id":"test","folder_id":1,"uid":1,"message_id":"test","from":{"name":"Ophélie","address":"lea@example.test"},"to":[],"cc":[],"subject":"Été déjà prêt","date":0,"flags":"","preview":"","has_attachments":false},
            "body_text":"Été déjà prêt — texte de secours", "body_html":format!("<html><head><meta http-equiv=refresh content='0;url=http://{address}/refresh'><link rel=preload href='http://{address}/preload'><style>@import url('http://{address}/sheet'); td{{padding:20px;background:#e4eede}}#background{{background-image:url(http://{address}/background);width:32px;height:32px}}</style></head><body style='background:#fff;color:#222'><h1>Les offres d’été</h1><table><tr><td>Déjà prêt</td><td>À partir de 3,14 €</td></tr></table><img id='embedded' src='cid:logo%40example.test'><img id='remote' src='http://{address}/remote'><div id='background'></div><script src='http://{address}/script'></script><script>document.title='executed'</script><a href='https://example.test'>Voir les nouveautés</a></body></html>"),
            "attachments":[{"id":1,"message_id":1,"filename":"logo.png","content_type":"image/png","size":png.len(),"path":path,"content_id":"logo@example.test","disposition":"inline"}]
        })).unwrap();
        ui.set_messages(slint::ModelRc::new(slint::VecModel::from(vec![
            crate::state::to_row(&message.summary),
        ])));
        ui.set_folders(slint::ModelRc::new(slint::VecModel::from(vec![
            crate::FolderRow {
                id: 1,
                name: "Boîte de réception".into(),
                kind: "inbox".into(),
                ..Default::default()
            },
        ])));
        crate::state::set_message_header(&ui, &message.summary);
        ui.set_message_sender(message.summary.from.name.clone().into());
        ui.set_message_subject(message.summary.subject.clone().into());
        ui.set_message_body(message.body_text.clone().into());
        ui.show().unwrap();
        pump_for(Duration::from_millis(200));
        #[cfg(feature = "preview-renderer")]
        if std::env::var_os("BANG_NATIVE_POINTER_TEST").is_some() {
            assert_native_chrome_click(&ui);
        }
        load(&ui, &message);
        await_loaded();
        #[cfg(feature = "preview-renderer")]
        if std::env::var_os("BANG_NATIVE_POINTER_TEST").is_some() {
            assert_native_chrome_click(&ui);
            let clicks = Rc::new(std::cell::Cell::new(0));
            let count = clicks.clone();
            current_view().connect_button_press_event(move |_, _| {
                count.set(count.get() + 1);
                glib::Propagation::Proceed
            });
            crate::gtk_host::click_native(slint::LogicalPosition::new(
                ui.get_reader_x_px() + 12.,
                ui.get_reader_y_px() + 12.,
            ));
            let deadline = std::time::Instant::now() + Duration::from_secs(3);
            while clicks.get() == 0 {
                pump_for(Duration::from_millis(20));
                assert!(
                    std::time::Instant::now() < deadline,
                    "native click did not reach WebKit"
                );
            }
        }
        assert!(!ui.get_reader_failed());
        assert!(current_view().is_visible());
        assert!(
            requests.lock().unwrap().is_empty(),
            "network request before consent: {:?}",
            requests.lock().unwrap()
        );
        assert_eq!(
            js("document.getElementById('embedded').naturalWidth.toString()"),
            "32"
        );
        assert_eq!(
            js("document.getElementById('remote').naturalWidth.toString()"),
            "0"
        );
        assert!(js("document.body.innerText").contains("À partir de 3,14 €"));
        assert_ne!(js("document.title"), "executed");
        assert_eq!(
            js("getComputedStyle(document.body).backgroundColor"),
            "rgb(255, 255, 255)"
        );
        if let Some(path) = std::env::var_os("BANG_NATIVE_SNAPSHOT") {
            crate::gtk_host::snapshot(std::path::Path::new(&path));
        }
        #[cfg(feature = "preview-renderer")]
        if std::env::var_os("BANG_NATIVE_POINTER_TEST").is_some() {
            let folders = Rc::new(std::cell::Cell::new(0));
            let count = folders.clone();
            ui.on_folder_selected(move |id| {
                assert_eq!(id, 1);
                count.set(count.get() + 1);
            });
            native_click(&ui, "Boîte de réception");
            let deadline = std::time::Instant::now() + Duration::from_secs(3);
            while folders.get() == 0 {
                pump_for(Duration::from_millis(20));
                assert!(
                    std::time::Instant::now() < deadline,
                    "native folder click was blocked"
                );
            }
            let weak = ui.as_weak();
            ui.on_settings_opened(move || weak.upgrade().unwrap().set_settings_open(true));
            let weak = ui.as_weak();
            ui.on_settings_close(move || weak.upgrade().unwrap().set_settings_open(false));
            native_click(&ui, &crate::i18n::tr("Settings"));
            await_visibility(&ui, false);
            native_click(&ui, &crate::i18n::tr("Close"));
            await_visibility(&ui, true);
        }
        ui.set_settings_open(true);
        await_visibility(&ui, false);
        ui.set_settings_open(false);
        await_visibility(&ui, true);
        ui.set_organization_open(true);
        await_visibility(&ui, false);
        ui.set_managing_labels(true);
        pump_for(Duration::from_millis(50));
        ui.set_organization_open(false);
        await_visibility(&ui, true);
        ui.set_preview_open(true);
        await_visibility(&ui, false);
        ui.set_preview_open(false);
        pump_for(Duration::from_millis(50));
        ui.window().set_size(slint::LogicalSize::new(1024., 768.));
        pump_for(Duration::from_millis(150));
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while (current_view().allocated_width() as f32 - ui.get_reader_width_px()).abs() >= 2. {
            pump_for(Duration::from_millis(20));
            assert!(
                std::time::Instant::now() < deadline,
                "reader width {} differs from viewport {}",
                current_view().allocated_width(),
                ui.get_reader_width_px()
            );
        }
        #[cfg(feature = "preview-renderer")]
        {
            ui.window().set_size(slint::LogicalSize::new(720., 480.));
            pump_for(Duration::from_millis(150));
            // A compositor may enforce fullscreen, as with Weston's kiosk shell.
            if ui.window().size().width as f32 / ui.window().scale_factor() <= 721. {
                assert!(!ui.get_reader_shown());
                await_visibility(&ui, false);
                if std::env::var_os("BANG_NATIVE_POINTER_TEST").is_some() {
                    native_click(&ui, "Ophélie, Été déjà prêt");
                    await_visibility(&ui, true);
                    native_click(&ui, &crate::i18n::tr("Back"));
                    await_visibility(&ui, false);
                } else {
                    let row = i_slint_backend_testing::ElementHandle::find_by_accessible_label(
                        &ui,
                        "Ophélie, Été déjà prêt",
                    )
                    .next()
                    .expect("message row");
                    let position = row.absolute_position();
                    let position = slint::LogicalPosition::new(position.x + 12., position.y + 12.);
                    ui.window()
                        .dispatch_event(slint::platform::WindowEvent::PointerPressed {
                            position,
                            button: slint::platform::PointerEventButton::Left,
                        });
                    ui.window()
                        .dispatch_event(slint::platform::WindowEvent::PointerReleased {
                            position,
                            button: slint::platform::PointerEventButton::Left,
                        });
                    await_visibility(&ui, true);
                    ui.window()
                        .dispatch_event(slint::platform::WindowEvent::KeyPressed {
                            text: slint::platform::Key::Escape.into(),
                        });
                    ui.window()
                        .dispatch_event(slint::platform::WindowEvent::KeyReleased {
                            text: slint::platform::Key::Escape.into(),
                        });
                    await_visibility(&ui, false);
                }
            }
            ui.window().set_size(slint::LogicalSize::new(1024., 768.));
            pump_for(Duration::from_millis(150));
        }
        #[cfg(feature = "preview-renderer")]
        if std::env::var_os("BANG_NATIVE_POINTER_TEST").is_some() {
            native_click(&ui, &crate::i18n::tr("Load images"));
            let deadline = std::time::Instant::now() + Duration::from_secs(3);
            while !ui.get_remote_images() {
                pump_for(Duration::from_millis(20));
                assert!(
                    std::time::Instant::now() < deadline,
                    "native image button click was blocked"
                );
            }
        } else {
            ui.invoke_reader_load_images();
        }
        #[cfg(not(feature = "preview-renderer"))]
        ui.invoke_reader_load_images();
        await_loaded();
        pump_for(Duration::from_millis(150));
        assert_eq!(
            js("document.getElementById('remote').naturalWidth.toString()"),
            "32"
        );
        let log = requests.lock().unwrap().clone();
        assert!(log.iter().any(|r| r.contains("/remote")));
        assert!(log.iter().any(|r| r.contains("/background")));
        assert!(
            !log.iter()
                .any(|r| ["/sheet", "/refresh", "/preload", "/script"]
                    .iter()
                    .any(|p| r.contains(p))),
            "unexpected request: {log:?}"
        );
        // Pending failures and resources from the old selection must not touch a new message.
        message.summary.id = 2;
        message.body_html = "<p>Deuxième été</p>".into();
        ui.set_selected_message(2);
        load(&ui, &message);
        await_loaded();
        assert!(!ui.get_remote_images());
        assert!(!ui.get_reader_failed());
        assert_eq!(js("document.body.innerText.trim()"), "Deuxième été");
        fail(&ui.as_weak(), 1, 1);
        pump_for(Duration::from_millis(50));
        assert!(!ui.get_reader_failed());
        Theme::get(&ui).set_dark(true);
        pump_for(Duration::from_millis(100));
        await_loaded();
        assert_eq!(
            js("getComputedStyle(document.body).backgroundColor"),
            "rgb(31, 31, 31)"
        );
        current_view().grab_focus();
        crate::gtk_host::check_keyboard_and_clipboard(&ui);
        let generation = READER.with(|r| r.borrow().as_ref().unwrap().borrow().generation);
        fail(&ui.as_weak(), 2, generation);
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while !ui.get_reader_failed() {
            pump_for(Duration::from_millis(20));
            assert!(
                std::time::Instant::now() < deadline,
                "failure did not activate the text fallback"
            );
        }
        assert!(!ui.get_html_active());
        message.summary.id = 3;
        message.body_html.clear();
        ui.set_selected_message(3);
        load(&ui, &message);
        assert!(!ui.get_html_active());
        assert!(READER.with(|r| r.borrow().as_ref().unwrap().borrow().view.is_none()));
        clear(&ui);
        assert!(!ui.get_html_active());
        ui.hide().unwrap();
        stop.store(true, Ordering::Relaxed);
        server.join().unwrap();
        READER.with(|r| *r.borrow_mut() = None);
        drop(ui);
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn mail_document_preserves_style_and_blocks_active_content() {
        let html = prepare_document("<html><head><meta http-equiv=refresh content='0;url=https://bad.test'><link rel=prefetch href='https://bad.test'><style>td{color:red}</style></head><body bgcolor='white'><script>alert(1)</script><table><tr><td>Été à 3,14€</td></tr></table><img src='cid:logo' onerror='alert(1)'><a href='https://example.test' ping='https://bad.test'>Lien</a></body></html>", true);
        assert!(html.contains("Été à 3,14€"));
        assert!(html.contains("td{color:red}"));
        assert!(html.contains("cid:logo"));
        for forbidden in ["refresh", "prefetch", "<script", "onerror", "ping="] {
            assert!(!html.contains(forbidden), "{forbidden}");
        }
        assert!(content_policy(false).contains("img-src cid: data:;"));
        assert!(content_policy(true).contains("img-src cid: data: http: https:;"));
        assert!(content_policy(true).contains("style-src 'unsafe-inline';"));
    }
    #[test]
    fn mailto_decodes_accents_and_escapes_body() {
        let p = mailto_prefill(
            &url::Url::parse(
                "mailto:lea%40example.test?subject=%C3%89t%C3%A9&body=Bonjour%0A%3Cb%3E",
            )
            .unwrap(),
        );
        assert_eq!(p.to, ["lea@example.test"]);
        assert_eq!(p.subject, "Été");
        assert_eq!(p.body_html, "<p>Bonjour<br>&lt;b&gt;</p>");
    }
}
