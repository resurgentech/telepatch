//! The terminal frame. [`Terminal`] is the Dioxus component: the bar with
//! the size and the full-screen button, and the element the shell fills.
//! [`Shell`] is what runs inside it: a WebSocket to the server's shell, a
//! terminal that turns its bytes into cells, and a canvas the cells are
//! drawn on. The grid is as many cells as fit the frame, so it follows the
//! frame when the window or full screen changes its size. Key presses go
//! to the shell while the frame has the focus.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use dioxus::prelude::*;
use futures_channel::oneshot;
use wasm_bindgen::convert::FromWasmAbi;
use wasm_bindgen::prelude::*;
use web_sys::{BinaryType, CanvasRenderingContext2d, HtmlCanvasElement, MessageEvent, WebSocket, Window};

use crate::keys;
use crate::term::{self, Rgb, Term};

const FONT_PX: f64 = 15.0;
const FONT_FAMILY: &str = "ui-monospace, 'DejaVu Sans Mono', Menlo, Consolas, monospace";

fn font(bold: bool, italic: bool) -> String {
    let weight = if bold { "bold " } else { "" };
    let slant = if italic { "italic " } else { "" };
    format!("{slant}{weight}{FONT_PX}px {FONT_FAMILY}")
}

fn css((r, g, b): Rgb) -> String {
    format!("rgb({r},{g},{b})")
}

/// The terminal and the canvas it is drawn on.
struct View {
    term: Term,
    canvas: HtmlCanvasElement,
    ctx: CanvasRenderingContext2d,
    dpr: f64,
    /// Cell size in CSS pixels.
    cell_w: f64,
    cell_h: f64,
    cols: u16,
    rows: u16,
}

impl View {
    /// Make the grid as many cells as fit `width` x `height` CSS pixels.
    /// True when that changed its size.
    fn fit(&mut self, width: f64, height: f64) -> bool {
        let cols = (width / self.cell_w).floor().clamp(20.0, 500.0) as u16;
        let rows = (height / self.cell_h).floor().clamp(4.0, 200.0) as u16;
        if (cols, rows) == (self.cols, self.rows) {
            return false;
        }
        (self.cols, self.rows) = (cols, rows);
        self.term.resize(cols, rows);
        let (w, h) = (self.cell_w * f64::from(cols), self.cell_h * f64::from(rows));
        self.canvas.set_width((w * self.dpr).round() as u32);
        self.canvas.set_height((h * self.dpr).round() as u32);
        let _ = self
            .canvas
            .set_attribute("style", &format!("width:{w}px;height:{h}px"));
        // Resizing a canvas resets its context.
        let _ = self.ctx.set_transform(self.dpr, 0.0, 0.0, self.dpr, 0.0, 0.0);
        self.ctx.set_text_baseline("middle");
        true
    }

    /// Redraw the whole screen.
    fn draw(&self) {
        let (ctx, cw, ch) = (&self.ctx, self.cell_w, self.cell_h);
        ctx.set_fill_style_str(&css(term::BG));
        ctx.fill_rect(0.0, 0.0, cw * f64::from(self.cols), ch * f64::from(self.rows));
        let cursor = self.term.cursor();
        let mut style = (false, false);
        ctx.set_font(&font(false, false));
        for row in 0..self.rows {
            for col in 0..self.cols {
                let Some(cell) = self.term.cell(row, col) else {
                    continue;
                };
                let (x, y) = (f64::from(col) * cw, f64::from(row) * ch);
                let w = if cell.wide { cw * 2.0 } else { cw };
                // The cursor is a block: the cell with its colours swapped.
                let (fg, bg) = if cursor == Some((row, col)) {
                    (cell.bg, cell.fg)
                } else {
                    (cell.fg, cell.bg)
                };
                if bg != term::BG {
                    ctx.set_fill_style_str(&css(bg));
                    ctx.fill_rect(x, y, w, ch);
                }
                ctx.set_fill_style_str(&css(fg));
                if !cell.text.trim().is_empty() {
                    if style != (cell.bold, cell.italic) {
                        style = (cell.bold, cell.italic);
                        ctx.set_font(&font(cell.bold, cell.italic));
                    }
                    let _ = ctx.fill_text(cell.text, x, y + ch / 2.0);
                }
                if cell.underline {
                    ctx.fill_rect(x, y + ch - 2.0, w, 1.0);
                }
            }
        }
    }
}

/// Listen for `event` on `target` for the life of the page.
fn listen<E: FromWasmAbi + 'static>(
    target: &web_sys::EventTarget,
    event: &str,
    handler: impl FnMut(E) + 'static,
) -> Result<(), JsValue> {
    let closure = Closure::<dyn FnMut(E)>::new(handler);
    target.add_event_listener_with_callback(event, closure.as_ref().unchecked_ref())?;
    closure.forget();
    Ok(())
}

/// A shell on the server, shown on the canvas `#term`.
struct Shell {
    view: Rc<RefCell<View>>,
    ws: WebSocket,
}

impl Shell {
    /// Start a shell whose grid fits `width` x `height` CSS pixels. `closed`
    /// is sent when the shell ends.
    fn open(window: &Window, width: f64, height: f64, closed: oneshot::Sender<()>) -> Result<Shell, JsValue> {
        let canvas: HtmlCanvasElement = window
            .document()
            .and_then(|d| d.get_element_by_id("term"))
            .ok_or("no #term canvas")?
            .dyn_into()?;
        let ctx: CanvasRenderingContext2d = canvas.get_context("2d")?.ok_or("no 2d context")?.dyn_into()?;

        // The cell width is rounded up to a whole device pixel so
        // neighbouring backgrounds leave no seam.
        let dpr = window.device_pixel_ratio();
        ctx.set_font(&font(false, false));
        let mut view = View {
            term: Term::new(80, 24),
            canvas,
            cell_w: (ctx.measure_text("M")?.width() * dpr).ceil() / dpr,
            cell_h: (FONT_PX * 1.3 * dpr).ceil() / dpr,
            ctx,
            dpr,
            cols: 0,
            rows: 0,
        };
        view.fit(width, height);
        view.draw();
        let (cols, rows) = (view.cols, view.rows);
        let view = Rc::new(RefCell::new(view));

        let host = window.location().host()?;
        let ws = WebSocket::new(&format!("ws://{host}/ws?cols={cols}&rows={rows}"))?;
        ws.set_binary_type(BinaryType::Arraybuffer);
        let shell = Shell { view, ws };

        // Output: feed the terminal now, draw once on the next frame
        // however many messages arrive before it.
        let scheduled = Rc::new(Cell::new(false));
        let on_frame = Closure::<dyn FnMut()>::new({
            let (view, scheduled) = (shell.view.clone(), scheduled.clone());
            move || {
                scheduled.set(false);
                view.borrow().draw();
            }
        });
        listen(&shell.ws, "message", {
            let (view, window) = (shell.view.clone(), window.clone());
            move |event: MessageEvent| {
                let Ok(buffer) = event.data().dyn_into::<js_sys::ArrayBuffer>() else {
                    return;
                };
                let bytes = js_sys::Uint8Array::new(&buffer).to_vec();
                view.borrow_mut().term.write(&bytes);
                if !scheduled.replace(true) {
                    let _ = window.request_animation_frame(on_frame.as_ref().unchecked_ref());
                }
            }
        })?;
        // The frame may have changed size while the socket was connecting.
        listen(&shell.ws, "open", {
            let (view, ws) = (shell.view.clone(), shell.ws.clone());
            move |_: JsValue| send_size(&ws, &view.borrow())
        })?;
        let mut closed = Some(closed);
        listen(&shell.ws, "close", move |_: JsValue| {
            if let Some(closed) = closed.take() {
                let _ = closed.send(());
            }
        })?;
        Ok(shell)
    }

    fn grid(&self) -> (u16, u16) {
        let view = self.view.borrow();
        (view.cols, view.rows)
    }

    /// The frame is now `width` x `height` CSS pixels: refit the grid and
    /// tell the shell.
    fn fit(&self, width: f64, height: f64) {
        if self.view.borrow_mut().fit(width, height) {
            send_size(&self.ws, &self.view.borrow());
            self.view.borrow().draw();
        }
    }

    /// Send a key press (`key` is `KeyboardEvent.key`). False when the key
    /// sends nothing.
    fn key(&self, key: &str, ctrl: bool, alt: bool) -> bool {
        let app_cursor = self.view.borrow().term.application_cursor();
        match keys::encode(key, ctrl, alt, app_cursor) {
            Some(bytes) => {
                let _ = self.ws.send_with_u8_array(&bytes);
                true
            }
            None => false,
        }
    }
}

/// The shell's size follows the grid's: a text frame `"<cols> <rows>"`.
fn send_size(ws: &WebSocket, view: &View) {
    let _ = ws.send_with_str(&format!("{} {}", view.cols, view.rows));
}

/// In full screen the browser takes Escape to leave it. Where the Keyboard
/// Lock API exists (Chromium) ask for the key, so the shell gets a press
/// and holding it leaves full screen; elsewhere Escape leaves at once.
fn lock_escape(window: &Window, on: bool) {
    let get = |target: &JsValue, name: &str| js_sys::Reflect::get(target, &name.into()).ok();
    let Some(keyboard) = get(&window.navigator(), "keyboard").filter(|k| !k.is_undefined()) else {
        return;
    };
    let args = js_sys::Array::new();
    if on {
        args.push(&js_sys::Array::of1(&"Escape".into()));
    }
    let method = get(&keyboard, if on { "lock" } else { "unlock" });
    if let Some(Ok(method)) = method.map(|m| m.dyn_into::<js_sys::Function>()) {
        let _ = method.apply(&keyboard, &args);
    }
}

#[component]
pub fn Terminal() -> Element {
    // Started by the first size the frame reports.
    let mut shell = use_signal(|| None::<Rc<Shell>>);
    let mut grid = use_signal(|| None::<(u16, u16)>);
    let mut closed = use_signal(|| false);
    let mut full = use_signal(|| false);

    // The frame's size: on first render, when the window changes, and
    // when full screen is entered or left.
    let resized = move |event: Event<ResizeData>| {
        let (Ok(size), Some(window)) = (event.get_content_box_size(), web_sys::window()) else {
            return;
        };
        let is_full = window.document().is_some_and(|d| d.fullscreen_element().is_some());
        if is_full != full() {
            full.set(is_full);
            lock_escape(&window, is_full);
        }
        let running = shell.peek().clone();
        let running = match running {
            Some(running) => {
                running.fit(size.width, size.height);
                running
            }
            None => {
                let (ended, end) = oneshot::channel();
                match Shell::open(&window, size.width, size.height, ended) {
                    Ok(started) => {
                        spawn(async move {
                            if end.await.is_ok() {
                                closed.set(true);
                            }
                        });
                        let started = Rc::new(started);
                        shell.set(Some(started.clone()));
                        started
                    }
                    Err(e) => {
                        web_sys::console::error_1(&e);
                        return;
                    }
                }
            }
        };
        grid.set(Some(running.grid()));
    };

    // Cmd and Ctrl+Shift combinations stay with the browser (copy, dev
    // tools); everything else the shell understands goes to it.
    let pressed = move |event: KeyboardEvent| {
        let mods = event.modifiers();
        if mods.meta() || (mods.ctrl() && mods.shift()) {
            return;
        }
        let sent = shell
            .peek()
            .as_ref()
            .is_some_and(|shell| shell.key(&event.key().to_string(), mods.ctrl(), mods.alt()));
        if sent {
            event.prevent_default();
        }
    };

    let toggle_full = move |_| {
        let Some(document) = web_sys::window().and_then(|w| w.document()) else {
            return;
        };
        if document.fullscreen_element().is_some() {
            document.exit_fullscreen();
        } else if let Some(frame) = document.get_element_by_id("term-frame") {
            let _ = frame.request_fullscreen();
        }
        // Keys should go to the shell straight away.
        let body = document.get_element_by_id("term-body");
        if let Some(body) = body.and_then(|b| b.dyn_into::<web_sys::HtmlElement>().ok()) {
            let _ = body.focus();
        }
    };

    rsx! {
        div { class: "term-frame", id: "term-frame",
            div { class: "term-bar",
                span { class: "term-title", "Shell" }
                span {
                    if closed() {
                        "shell closed"
                    } else if let Some((cols, rows)) = grid() {
                        "{cols}×{rows}"
                    }
                }
                button { r#type: "button", onclick: toggle_full,
                    if full() { "Exit full screen" } else { "Full screen" }
                }
            }
            div {
                class: "term-body",
                id: "term-body",
                tabindex: "0",
                onresize: resized,
                onkeydown: pressed,
                canvas { id: "term" }
            }
        }
    }
}
