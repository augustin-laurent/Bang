//! Native GTK/WebKit host. Standard output is reserved for JSON IPC.
use bang_composer::{EditorMessage, HostMessage};
use gtk::prelude::*;
use std::io::{BufRead, Write};
use tao::platform::unix::{EventLoopBuilderExtUnix, WindowExtUnix};
use tao::{
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder},
    window::WindowBuilder,
};
use wry::{http::Response, WebViewBuilder, WebViewBuilderExtUnix};
include!(concat!(env!("OUT_DIR"), "/assets.rs"));
#[derive(Debug)]
enum Message {
    Host(HostMessage),
    Editor(EditorMessage),
    ParentClosed,
}
fn emit(msg: &EditorMessage) {
    let mut out = std::io::stdout().lock();
    if serde_json::to_writer(&mut out, msg).is_ok() {
        let _ = writeln!(out);
        let _ = out.flush();
    }
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut event_loop_builder = EventLoopBuilder::<Message>::with_user_event();
    event_loop_builder.with_app_id("io.github.augustin_laurent.Bang");
    let event_loop = event_loop_builder.build();
    let window = WindowBuilder::new()
        .with_title("Bang — New message")
        .with_inner_size(tao::dpi::LogicalSize::new(900., 720.))
        .with_min_inner_size(tao::dpi::LogicalSize::new(640., 480.))
        .build(&event_loop)?;
    window
        .gtk_window()
        .set_icon_name(Some("io.github.augustin_laurent.Bang"));
    let proxy = event_loop.create_proxy();
    let ipc = proxy.clone();
    let builder=WebViewBuilder::new().with_custom_protocol("bang".into(),move |_,request| {
        let path=if request.uri().path()=="/" {"/index.html"}else{request.uri().path()};
        let (status,body,mime)=match asset(path){Some((body,mime))=>(200,body,mime),None=>(404,&b"Not found"[..],"text/plain")};
        Response::builder().status(status).header("Content-Type",mime)
            .header("Content-Security-Policy","default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self'; connect-src 'none'; base-uri 'none'; form-action 'none'")
            .body(std::borrow::Cow::Borrowed(body)).unwrap()
    }).with_navigation_handler(|url| url.starts_with("bang://localhost/"))
    .with_ipc_handler(move |request|{if let Ok(msg)=serde_json::from_str::<EditorMessage>(request.body()){let _=ipc.send_event(Message::Editor(msg));}})
    .with_url("bang://localhost/");
    let webview = builder.build_gtk(window.default_vbox().ok_or("GTK container unavailable")?)?;
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else {
                break;
            };
            if let Ok(msg) = serde_json::from_str(&line) {
                if proxy.send_event(Message::Host(msg)).is_err() {
                    return;
                }
            }
        }
        let _ = proxy.send_event(Message::ParentClosed);
    });
    let ready = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    event_loop.run(move |event, _, flow| {
        *flow = ControlFlow::Wait;
        match event {
            Event::UserEvent(Message::ParentClosed) => *flow = ControlFlow::Exit,
            Event::UserEvent(Message::Editor(EditorMessage::Ready)) => {
                ready.store(true, std::sync::atomic::Ordering::SeqCst);
                emit(&EditorMessage::Ready);
            }
            Event::UserEvent(Message::Host(msg)) => {
                if let HostMessage::Focus = msg {
                    window.set_focus();
                } else {
                    if let HostMessage::Init {
                        ref language, dark, ..
                    }
                    | HostMessage::Preferences { ref language, dark } = msg
                    {
                        window.set_title(if language == "fr" {
                            "Bang — Nouveau message"
                        } else {
                            "Bang — New message"
                        });
                        window.set_theme(Some(if dark {
                            tao::window::Theme::Dark
                        } else {
                            tao::window::Theme::Light
                        }));
                    }
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = webview.evaluate_script(&format!("window.bangReceive({json})"));
                    }
                }
            }
            Event::UserEvent(Message::Editor(EditorMessage::Attach)) => {
                if let Some(paths) = rfd::FileDialog::new().pick_files() {
                    emit(&EditorMessage::AttachPaths {
                        paths: paths
                            .into_iter()
                            .map(|p| p.to_string_lossy().into_owned())
                            .collect(),
                    });
                }
            }
            Event::UserEvent(Message::Editor(EditorMessage::Close)) => *flow = ControlFlow::Exit,
            Event::UserEvent(Message::Editor(msg)) => emit(&msg),
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                // Before the editor script is ready there is no draft to
                // protect, so a close request must not be lost in a failed
                // script evaluation.
                if ready.load(std::sync::atomic::Ordering::SeqCst) {
                    let _ = webview.evaluate_script("window.bangReceive({type:'request_close'})");
                } else {
                    *flow = ControlFlow::Exit;
                }
            }
            _ => {}
        }
    });
}
